//! Shared E2E harness for the multilinguarr integration tests.
//!
//! All per-session state (sandbox tempdir, running arr containers,
//! in-process multilinguarr server) lives behind a single
//! `OnceCell<Arc<Harness>>` so the test binary pays the ~40-60 second
//! container startup cost exactly once per `cargo test --features e2e`
//! invocation, regardless of how many scenarios run.
//!
//! Per-test isolation is achieved by using unique `tmdb_id` /
//! `tvdb_id` / `folder_name` values per scenario rather than by
//! resetting state between tests.

pub mod arr;
pub mod assertions;
pub mod containers;
pub mod fixtures;
pub mod harness;
pub mod sandbox;
pub mod server;

use std::time::{Duration, Instant};

use multilinguarr::queue::JobStatus;
use serde_json::Value;

use self::harness::Harness;

/// Shared `Result` type for every helper in the harness.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Narrow a JSON-decoded `u64` to `u32`. Every arr ID (movie,
/// series, tmdb, tvdb, episode file) is small enough to fit, so
/// overflow here is a test-bug and we panic loudly.
pub fn as_u32(value: u64, what: &str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("{what} ({value}) does not fit in u32"))
}

// Real arr containers on GH runners can be slow (API calls, container
// scheduling). 120s gives enough headroom for cross-instance propagation.
const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const JOB_POLL: Duration = Duration::from_millis(250);

/// POST a webhook payload to the in-process multilinguarr server.
///
/// Returns the parsed JSON response, which for accepted events
/// includes a `job_id` the caller can pass to `wait_for_job`.
pub async fn post_webhook(harness: &Harness, instance_name: &str, body: Value) -> Result<Value> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let url = format!("{}/webhook/{instance_name}", harness.server.base_url);
    let resp = http.post(url).json(&body).send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("webhook POST → {status}: {text}").into());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Block until the queue row for `job_id` reaches a terminal state.
/// Returns `Ok(true)` on `Completed`, errors on `Failed`/`DeadLetter`
/// or timeout.
pub async fn wait_for_job(harness: &Harness, job_id: i64) -> Result<bool> {
    let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
    let mut last_status = String::new();
    loop {
        if Instant::now() > deadline {
            // Dump all jobs for debugging on timeout.
            let stats = match harness.server.job_store.stats().await {
                Ok(s) => format!(
                    "pending={} claimed={} completed={} failed={} dead={}",
                    s.pending, s.claimed, s.completed, s.failed, s.dead_letter,
                ),
                Err(e) => format!("stats error: {e}"),
            };
            return Err(format!(
                "job {job_id} timed out after {}s — last status: {last_status}, queue: {stats}",
                JOB_WAIT_TIMEOUT.as_secs(),
            )
            .into());
        }
        let job = harness
            .server
            .job_store
            .get(job_id)
            .await
            .map_err(|e| format!("job lookup failed: {e}"))?;
        if let Some(job) = job {
            last_status.clone_from(&job.status);
            match job
                .status_typed()
                .map_err(|e| format!("status parse: {e}"))?
            {
                JobStatus::Completed => return Ok(true),
                JobStatus::Failed | JobStatus::DeadLetter => {
                    return Err(format!(
                        "job {job_id} ended in {:?} with error: {:?}",
                        job.status_typed().ok(),
                        job.last_error
                    )
                    .into());
                }
                JobStatus::Pending | JobStatus::Claimed => {}
            }
        }
        tokio::time::sleep(JOB_POLL).await;
    }
}
