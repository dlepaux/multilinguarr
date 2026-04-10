//! `JobStore` — the SQL-facing layer of the queue.
//!
//! All read/write operations against the `jobs` table live here. The
//! worker pool ([`super::worker`]) and the sweeper ([`super::sweeper`])
//! call into this; they never touch sqlx directly.
//!
//! Every method that mutates state uses a single SQL statement so that
//! the operation is atomic at the `SQLite` level — no read-modify-write
//! races between concurrent workers competing for the same row.

use chrono::{DateTime, Utc};
use sqlx::{Pool, Sqlite};

use super::error::QueueError;
use super::job::{Job, JobId, JobPayload, JobStatus, QueueStats};
use crate::db::Database;

/// Outcome of a failed attempt — decided by the caller based on the
/// error type and remaining attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    /// Bump attempts, schedule a retry at `retry_at`, return to `pending`.
    Retry { retry_at: DateTime<Utc> },
    /// Move to `dead_letter` immediately. No further retries.
    DeadLetter,
    /// Move to `failed` (permanent error, distinct from "exhausted retries").
    PermanentFailure,
}

/// Cloneable handle around the queue's database access. The pool inside
/// `Database` is itself an `Arc`, so cloning is cheap.
#[derive(Debug, Clone)]
pub struct JobStore {
    db: Database,
}

impl JobStore {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    fn pool(&self) -> &Pool<Sqlite> {
        self.db.pool()
    }

    // -----------------------------------------------------------------
    // Producer side
    // -----------------------------------------------------------------

    /// Insert a fresh job. Returns the assigned [`JobId`].
    ///
    /// `available_at` controls when the job becomes claimable; pass
    /// `Utc::now()` for "available immediately", or a future timestamp
    /// to schedule.
    ///
    /// # Errors
    ///
    /// - [`QueueError::Payload`] if the payload cannot be serialized to JSON.
    /// - [`QueueError::Sqlx`] on database insert failure.
    pub async fn enqueue<P: JobPayload>(
        &self,
        payload: &P,
        max_attempts: u32,
        available_at: DateTime<Utc>,
    ) -> Result<JobId, QueueError> {
        let payload_json = serde_json::to_string(payload)?;
        let now = Utc::now();
        let row = sqlx::query_as::<_, (i64,)>(
            "INSERT INTO jobs (
                kind, payload, status, attempts, max_attempts,
                next_attempt_at, created_at, updated_at
             ) VALUES (?, ?, 'pending', 0, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(P::KIND)
        .bind(payload_json)
        .bind(i64::from(max_attempts))
        .bind(available_at)
        .bind(now)
        .bind(now)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    // -----------------------------------------------------------------
    // Worker side — claim / ack / nack
    // -----------------------------------------------------------------

    /// Atomically claim the next available job, marking it as
    /// `claimed` with a lease deadline of `claim_for` from now.
    ///
    /// Returns `Ok(None)` when no job is currently eligible. Eligible
    /// means `status='pending'` and `next_attempt_at <= now`.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn claim_next(
        &self,
        worker_id: &str,
        claim_for: chrono::TimeDelta,
    ) -> Result<Option<Job>, QueueError> {
        let now = Utc::now();
        let claimed_until = now + claim_for;

        // SQLite supports `UPDATE ... RETURNING *` (since 3.35) which
        // gives us a single-shot atomic claim — no SELECT then UPDATE
        // race window between concurrent workers.
        let job = sqlx::query_as::<_, Job>(
            "UPDATE jobs
             SET status = 'claimed',
                 claimed_until = ?,
                 claimed_by = ?,
                 updated_at = ?
             WHERE id = (
                 SELECT id FROM jobs
                 WHERE status = 'pending' AND next_attempt_at <= ?
                 ORDER BY next_attempt_at, id
                 LIMIT 1
             )
             RETURNING *",
        )
        .bind(claimed_until)
        .bind(worker_id)
        .bind(now)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;

        Ok(job)
    }

    /// Mark a claimed job as completed. Idempotent on already-completed
    /// rows; no error if the row no longer exists.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn ack_success(&self, id: JobId) -> Result<(), QueueError> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE jobs
             SET status = 'completed',
                 claimed_until = NULL,
                 claimed_by = NULL,
                 completed_at = ?,
                 updated_at = ?,
                 last_error = NULL
             WHERE id = ?",
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Mark a claimed job as failed.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn ack_failure(
        &self,
        id: JobId,
        error_message: &str,
        disposition: FailureDisposition,
    ) -> Result<(), QueueError> {
        let now = Utc::now();
        match disposition {
            FailureDisposition::Retry { retry_at } => {
                sqlx::query(
                    "UPDATE jobs
                     SET status = 'pending',
                         attempts = attempts + 1,
                         next_attempt_at = ?,
                         claimed_until = NULL,
                         claimed_by = NULL,
                         last_error = ?,
                         updated_at = ?
                     WHERE id = ?",
                )
                .bind(retry_at)
                .bind(error_message)
                .bind(now)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            FailureDisposition::DeadLetter => {
                sqlx::query(
                    "UPDATE jobs
                     SET status = 'dead_letter',
                         attempts = attempts + 1,
                         claimed_until = NULL,
                         claimed_by = NULL,
                         last_error = ?,
                         updated_at = ?
                     WHERE id = ?",
                )
                .bind(error_message)
                .bind(now)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            FailureDisposition::PermanentFailure => {
                sqlx::query(
                    "UPDATE jobs
                     SET status = 'failed',
                         attempts = attempts + 1,
                         claimed_until = NULL,
                         claimed_by = NULL,
                         last_error = ?,
                         updated_at = ?
                     WHERE id = ?",
                )
                .bind(error_message)
                .bind(now)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
        }
        Ok(())
    }

    /// Extend the lease on a claimed job. Used by long-running handlers
    /// that periodically heartbeat — call from a background task while
    /// the work is in progress.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn heartbeat(
        &self,
        id: JobId,
        extend_by: chrono::TimeDelta,
    ) -> Result<(), QueueError> {
        let new_deadline = Utc::now() + extend_by;
        sqlx::query(
            "UPDATE jobs
             SET claimed_until = ?, updated_at = ?
             WHERE id = ? AND status = 'claimed'",
        )
        .bind(new_deadline)
        .bind(Utc::now())
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Sweeper / startup recovery
    // -----------------------------------------------------------------

    /// Reset every `claimed` row whose lease has expired back to
    /// `pending`. Returns the number of rows recovered. Run from a
    /// periodic background task to recover crashed workers.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn sweep_expired_claims(&self) -> Result<u64, QueueError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'pending',
                 claimed_until = NULL,
                 claimed_by = NULL,
                 updated_at = ?
             WHERE status = 'claimed' AND claimed_until <= ?",
        )
        .bind(now)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    /// One-shot reset on startup: any row still in `claimed` state must
    /// be from a previous process that crashed without a graceful
    /// shutdown. Reset them all unconditionally.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn reset_orphaned_claims(&self) -> Result<u64, QueueError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'pending',
                 claimed_until = NULL,
                 claimed_by = NULL,
                 updated_at = ?
             WHERE status = 'claimed'",
        )
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    // -----------------------------------------------------------------
    // Read API for admin endpoints / observability
    // -----------------------------------------------------------------

    /// All currently in-flight (claimed) jobs.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn list_in_flight(&self) -> Result<Vec<Job>, QueueError> {
        let jobs = sqlx::query_as::<_, Job>(
            "SELECT * FROM jobs WHERE status = 'claimed' ORDER BY claimed_until",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(jobs)
    }

    /// All dead-letter rows.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn list_dead_letters(&self) -> Result<Vec<Job>, QueueError> {
        let jobs = sqlx::query_as::<_, Job>(
            "SELECT * FROM jobs WHERE status = 'dead_letter' ORDER BY updated_at DESC",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(jobs)
    }

    /// Look up a single job by id.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn get(&self, id: JobId) -> Result<Option<Job>, QueueError> {
        let job = sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(job)
    }

    /// List jobs, optionally filtered by status. Most recent first.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn list_jobs(
        &self,
        status_filter: Option<JobStatus>,
        limit: u32,
    ) -> Result<Vec<Job>, QueueError> {
        let jobs = match status_filter {
            Some(status) => {
                sqlx::query_as::<_, Job>(
                    "SELECT * FROM jobs WHERE status = ? ORDER BY updated_at DESC LIMIT ?",
                )
                .bind(status.as_str())
                .bind(limit)
                .fetch_all(self.pool())
                .await?
            }
            None => {
                sqlx::query_as::<_, Job>("SELECT * FROM jobs ORDER BY updated_at DESC LIMIT ?")
                    .bind(limit)
                    .fetch_all(self.pool())
                    .await?
            }
        };
        Ok(jobs)
    }

