//! Setup flow — first-run detection and completion gate.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use super::error::ApiError;
use super::state::ApiState;
use crate::config::{ConfigRepo, InstanceConfig, LanguageDefinition};

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatus {
    pub complete: bool,
    pub missing: Vec<String>,
}

/// Check what's missing from the minimum viable config.
fn collect_missing_items(
    langs: &[(String, LanguageDefinition)],
    primary: &str,
    instances: &[InstanceConfig],
) -> Vec<String> {
    let mut missing = Vec::new();

    if langs.is_empty() {
        missing.push("at least one language definition required".to_owned());
    }

    if primary.is_empty() {
        missing.push("primary_language not set (PUT /api/v1/config)".to_owned());
    } else if !langs.iter().any(|(k, _)| k == primary) {
        missing.push(format!(
            "primary_language '{primary}' not found in language definitions"
        ));
    }

    if instances.is_empty() {
        missing.push("at least one instance required".to_owned());
    }

    missing
}

async fn fetch_setup_inputs(
    repo: &ConfigRepo,
) -> Result<
    (
        Vec<(String, LanguageDefinition)>,
        String,
        Vec<InstanceConfig>,
    ),
    ApiError,
> {
    let langs = repo.list_languages().await?;
    let primary = repo
        .get_config_value("primary_language")
        .await?
        .unwrap_or_default();
    let instances = repo.list_instances().await?;
    Ok((langs, primary, instances))
}

#[utoipa::path(
    get,
    path = "/api/v1/setup/status",
    tag = "setup",
    responses(
        (status = 200, description = "Current setup status", body = SetupStatus),
    ),
    security(("api_key" = []))
)]
pub async fn status(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let complete = state.repo.is_setup_complete().await?;
    let mut missing = Vec::new();

    if !complete {
        let (langs, primary, instances) = fetch_setup_inputs(&state.repo).await?;
        missing = collect_missing_items(&langs, &primary, &instances);
    }

    Ok(Json(SetupStatus { complete, missing }))
}

#[utoipa::path(
    post,
    path = "/api/v1/setup/complete",
    tag = "setup",
    responses(
        (status = 200, description = "Setup marked complete", body = SetupStatus),
        (status = 400, description = "Prerequisites not met"),
    ),
    security(("api_key" = []))
)]
pub async fn complete(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let (langs, primary, instances) = fetch_setup_inputs(&state.repo).await?;
    let missing = collect_missing_items(&langs, &primary, &instances);

    if !missing.is_empty() {
        return Err(ApiError::BadRequest(missing.join("; ")));
    }

    state.repo.mark_setup_complete().await?;

    Ok((
        StatusCode::OK,
        Json(SetupStatus {
            complete: true,
            missing: vec![],
        }),
    ))
}
