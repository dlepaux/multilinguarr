//! Prometheus metrics — `/metrics` endpoint.
//!
//! Uses the `metrics` facade crate. Counters and histograms are
//! recorded inline where events happen (webhook, handler, detection,
//! link). The exporter renders them on `GET /metrics`.
//!
//! See [plan/research/v1-metrics-design.md](../../../plan/research/v1-metrics-design.md)
//! for the naming/label contract this module enforces.

use std::time::Duration;

use axum::response::IntoResponse;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use sqlx::{Pool, Sqlite};
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[cfg(test)]
mod tests;

/// Metric names — centralized to avoid typos and to give stories
/// 01/02/04/05 a single source of truth for the names they emit.
///
/// Some constants are declared but not yet emitted; the wiring lands
/// in their respective stories. They live here so the naming contract
/// is reviewable in one place.
pub mod names {
    // -------- existing (v1.0.0) --------
    /// Counter — webhooks accepted by the HTTP layer.
    pub const WEBHOOKS_RECEIVED: &str = "multilinguarr_webhooks_received_total";
    /// Counter — queue jobs that reached a terminal state.
    pub const JOBS_PROCESSED: &str = "multilinguarr_jobs_processed_total";
    /// Histogram — wall-time per ffprobe call.
    pub const FFPROBE_DURATION: &str = "multilinguarr_ffprobe_duration_seconds";
    /// Counter — physical link operations performed.
    pub const LINKS_CREATED: &str = "multilinguarr_links_created_total";

    // -------- new in story 03 (registered + emitted here) --------
    /// Histogram — wall-time per processed job.
    /// Emit sites land in stories 01/02 (worker outcome wiring).
    pub const JOB_DURATION: &str = "multilinguarr_job_duration_seconds";
    /// Gauge — current dead-letter queue depth.
    /// Polled by [`super::spawn_dlq_tick`] every 30s.
    pub const DEAD_LETTER_JOBS: &str = "multilinguarr_dead_letter_jobs";

    // -------- declared for stories 01/02/04/05 (registered, not emitted yet) --------
    /// Counter — cross-instance add outcomes (story 01).
    pub const CROSS_INSTANCE_ADD: &str = "multilinguarr_cross_instance_add_total";
    /// Counter — webhook events whose type is unhandled (story 02).
    pub const WEBHOOK_UNKNOWN_EVENT: &str = "multilinguarr_webhook_unknown_event_total";
    /// Counter — fallbacks to instance default language (story 04).
    pub const LANGUAGE_TAG_FALLBACK: &str = "multilinguarr_language_tag_fallback_total";
    /// Counter — files skipped because detected language ≠ expected (story 05).
    pub const WRONG_LANGUAGE_SKIP: &str = "multilinguarr_wrong_language_skip_total";
}

/// DLQ gauge polling interval.
///
/// Operator-action territory, not real-time — 30 s strikes the right
/// balance between freshness on dashboards and noise on a quiet system.
pub const DLQ_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Install the prometheus recorder and return the handle for the
/// `/metrics` endpoint.
///
/// Registers histogram bucket overrides (so `histogram!` calls render
/// as real Prometheus histograms instead of degenerate summaries) and
/// emits `# HELP` lines for every metric.
///
/// Safe to call multiple times: subsequent calls return a fresh handle
/// against the already-installed global recorder. The bucket overrides
/// only take effect on the first install — after that the global
/// recorder owns the configuration.
#[must_use]
pub fn install() -> PrometheusHandle {
    let handle = match build_with_overrides().install_recorder() {
        Ok(h) => h,
        Err(_) => {
            // Recorder already installed (e.g., multiple tests in
            // the same process). Build a new handle without
            // re-installing the global recorder. Bucket overrides
            // from the first install remain in force globally.
            build_with_overrides().build_recorder().handle()
        }
    };
    describe_all();
    handle
}

/// Apply bucket overrides shared by `install_recorder` (production) and
/// `build_recorder` (test re-install fallback) so both code paths get
/// real histograms instead of summaries.
fn build_with_overrides() -> PrometheusBuilder {
    let job_buckets = exponential_buckets(0.01, 2.0, 14);
    let ffprobe_buckets = exponential_buckets(0.05, 2.0, 14);

    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Full(names::JOB_DURATION.to_owned()), &job_buckets)
        .expect("non-empty job-duration bucket list")
        .set_buckets_for_metric(
            Matcher::Full(names::FFPROBE_DURATION.to_owned()),
            &ffprobe_buckets,
        )
        .expect("non-empty ffprobe-duration bucket list")
}