    /// Snapshot row counts per status.
    ///
    /// # Errors
    ///
    /// - [`QueueError::Sqlx`] on database failure.
    /// - [`QueueError::InvalidStatus`] if a stored status string is unrecognized.
    pub async fn stats(&self) -> Result<QueueStats, QueueError> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status, count(*) FROM jobs GROUP BY status")
                .fetch_all(self.pool())
                .await?;

        let mut stats = QueueStats::default();
        for (status, count) in rows {
            match status.parse::<JobStatus>()? {
                JobStatus::Pending => stats.pending = count,
                JobStatus::Claimed => stats.claimed = count,
                JobStatus::Completed => stats.completed = count,
                JobStatus::Failed => stats.failed = count,
                JobStatus::DeadLetter => stats.dead_letter = count,
            }
        }
        Ok(stats)
    }

    /// Reset a single job to `pending` so it gets reprocessed.
    /// Works on any terminal state (completed, failed, `dead_letter`).
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn retry_job(&self, id: JobId) -> Result<bool, QueueError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE jobs SET status = 'pending', attempts = 0, \
             next_attempt_at = ?, last_error = NULL, updated_at = ? \
             WHERE id = ? AND status IN ('completed', 'failed', 'dead_letter')",
        )
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Reset ALL `completed/failed/dead_letter` jobs to `pending`.
    /// Returns the number of jobs requeued.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Sqlx`] on database failure.
    pub async fn reprocess_all(&self) -> Result<u64, QueueError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE jobs SET status = 'pending', attempts = 0, \
             next_attempt_at = ?, last_error = NULL, updated_at = ? \
             WHERE status IN ('completed', 'failed', 'dead_letter')",
        )
        .bind(&now)
        .bind(&now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }
}
