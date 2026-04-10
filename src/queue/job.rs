//! Job model: persisted shape, status state machine, retry policy.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use super::error::QueueError;

/// Stable identifier for a job — the `SQLite` rowid.
pub type JobId = i64;

/// State machine for a row in the `jobs` table.
///
/// Stored as a TEXT column for human readability in a sqlite browser.
/// Serialized via the round-trip `as_str` / `from_str` pair below
/// because sqlx's TEXT-as-enum support is limited without the `query!`
/// macros, which we are deferring (see story 06 spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Claimed,
    Completed,
    Failed,
    DeadLetter,
}

impl JobStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::DeadLetter => "dead_letter",
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobStatus {
    type Err = QueueError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "dead_letter" => Ok(Self::DeadLetter),
            other => Err(QueueError::InvalidStatus(other.to_owned())),
        }
    }
}

/// Trait every job-payload type implements so the queue can route it.
///
/// Story 08 will register concrete handlers (movie import, episode
/// upgrade, regenerate, etc.). For story 06 the only consumer is the
/// test harness, which uses [`TestPayload`].
pub trait JobPayload: Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static {
    /// Stable string identifier persisted in the `kind` column. Must
    /// be unique across all registered payload types.
    const KIND: &'static str;
}

/// Retry behaviour for a queued job.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl RetryPolicy {
    /// Defaults: 5 attempts, 1s → 60s exponential.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        }
    }

    /// `Duration` to wait before the `nth` retry (1-indexed).
    #[must_use]
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        let exp = self
            .initial_backoff
            .saturating_mul(2_u32.saturating_pow(attempt.saturating_sub(1)));
        exp.min(self.max_backoff)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::defaults()
    }
}

/// One row of the `jobs` table.
///
/// Decoded directly via `sqlx::FromRow`. The `payload` is left as a
/// raw JSON string until the consumer calls [`Job::decode_payload`] —
/// keeping it generic over `P: JobPayload` lets the queue stay
/// agnostic to the handler-specific shape.
#[derive(Debug, Clone, FromRow)]
pub struct Job {
    pub id: JobId,
    pub kind: String,
    pub payload: String,
    pub status: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub next_attempt_at: DateTime<Utc>,
    pub claimed_until: Option<DateTime<Utc>>,
    pub claimed_by: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Job {
    /// Parse `status` into the typed enum.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::InvalidStatus`] if the stored string is not a known variant.
    pub fn status_typed(&self) -> Result<JobStatus, QueueError> {
        self.status.parse()
    }

    /// Decode the JSON payload into the handler-specific shape.
    ///
    /// # Errors
    ///
    /// - [`QueueError::UnknownKind`] if `self.kind` does not match `P::KIND`.
    /// - [`QueueError::Payload`] if JSON deserialization fails.
    pub fn decode_payload<P: JobPayload>(&self) -> Result<P, QueueError> {
        if self.kind != P::KIND {
            return Err(QueueError::UnknownKind(self.kind.clone()));
        }
        serde_json::from_str(&self.payload).map_err(QueueError::from)
    }
}

/// Snapshot of queue health for observability / admin endpoints.
#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct QueueStats {
    pub pending: i64,
    pub claimed: i64,
    pub completed: i64,
    pub failed: i64,
    pub dead_letter: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        for s in [
            JobStatus::Pending,
            JobStatus::Claimed,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::DeadLetter,
        ] {
            assert_eq!(JobStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn unknown_status_rejected() {
        let err = JobStatus::from_str("nope").unwrap_err();
        assert!(matches!(err, QueueError::InvalidStatus(s) if s == "nope"));
    }

    #[test]
    fn retry_backoff_caps_at_max() {
        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(8),
        };
        assert_eq!(policy.backoff_for(1), Duration::from_secs(1));
        assert_eq!(policy.backoff_for(2), Duration::from_secs(2));
        assert_eq!(policy.backoff_for(3), Duration::from_secs(4));
        assert_eq!(policy.backoff_for(4), Duration::from_secs(8));
        // Capped — does not grow further.
        assert_eq!(policy.backoff_for(8), Duration::from_secs(8));
    }
}
