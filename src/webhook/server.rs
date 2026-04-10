//! Axum HTTP server: webhook ingress + health endpoint.
//!
//! All instance routes share a single `AppState`. The dispatch route
//! `POST /webhook/:instance` looks up the instance by name in the
//! configured set; unknown names return 404. The body is parsed as
//! `serde_json::Value` first (so we can log the raw payload on parse
//! failures), then converted into the kind-specific tagged enum.
//!
//! Successful events are enqueued via `JobStore` and acked with 200
//! before any handler logic runs — the worker pool (story 06) takes
//! it from there. Story 08 plugs in the actual handlers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::{serve, Router};
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use super::error::WebhookError;
use super::events::{RadarrEvent, RadarrWebhookJob, SonarrEvent, SonarrWebhookJob};
use crate::config::{Config, InstanceConfig, InstanceKind};
use crate::queue::JobStore;

/// Per-process Axum state. Cloneable — every internal field is either
/// `Arc` or already cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub job_store: JobStore,
    /// Pre-built name → `InstanceConfig` index so the request handler
    /// is O(1) instead of iterating Config.instances on every call.
    pub instances: Arc<HashMap<String, InstanceConfig>>,
}

impl AppState {
    #[must_use]
    pub fn new(config: Arc<Config>, job_store: JobStore) -> Self {
        let instances = config
            .instances
            .iter()
            .map(|i| (i.name.clone(), i.clone()))
            .collect();
        Self {
            config,
            job_store,
            instances: Arc::new(instances),
        }
    }
}

/// Build the Axum `Router` with every endpoint mounted.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/webhook/{instance}", post(webhook))
        .layer(RequestBodyLimitLayer::new(1024 * 1024)) // 1 MB
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind to `addr`, run the server, and return when `cancel` fires.
///
/// # Errors
///
/// Returns [`std::io::Error`] if the TCP listener cannot bind to `addr`
/// or the server encounters a fatal I/O error.
pub async fn serve_http(
    state: AppState,
    addr: SocketAddr,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "webhook server listening");
    serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await
}

// ---------------------------------------------------------------------
// /health
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HealthBody {
    status: &'static str,
    version: &'static str,
    timestamp: chrono::DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue: Option<crate::queue::QueueStats>,
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "monitoring",
    responses(
        (status = 200, description = "Service is healthy"),
        (status = 503, description = "Service is unhealthy"),
    ),
)]
pub async fn health(State(app): State<AppState>) -> Response {
    match app.job_store.stats().await {
        Ok(queue_stats) => (
            StatusCode::OK,
            Json(HealthBody {
                status: "ok",
                version: env!("CARGO_PKG_VERSION"),
                timestamp: Utc::now(),
                queue: Some(queue_stats),
            }),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, "health check failed — database unreachable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthBody {
                    status: "unhealthy",
                    version: env!("CARGO_PKG_VERSION"),
                    timestamp: Utc::now(),
                    queue: None,
                }),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------
// /webhook/:instance
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct EnqueueResponse {
    job_id: i64,
    instance: String,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
struct AckResponse {
    instance: String,
    /// `enqueued` for handled events, `ignored` for unknown event types.
    status: &'static str,
}

async fn webhook(
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, WebhookError> {
    // Resolve the instance from the configured set.
    let Some(instance) = state.instances.get(&instance_name).cloned() else {
        return Err(WebhookError::UnknownInstance(instance_name));
    };

    // Parse as Value first so a malformed body fails fast with a 400
    // and the original bytes can be logged for debugging.
    let value: Value = serde_json::from_slice(&body).map_err(|source| {
        tracing::warn!(
            instance = %instance_name,
            body_len = body.len(),
            error = %source,
            "webhook body is not valid json"
        );
        WebhookError::MalformedJson(source)
    })?;

    metrics::counter!(crate::observability::names::WEBHOOKS_RECEIVED,
        "instance" => instance_name.clone(),
        "kind" => match instance.kind {
            InstanceKind::Radarr => "radarr",
            InstanceKind::Sonarr => "sonarr",
        }
    )
    .increment(1);

    match instance.kind {
        InstanceKind::Radarr => handle_radarr(&state, instance.name, value).await,
        InstanceKind::Sonarr => handle_sonarr(&state, instance.name, value).await,
    }
}

async fn handle_radarr(
    state: &AppState,
    instance: String,
    value: Value,
) -> Result<Response, WebhookError> {
    let event: RadarrEvent = serde_json::from_value(value).map_err(WebhookError::Decode)?;

    if matches!(event, RadarrEvent::Unknown | RadarrEvent::Test(_)) {
        tracing::info!(%instance, ?event, "ignoring radarr webhook");
        return Ok((
            StatusCode::OK,
            Json(AckResponse {
                instance,
                status: "ignored",
            }),
        )
            .into_response());
    }

    let payload = RadarrWebhookJob {
        instance: instance.clone(),
        event,
    };
    let id = state
        .job_store
        .enqueue(&payload, 5, Utc::now())
        .await
        .map_err(WebhookError::Enqueue)?;
    tracing::info!(%instance, job_id = id, "enqueued radarr webhook");
    Ok((
        StatusCode::OK,
        Json(EnqueueResponse {
            job_id: id,
            instance,
            kind: <RadarrWebhookJob as crate::queue::JobPayload>::KIND,
        }),
    )
        .into_response())
}

async fn handle_sonarr(
    state: &AppState,
    instance: String,
    value: Value,
) -> Result<Response, WebhookError> {
    let event: SonarrEvent = serde_json::from_value(value).map_err(WebhookError::Decode)?;

    if matches!(event, SonarrEvent::Unknown | SonarrEvent::Test(_)) {
        tracing::info!(%instance, ?event, "ignoring sonarr webhook");
        return Ok((
            StatusCode::OK,
            Json(AckResponse {
                instance,
                status: "ignored",
            }),
        )
            .into_response());
    }

    let payload = SonarrWebhookJob {
        instance: instance.clone(),
        event,
    };
    let id = state
        .job_store
        .enqueue(&payload, 5, Utc::now())
        .await
        .map_err(WebhookError::Enqueue)?;
    tracing::info!(%instance, job_id = id, "enqueued sonarr webhook");
    Ok((
        StatusCode::OK,
        Json(EnqueueResponse {
            job_id: id,
            instance,
            kind: <SonarrWebhookJob as crate::queue::JobPayload>::KIND,
        }),
    )
        .into_response())
}
