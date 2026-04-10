//! End-to-end queue tests against an in-memory `SQLite` database.
//!
//! These cover the full state machine: enqueue, claim, ack success,
//! ack transient retry, ack permanent failure, dead-letter at max
//! attempts, expired-claim sweep, orphaned-claim startup reset, and
//! the worker pool dispatch loop with graceful shutdown.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    spawn_sweeper, spawn_worker_pool, FailureDisposition, Job, JobPayload, JobProcessor, JobStatus,
    JobStore, ProcessOutcome, QueueError, RetryPolicy, WorkerPoolConfig,
};
use crate::db::Database;

// ---------- payloads ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TestPayload {
    value: u32,
}

impl JobPayload for TestPayload {
    const KIND: &'static str = "test";
}

// ---------- helpers ----------

async fn make_store() -> JobStore {
    let db = Database::in_memory().await.expect("in-memory db");
    JobStore::new(db)
}

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

// ---------- raw store: enqueue / claim / ack ----------

#[tokio::test]
async fn enqueue_then_claim_returns_the_job() {
    let store = make_store().await;
    let id = store
        .enqueue(&TestPayload { value: 42 }, 5, now())
        .await
        .unwrap();

    let claimed = store
        .claim_next("worker-1", TimeDelta::seconds(60))
        .await
        .unwrap()
        .expect("claim should succeed");
    assert_eq!(claimed.id, id);
    assert_eq!(claimed.status_typed().unwrap(), JobStatus::Claimed);
    let payload: TestPayload = claimed.decode_payload().unwrap();
    assert_eq!(payload.value, 42);
}

#[tokio::test]
async fn claim_next_returns_none_when_empty() {
    let store = make_store().await;
    let claimed = store
        .claim_next("worker-1", TimeDelta::seconds(60))
        .await
        .unwrap();
    assert!(claimed.is_none());
}

#[tokio::test]
async fn claim_skips_jobs_scheduled_in_the_future() {
    let store = make_store().await;
    let future = now() + TimeDelta::hours(1);
    store
        .enqueue(&TestPayload { value: 1 }, 5, future)
        .await
        .unwrap();
    assert!(store
        .claim_next("w", TimeDelta::seconds(60))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn two_workers_never_double_claim_the_same_job() {
    let store = make_store().await;
    store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();

    let store_a = store.clone();
    let store_b = store.clone();

    let (a, b) = tokio::join!(
        async move { store_a.claim_next("a", TimeDelta::seconds(60)).await },
        async move { store_b.claim_next("b", TimeDelta::seconds(60)).await }
    );
    let a = a.unwrap();
    let b = b.unwrap();
    // Exactly one wins.
    assert!(
        (a.is_some() && b.is_none()) || (a.is_none() && b.is_some()),
        "exactly one worker must claim"
    );
}

#[tokio::test]
async fn ack_success_marks_completed() {
    let store = make_store().await;
    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    store.claim_next("w", TimeDelta::seconds(60)).await.unwrap();
    store.ack_success(id).await.unwrap();

    let job = store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status_typed().unwrap(), JobStatus::Completed);
    assert!(job.completed_at.is_some());
    assert!(job.claimed_until.is_none());
    assert!(job.last_error.is_none());
}

#[tokio::test]
async fn ack_failure_retry_returns_job_to_pending_with_error() {
    let store = make_store().await;
    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    store.claim_next("w", TimeDelta::seconds(60)).await.unwrap();

    store
        .ack_failure(
            id,
            "transient",
            FailureDisposition::Retry {
                retry_at: now() + TimeDelta::seconds(1),
            },
        )
        .await
        .unwrap();

    let job = store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status_typed().unwrap(), JobStatus::Pending);
    assert_eq!(job.attempts, 1);
    assert_eq!(job.last_error.as_deref(), Some("transient"));
}

#[tokio::test]
async fn ack_failure_dead_letter_terminates() {
    let store = make_store().await;
    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    store.claim_next("w", TimeDelta::seconds(60)).await.unwrap();
    store
        .ack_failure(id, "exhausted", FailureDisposition::DeadLetter)
        .await
        .unwrap();

    let job = store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status_typed().unwrap(), JobStatus::DeadLetter);
    assert_eq!(job.last_error.as_deref(), Some("exhausted"));
}

#[tokio::test]
async fn ack_failure_permanent_marks_failed() {
    let store = make_store().await;
    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    store.claim_next("w", TimeDelta::seconds(60)).await.unwrap();
    store
        .ack_failure(id, "401", FailureDisposition::PermanentFailure)
        .await
        .unwrap();

    let job = store.get(id).await.unwrap().unwrap();
    assert_eq!(job.status_typed().unwrap(), JobStatus::Failed);
}

