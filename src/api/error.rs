//! Shared error type for API handlers.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// API error — converts to an HTTP response with status + JSON body.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Conflict(String),
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<crate::queue::QueueError> for ApiError {
    fn from(err: crate::queue::QueueError) -> Self {
        tracing::error!(error = %err, "queue error in API handler");
        Self::Internal("internal error".to_owned())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        // Foreign key constraint → language still referenced by an instance
        if let sqlx::Error::Database(ref db_err) = err {
            let msg = db_err.message();
            if msg.contains("FOREIGN KEY") {
                return Self::Conflict("cannot delete: still referenced by an instance".to_owned());
            }
            if msg.contains("UNIQUE") || msg.contains("PRIMARY KEY") {
                return Self::Conflict("already exists".to_owned());
            }
        }
        tracing::error!(error = %err, "database error in API handler");
        Self::Internal("internal error".to_owned())
    }
}
