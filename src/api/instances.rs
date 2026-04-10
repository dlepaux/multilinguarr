//! CRUD endpoints for arr instances.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::ApiError;
use super::state::ApiState;
use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceDto {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub language: String,
    pub url: String,
    pub api_key: String,
    pub storage_path: String,
    pub library_path: String,
    pub link_strategy: String,
    #[serde(default = "default_true")]
    pub propagate_delete: bool,
}

const fn default_true() -> bool {
    true
}

impl From<InstanceConfig> for InstanceDto {
    fn from(inst: InstanceConfig) -> Self {
        Self {
            name: inst.name,
            kind: match inst.kind {
                InstanceKind::Radarr => "radarr".to_owned(),
                InstanceKind::Sonarr => "sonarr".to_owned(),
            },
            language: inst.language,
            url: inst.url,
            api_key: "***".to_owned(),
            storage_path: inst.storage_path.to_string_lossy().into_owned(),
            library_path: inst.library_path.to_string_lossy().into_owned(),
            link_strategy: match inst.link_strategy {
                LinkStrategy::Symlink => "symlink".to_owned(),
                LinkStrategy::Hardlink => "hardlink".to_owned(),
            },
            propagate_delete: inst.propagate_delete,
        }
    }
}

impl TryFrom<InstanceDto> for InstanceConfig {
    type Error = ApiError;

    fn try_from(dto: InstanceDto) -> Result<Self, ApiError> {
        let kind = match dto.kind.as_str() {
            "radarr" => InstanceKind::Radarr,
            "sonarr" => InstanceKind::Sonarr,
            other => {
                return Err(ApiError::BadRequest(format!(
                    "invalid type '{other}', expected 'radarr' or 'sonarr'"
                )))
            }
        };
        let link_strategy = match dto.link_strategy.as_str() {
            "symlink" => LinkStrategy::Symlink,
            "hardlink" => LinkStrategy::Hardlink,
            other => {
                return Err(ApiError::BadRequest(format!(
                    "invalid link_strategy '{other}', expected 'symlink' or 'hardlink'"
                )))
            }
        };
        Ok(Self {
            name: dto.name,
            kind,
            language: dto.language,
            url: dto.url,
            api_key: dto.api_key,
            storage_path: PathBuf::from(dto.storage_path),
            library_path: PathBuf::from(dto.library_path),
            link_strategy,
            propagate_delete: dto.propagate_delete,
        })
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/instances",
    tag = "instances",
    responses(
        (status = 200, description = "All configured instances", body = Vec<InstanceDto>),
    ),
    security(("api_key" = []))
)]
pub async fn list(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let instances = state.repo.list_instances().await?;
    let body: Vec<InstanceDto> = instances.into_iter().map(InstanceDto::from).collect();
    Ok(Json(body))
}

#[utoipa::path(
    get,
    path = "/api/v1/instances/{name}",
    tag = "instances",
    params(("name" = String, Path, description = "Instance name")),
    responses(
        (status = 200, description = "Instance found", body = InstanceDto),
        (status = 404, description = "Instance not found"),
    ),
    security(("api_key" = []))
)]
pub async fn get(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let inst = state
        .repo
        .get_instance(&name)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("instance '{name}' not found")))?;
    Ok(Json(InstanceDto::from(inst)))
}

#[utoipa::path(
    post,
    path = "/api/v1/instances",
    tag = "instances",
    request_body = InstanceDto,
    responses(
        (status = 201, description = "Instance created", body = InstanceDto),
        (status = 400, description = "Validation error"),
        (status = 409, description = "Instance already exists"),
    ),
    security(("api_key" = []))
)]
pub async fn create(
    State(state): State<ApiState>,
    Json(body): Json<InstanceDto>,
) -> Result<impl IntoResponse, ApiError> {
    if body.name.is_empty() {
        return Err(ApiError::BadRequest("name is required".to_owned()));
    }
    // Validate language exists
    if state.repo.get_language(&body.language).await?.is_none() {
        return Err(ApiError::BadRequest(format!(
            "language '{}' not found — create it first via /api/v1/languages",
            body.language
        )));
    }
    let inst: InstanceConfig = body.try_into()?;
    state.repo.insert_instance(&inst).await?;
    Ok((StatusCode::CREATED, Json(InstanceDto::from(inst))))
}

#[utoipa::path(
    put,
    path = "/api/v1/instances/{name}",
    tag = "instances",
    params(("name" = String, Path, description = "Instance name")),
    request_body = InstanceDto,
    responses(
        (status = 200, description = "Instance updated", body = InstanceDto),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Instance not found"),
    ),
    security(("api_key" = []))
)]
pub async fn update(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    Json(mut body): Json<InstanceDto>,
) -> Result<impl IntoResponse, ApiError> {
    name.clone_into(&mut body.name);
    // Validate language exists
    if state.repo.get_language(&body.language).await?.is_none() {
        return Err(ApiError::BadRequest(format!(
            "language '{}' not found",
            body.language
        )));
    }
    let inst: InstanceConfig = body.try_into()?;
    let updated = state.repo.update_instance(&inst).await?;
    if !updated {
        return Err(ApiError::NotFound(format!("instance '{name}' not found")));
    }
    Ok(Json(InstanceDto::from(inst)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/instances/{name}",
    tag = "instances",
    params(("name" = String, Path, description = "Instance name")),
    responses(
        (status = 204, description = "Instance deleted"),
        (status = 404, description = "Instance not found"),
    ),
    security(("api_key" = []))
)]
pub async fn delete(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let deleted = state.repo.delete_instance(&name).await?;
    if !deleted {
        return Err(ApiError::NotFound(format!("instance '{name}' not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}
