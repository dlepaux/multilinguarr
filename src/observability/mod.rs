//! Prometheus metrics — `/metrics` endpoint.
//!
//! Uses the `metrics` facade crate. Counters and histograms are
//! recorded inline where events happen (webhook, handler, detection,
//! link). The exporter renders them on `GET /metrics`.

use axum::response::IntoResponse;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Metric names — centralized to avoid typos.
pub mod names {
    pub const WEBHOOKS_RECEIVED: &str = "multilinguarr_webhooks_received_total";
    pub const JOBS_PROCESSED: &str = "multilinguarr_jobs_processed_total";
    pub const FFPROBE_DURATION: &str = "multilinguarr_ffprobe_duration_seconds";
    pub const LINKS_CREATED: &str = "multilinguarr_links_created_total";
}

/// Install the prometheus recorder and return the handle for the
/// `/metrics` endpoint. Safe to call multiple times (subsequent
/// calls return a new handle against the already-installed recorder).
#[must_use]
pub fn install() -> PrometheusHandle {
    let builder = PrometheusBuilder::new();
    match builder.install_recorder() {
        Ok(handle) => handle,
        Err(_) => {
            // Recorder already installed (e.g., multiple tests in
            // the same process). Build a new handle without
            // re-installing the global recorder.
            PrometheusBuilder::new().build_recorder().handle()
        }
    }
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
