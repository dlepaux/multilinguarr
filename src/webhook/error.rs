//! Webhook layer errors and `IntoResponse` mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

use crate::queue::QueueError;

#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("unknown instance `{0}` — no instance with this name is configured")]
    UnknownInstance(String),

    #[error("malformed json body: {0}")]
    MalformedJson(#[source] serde_json::Error),

    #[error("failed to enqueue job: {0}")]
    Enqueue(#[source] QueueError),
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    detail: String,
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::UnknownInstance(_) => (StatusCode::NOT_FOUND, "unknown_instance"),
            Self::MalformedJson(_) => (StatusCode::BAD_REQUEST, "malformed_json"),
            Self::Enqueue(_) => (StatusCode::INTERNAL_SERVER_ERROR, "enqueue_failed"),
        };
        let body = ErrorBody {
            error: code.to_owned(),
            detail: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}