/// Emit `# HELP` text for every metric this binary may surface.
/// Operators reading `/metrics` raw should not have to grep the source.
fn describe_all() {
    // Counters — currently emitted
    metrics::describe_counter!(
        names::WEBHOOKS_RECEIVED,
        "Webhooks accepted by the HTTP layer, before queueing."
    );
    metrics::describe_counter!(
        names::JOBS_PROCESSED,
        "Queue jobs that reached a terminal state (success/transient/permanent)."
    );
    metrics::describe_counter!(
        names::LINKS_CREATED,
        "Physical link operations performed (excludes idempotent no-ops)."
    );

    // Histograms — currently emitted
    metrics::describe_histogram!(
        names::FFPROBE_DURATION,
        metrics::Unit::Seconds,
        "Wall-time per ffprobe invocation, labelled by outcome."
    );
    metrics::describe_histogram!(
        names::JOB_DURATION,
        metrics::Unit::Seconds,
        "Wall-time per processed queue job, labelled by kind and outcome."
    );

    // Gauges
    metrics::describe_gauge!(
        names::DEAD_LETTER_JOBS,
        "Current count of jobs in the dead_letter terminal state."
    );

    // Counters — declared for stories 01/02/04/05
    metrics::describe_counter!(
        names::CROSS_INSTANCE_ADD,
        "Cross-instance add outcomes (created/already_existed/error)."
    );
    metrics::describe_counter!(
        names::WEBHOOK_UNKNOWN_EVENT,
        "Webhook events whose eventType is not handled by the binary."
    );
    metrics::describe_counter!(
        names::LANGUAGE_TAG_FALLBACK,
        "Imports that fell back to the instance default language."
    );
    metrics::describe_counter!(
        names::WRONG_LANGUAGE_SKIP,
        "Imports skipped because the detected language did not include the expected one."
    );
}

/// Compute exponential buckets analogous to `prometheus::exponential_buckets`.
///
/// Returns `count` bucket upper bounds: `start, start*factor, start*factor^2, …`.
/// Panics in debug if `start <= 0`, `factor <= 1`, or `count == 0` — these are
/// programmer errors, not runtime conditions.
fn exponential_buckets(start: f64, factor: f64, count: usize) -> Vec<f64> {
    debug_assert!(start > 0.0, "exponential_buckets: start must be > 0");
    debug_assert!(factor > 1.0, "exponential_buckets: factor must be > 1");
    debug_assert!(count > 0, "exponential_buckets: count must be > 0");
    let mut out = Vec::with_capacity(count);
    let mut value = start;
    for _ in 0..count {
        out.push(value);
        value *= factor;
    }
    out
}

/// Read the current dead-letter queue depth.
///
/// Extracted as a free function (rather than a `JobStore` method) so
/// the metrics layer can poll it without depending on the queue module
/// types directly, and so unit tests can drive it against an arbitrary
/// in-memory `Pool<Sqlite>`.
///
/// # Errors
///
/// Returns the underlying `sqlx::Error` if the query fails.
pub async fn dlq_count(pool: &Pool<Sqlite>) -> Result<u64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM jobs WHERE status = 'dead_letter'")
        .fetch_one(pool)
        .await?;
    // COUNT(*) is non-negative by definition; clamp defensively to keep
    // the public type unsigned.
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Spawn a background task that polls the DLQ depth every
/// [`DLQ_POLL_INTERVAL`] and updates the
/// [`names::DEAD_LETTER_JOBS`] gauge.
///
/// The task exits cleanly when `cancel` is triggered.
#[must_use]
pub fn spawn_dlq_tick(
    pool: Pool<Sqlite>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(DLQ_POLL_INTERVAL);
        // First tick fires immediately so the gauge is populated at
        // startup instead of after one full interval of `0`.
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    debug!("DLQ tick cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    match dlq_count(&pool).await {
                        Ok(n) => {
                            // metrics gauges accept f64; jobs/u64 → f64 is
                            // lossless for any realistic queue depth.
                            #[allow(clippy::cast_precision_loss)]
                            metrics::gauge!(names::DEAD_LETTER_JOBS).set(n as f64);
                        }
                        Err(err) => {
                            warn!(error = %err, "DLQ count query failed");
                        }
                    }
                }
            }
        }
    })
}

/// Prometheus metrics in text exposition format.
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "monitoring",
    responses(
        (status = 200, description = "Prometheus metrics in text format", content_type = "text/plain"),
    ),
)]
#[allow(clippy::unused_async)]
pub async fn metrics_handler(
    axum::extract::State(handle): axum::extract::State<PrometheusHandle>,
) -> impl IntoResponse {
    // Axum requires async handlers — the render itself is sync.
    handle.render()
}
