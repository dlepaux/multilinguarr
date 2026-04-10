//! General config endpoint (primary language, queue concurrency).

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::ApiError;
use super::state::ApiState;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GeneralConfigDto {
    pub primary_language: String,
    pub queue_concurrency: u32,
}

/// Returns the current general configuration (primary language, queue concurrency).
#[utoipa::path(
    get,
    path = "/api/v1/config",
    tag = "config",
    responses(
        (status = 200, description = "Current general config", body = GeneralConfigDto),
    ),
    security(("api_key" = []))
)]
pub async fn get(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let primary = state
        .repo
        .get_config_value("primary_language")
        .await?
        .unwrap_or_default();
    let concurrency: u32 = state
        .repo
        .get_config_value("queue_concurrency")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    Ok(Json(GeneralConfigDto {
        primary_language: primary,
        queue_concurrency: concurrency,
    }))
}

/// Updates the general configuration. The primary language must already exist.
#[utoipa::path(
    put,
    path = "/api/v1/config",
    tag = "config",
    request_body = GeneralConfigDto,
    responses(
        (status = 200, description = "Config updated", body = GeneralConfigDto),
        (status = 400, description = "Validation error"),
    ),
    security(("api_key" = []))
)]
pub async fn update(
    State(state): State<ApiState>,
    Json(body): Json<GeneralConfigDto>,
) -> Result<impl IntoResponse, ApiError> {
    if body.primary_language.is_empty() {
        return Err(ApiError::BadRequest(
            "primary_language must not be empty".to_owned(),
        ));
    }
    if body.queue_concurrency == 0 {
        return Err(ApiError::BadRequest(
            "queue_concurrency must be greater than 0".to_owned(),
        ));
    }

    // Validate primary language exists
    if state
        .repo
        .get_language(&body.primary_language)
        .await?
        .is_none()
    {
        return Err(ApiError::BadRequest(format!(
            "language '{}' not found — create it first",
            body.primary_language
        )));
    }

    state
        .repo
        .set_config_value("primary_language", &body.primary_language)
        .await?;
    state
        .repo
        .set_config_value("queue_concurrency", &body.queue_concurrency.to_string())
        .await?;

    // Re-read persisted state to return the actual DB values
    let primary = state
        .repo
        .get_config_value("primary_language")
        .await?
        .unwrap_or_default();
    let concurrency: u32 = state
        .repo
        .get_config_value("queue_concurrency")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    Ok(Json(GeneralConfigDto {
        primary_language: primary,
        queue_concurrency: concurrency,
    }))
}
