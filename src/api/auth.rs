//! X-Api-Key authentication middleware.

use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use subtle::ConstantTimeEq;

use super::state::ApiState;

/// Axum middleware that validates `X-Api-Key` header against the
/// configured API key. Uses constant-time comparison to prevent
/// timing side-channel attacks. Schema endpoints are excluded
/// upstream by mounting them outside the auth layer.
pub async fn require_api_key(
    State(state): State<ApiState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let provided = request
        .headers()
        .get("X-Api-Key")
        .and_then(|v| v.to_str().ok());

    match provided {
        Some(key) if key.as_bytes().ct_eq(state.api_key.as_bytes()).into() => {
            next.run(request).await
        }
        Some(_) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid API key" })),
        )
            .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "X-Api-Key header required" })),
        )
            .into_response(),
    }
}
