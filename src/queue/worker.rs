//! Worker pool — claims jobs from the store, dispatches to a
//! [`JobProcessor`], and acks results.
//!
//! The pool spawns a single dispatcher loop that:
//!
//! 1. Acquires a semaphore permit (caps concurrency)
//! 2. Claims the next available job
//! 3. Spawns a worker task that runs the processor and acks the result
//! 4. Releases the permit when the worker task finishes
//!
//! Graceful shutdown is driven by a [`tokio_util::sync::CancellationToken`]:
//! the dispatcher stops accepting new work, in-flight workers run to
//! completion, and the loop exits.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::job::{Job, RetryPolicy};
use super::store::{FailureDisposition, JobStore};

/// Result of running a single job.
///
/// The processor does **not** decide retry vs dead-letter — it only
/// classifies the outcome. The worker uses the job's `attempts` and
/// `max_attempts` to decide when transient errors should be promoted
/// to dead letters.
#[derive(Debug, Clone)]
pub enum ProcessOutcome {
    Success,
    /// Retry with exponential backoff. Promoted to dead-letter once
    /// `attempts >= max_attempts`.
    Transient(String),
    /// Move straight to `failed` — never retried (e.g. 4xx from arr
    /// API, filesystem permission denied).
    Permanent(String),
}

/// What every job processor implements. Concrete implementations live
/// in the handler module (story 08).
pub trait JobProcessor: Send + Sync + 'static {
    fn process(&self, job: Job) -> impl std::future::Future<Output = ProcessOutcome> + Send;
}

/// Tunables for the worker pool.
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// Maximum number of jobs running concurrently.
    pub concurrency: usize,
    /// How long the dispatcher sleeps between empty polls.
    pub poll_interval: Duration,
    /// Lease deadline for newly claimed jobs.
    pub claim_for: TimeDelta,
    /// Retry policy for transient failures.
    pub retry_policy: RetryPolicy,
    /// Identifier persisted in the `claimed_by` column. Useful when
    /// debugging multi-worker setups; arbitrary string otherwise.
    pub worker_id: String,
}

impl WorkerPoolConfig {
    #[must_use]
    pub fn new(concurrency: usize, worker_id: impl Into<String>) -> Self {
        Self {
            concurrency,
            poll_interval: Duration::from_millis(500),
            claim_for: TimeDelta::seconds(60),
            retry_policy: RetryPolicy::defaults(),
            worker_id: worker_id.into(),
        }
    }
}

/// Spawn the dispatcher task. Returns its `JoinHandle` so the caller
/// can `await` graceful shutdown after cancelling the token.
pub fn spawn_worker_pool<P: JobProcessor>(
    store: JobStore,
    processor: Arc<P>,
    config: WorkerPoolConfig,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(dispatcher_loop(store, processor, config, cancel))
}

async fn dispatcher_loop<P: JobProcessor>(
    store: JobStore,
    processor: Arc<P>,
    config: WorkerPoolConfig,
    cancel: CancellationToken,
) {
    let semaphore = Arc::new(Semaphore::new(config.concurrency));

    loop {
        // Stop accepting work as soon as cancellation fires.
        if cancel.is_cancelled() {
            break;
        }

        // Acquire a permit (caps concurrent workers). Cancellation can
        // interrupt the wait, in which case we exit cleanly.
        let permit_future = semaphore.clone().acquire_owned();
        let permit = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            permit = permit_future => match permit {
                Ok(p) => p,
                Err(_) => break, // semaphore closed
            },
        };

        // Try to claim a job.
        let job = match store.claim_next(&config.worker_id, config.claim_for).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                drop(permit);
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(config.poll_interval) => continue,
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "job claim failed — retrying");
                drop(permit);
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(config.poll_interval) => continue,
                }
            }
        };

        let processor = processor.clone();
        let store_clone = store.clone();
        let retry_policy = config.retry_policy;

        tokio::spawn(async move {
            let _permit = permit;
            process_one(job, processor.as_ref(), &store_clone, &retry_policy).await;
        });
    }
}

async fn process_one<P: JobProcessor>(
    job: Job,
    processor: &P,
    store: &JobStore,
    retry: &RetryPolicy,
) {
    // Snapshot fields we need before moving the job into the processor.
    let id = job.id;
    let attempts = u32::try_from(job.attempts).unwrap_or(u32::MAX);
    let max_attempts = u32::try_from(job.max_attempts).unwrap_or(u32::MAX);

    let outcome = processor.process(job).await;

    let _ = match outcome {
        ProcessOutcome::Success => store.ack_success(id).await,
        ProcessOutcome::Transient(msg) => {
            // The current attempt did not increment `attempts` yet —
            // ack_failure(Retry) bumps it. So the next-attempt count
            // we compare against is `attempts + 1`.
            if attempts + 1 >= max_attempts {
                store
                    .ack_failure(id, &msg, FailureDisposition::DeadLetter)
                    .await
            } else {
                let retry_at = Utc::now() + retry.backoff_for(attempts + 1);
                let chrono_at =
                    chrono::DateTime::<Utc>::from_naive_utc_and_offset(retry_at.naive_utc(), Utc);
                store
                    .ack_failure(
                        id,
                        &msg,
                        FailureDisposition::Retry {
                            retry_at: chrono_at,
                        },
                    )
                    .await
            }
        }
        ProcessOutcome::Permanent(msg) => {
            store
                .ack_failure(id, &msg, FailureDisposition::PermanentFailure)
                .await
        }
    };
}

// ---------------------------------------------------------------------
// Sweeper — periodic recovery of expired claims
// ---------------------------------------------------------------------

/// Spawn a periodic task that resets expired claims back to `pending`.
/// Returns the `JoinHandle` so the caller can await it during graceful
/// shutdown.
#[must_use]
pub fn spawn_sweeper(
    store: JobStore,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let _ = store.sweep_expired_claims().await;
                }
            }
        }
    })
}
