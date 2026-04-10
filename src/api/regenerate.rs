//! Regeneration HTTP adapter — thin wrapper around [`crate::reconcile`].

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use utoipa::IntoParams;

use super::error::ApiError;
use super::state::ApiState;
use crate::link::LinkManager;
use crate::reconcile::{self, RegenerateResult};

#[derive(Debug, Deserialize, IntoParams)]
pub struct RegenerateQuery {
    /// When true, report what would be done without creating links
    #[serde(default)]
    pub dry_run: bool,
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/regenerate",
    tag = "admin",
    params(RegenerateQuery),
    responses(
        (status = 200, description = "Regeneration result", body = RegenerateResult),
        (status = 400, description = "No instances configured or ffprobe unavailable"),
    ),
    security(("api_key" = []))
)]
/// `POST /admin/regenerate?dry_run=true|false`
pub async fn trigger(
    State(state): State<ApiState>,
    Query(query): Query<RegenerateQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let detector = state
        .detector
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("ffprobe not available on this host".to_owned()))?;

    let config = state.config.as_ref().ok_or_else(|| {
        ApiError::BadRequest("no config loaded — complete setup first".to_owned())
    })?;
    if config.instances.is_empty() {
        return Err(ApiError::BadRequest(
            "no instances configured — add at least one via POST /api/v1/instances".to_owned(),
        ));
    }
    let instances = &config.instances;

    let link_managers: Vec<(String, LinkManager)> = instances
        .iter()
        .map(|i| (i.name.clone(), LinkManager::from_instance(i)))
        .collect();

    let result =
        reconcile::regenerate_all(instances, detector, &link_managers, query.dry_run).await;
    Ok(Json(result))
}