// ---------- sweeper / startup recovery ----------

#[tokio::test]
async fn sweep_returns_expired_claims_to_pending() {
    let store = make_store().await;
    store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    // Claim with a lease deadline already in the past.
    let _ = store
        .claim_next("w", TimeDelta::seconds(-10))
        .await
        .unwrap()
        .unwrap();

    let recovered = store.sweep_expired_claims().await.unwrap();
    assert_eq!(recovered, 1);

    // Now claimable again.
    let claimed = store
        .claim_next("w2", TimeDelta::seconds(60))
        .await
        .unwrap();
    assert!(claimed.is_some());
}

#[tokio::test]
async fn sweep_leaves_unexpired_claims_alone() {
    let store = make_store().await;
    store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    let _ = store
        .claim_next("w", TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    let recovered = store.sweep_expired_claims().await.unwrap();
    assert_eq!(recovered, 0);
}

#[tokio::test]
async fn reset_orphaned_claims_unconditionally_returns_claimed_to_pending() {
    let store = make_store().await;
    store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    // Long lease — sweep would not touch this, but startup reset should.
    let _ = store
        .claim_next("crashed-worker", TimeDelta::hours(1))
        .await
        .unwrap()
        .unwrap();

    let n = store.reset_orphaned_claims().await.unwrap();
    assert_eq!(n, 1);

    let claimed = store.claim_next("w", TimeDelta::seconds(60)).await.unwrap();
    assert!(claimed.is_some());
    assert_eq!(claimed.unwrap().attempts, 0, "attempts not bumped by reset");
}

// ---------- stats ----------

#[tokio::test]
async fn stats_counts_by_status() {
    let store = make_store().await;
    for v in 0..3_u32 {
        store
            .enqueue(&TestPayload { value: v }, 5, now())
            .await
            .unwrap();
    }
    // Move one to completed.
    let job = store
        .claim_next("w", TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    store.ack_success(job.id).await.unwrap();

    let stats = store.stats().await.unwrap();
    assert_eq!(stats.pending, 2);
    assert_eq!(stats.completed, 1);
    assert_eq!(stats.claimed, 0);
}

// ---------- worker pool integration ----------

#[derive(Debug)]
struct CountingProcessor {
    success: AtomicUsize,
    transient: AtomicUsize,
    permanent: AtomicUsize,
    behaviour: Behaviour,
}

#[derive(Debug)]
enum Behaviour {
    AlwaysSuccess,
    AlwaysTransient,
    AlwaysPermanent,
    /// Fail transiently `fail_first` times, then succeed.
    FailThenSucceed {
        fail_first: usize,
    },
}

impl CountingProcessor {
    fn new(behaviour: Behaviour) -> Self {
        Self {
            success: AtomicUsize::new(0),
            transient: AtomicUsize::new(0),
            permanent: AtomicUsize::new(0),
            behaviour,
        }
    }
}

impl JobProcessor for CountingProcessor {
    async fn process(&self, _job: Job) -> ProcessOutcome {
        match &self.behaviour {
            Behaviour::AlwaysSuccess => {
                self.success.fetch_add(1, Ordering::SeqCst);
                ProcessOutcome::Success
            }
            Behaviour::AlwaysTransient => {
                self.transient.fetch_add(1, Ordering::SeqCst);
                ProcessOutcome::Transient("temporary".to_owned())
            }
            Behaviour::AlwaysPermanent => {
                self.permanent.fetch_add(1, Ordering::SeqCst);
                ProcessOutcome::Permanent("permanent".to_owned())
            }
            Behaviour::FailThenSucceed { fail_first } => {
                let prior = self.transient.load(Ordering::SeqCst);
                if prior < *fail_first {
                    self.transient.fetch_add(1, Ordering::SeqCst);
                    ProcessOutcome::Transient("retry me".to_owned())
                } else {
                    self.success.fetch_add(1, Ordering::SeqCst);
                    ProcessOutcome::Success
                }
            }
        }
    }
}

async fn wait_for<F, Fut>(mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if probe().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for condition");
}

#[tokio::test]
async fn worker_pool_processes_a_successful_job_and_acks() {
    let store = make_store().await;
    let processor = Arc::new(CountingProcessor::new(Behaviour::AlwaysSuccess));
    let cancel = CancellationToken::new();
    let mut config = WorkerPoolConfig::new(2, "test-pool");
    config.poll_interval = Duration::from_millis(10);

    let handle = spawn_worker_pool(store.clone(), processor.clone(), config, cancel.clone());

    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();

    wait_for(|| {
        let store = store.clone();
        async move {
            store
                .get(id)
                .await
                .unwrap()
                .is_some_and(|j| j.status_typed().unwrap() == JobStatus::Completed)
        }
    })
    .await;
    assert_eq!(processor.success.load(Ordering::SeqCst), 1);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn worker_pool_retries_transient_then_succeeds() {
    let store = make_store().await;
    let processor = Arc::new(CountingProcessor::new(Behaviour::FailThenSucceed {
        fail_first: 2,
    }));
    let cancel = CancellationToken::new();
    let mut config = WorkerPoolConfig::new(1, "test-pool");
    // Tight retry policy so the test runs in milliseconds.
    config.poll_interval = Duration::from_millis(5);
    config.retry_policy = RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
    };

    let handle = spawn_worker_pool(store.clone(), processor.clone(), config, cancel.clone());

    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();

    wait_for(|| {
        let store = store.clone();
        async move {
            store
                .get(id)
                .await
                .unwrap()
                .is_some_and(|j| j.status_typed().unwrap() == JobStatus::Completed)
        }
    })
    .await;
    assert_eq!(processor.transient.load(Ordering::SeqCst), 2);
    assert_eq!(processor.success.load(Ordering::SeqCst), 1);

    let final_job = store.get(id).await.unwrap().unwrap();
    assert_eq!(final_job.attempts, 2);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn worker_pool_dead_letters_after_max_attempts() {
    let store = make_store().await;
    let processor = Arc::new(CountingProcessor::new(Behaviour::AlwaysTransient));
    let cancel = CancellationToken::new();
    let mut config = WorkerPoolConfig::new(1, "test-pool");
    config.poll_interval = Duration::from_millis(5);
    config.retry_policy = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
    };

    let handle = spawn_worker_pool(store.clone(), processor.clone(), config, cancel.clone());

    let id = store
        .enqueue(&TestPayload { value: 1 }, 3, now())
        .await
        .unwrap();

    wait_for(|| {
        let store = store.clone();
        async move {
            store
                .get(id)
                .await
                .unwrap()
                .is_some_and(|j| j.status_typed().unwrap() == JobStatus::DeadLetter)
        }
    })
    .await;
    let final_job = store.get(id).await.unwrap().unwrap();
    assert_eq!(final_job.attempts, 3);
    assert!(final_job.last_error.is_some());

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn worker_pool_marks_permanent_failure_immediately() {
    let store = make_store().await;
    let processor = Arc::new(CountingProcessor::new(Behaviour::AlwaysPermanent));
    let cancel = CancellationToken::new();
    let mut config = WorkerPoolConfig::new(1, "test-pool");
    config.poll_interval = Duration::from_millis(5);

    let handle = spawn_worker_pool(store.clone(), processor.clone(), config, cancel.clone());

    let id = store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();

    wait_for(|| {
        let store = store.clone();
        async move {
            store
                .get(id)
                .await
                .unwrap()
                .is_some_and(|j| j.status_typed().unwrap() == JobStatus::Failed)
        }
    })
    .await;
    assert_eq!(processor.permanent.load(Ordering::SeqCst), 1);
    let job = store.get(id).await.unwrap().unwrap();
    // Permanent failure does NOT retry — single attempt only.
    assert_eq!(job.attempts, 1);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn worker_pool_graceful_shutdown_stops_dispatching() {
    let store = make_store().await;
    let processor = Arc::new(CountingProcessor::new(Behaviour::AlwaysSuccess));
    let cancel = CancellationToken::new();
    let mut config = WorkerPoolConfig::new(1, "test-pool");
    config.poll_interval = Duration::from_millis(5);

    let handle = spawn_worker_pool(store.clone(), processor.clone(), config, cancel.clone());

    cancel.cancel();
    handle.await.unwrap();

    // Enqueue AFTER cancellation. Worker should be gone — job stays pending.
    store
        .enqueue(&TestPayload { value: 1 }, 5, now())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let stats = store.stats().await.unwrap();
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.completed, 0);
}

// ---------- sweeper task ----------

#[tokio::test]
async fn sweeper_task_recovers_expired_claim() -> Result<(), QueueError> {
    let store = make_store().await;
    store.enqueue(&TestPayload { value: 1 }, 5, now()).await?;
    let _ = store
        .claim_next("orphan", TimeDelta::seconds(-10))
        .await?
        .unwrap();

    let cancel = CancellationToken::new();
    let handle = spawn_sweeper(store.clone(), Duration::from_millis(10), cancel.clone());

    wait_for(|| {
        let store = store.clone();
        async move {
            store
                .claim_next("recoverer", TimeDelta::seconds(60))
                .await
                .unwrap()
                .is_some()
        }
    })
    .await;

    cancel.cancel();
    handle.await.unwrap();
    Ok(())
}
