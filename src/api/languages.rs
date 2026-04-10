//! CRUD endpoints for language definitions.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::ApiError;
use super::state::ApiState;
use crate::config::LanguageDefinition;

#[derive(Debug, Serialize, ToSchema)]
pub struct LanguageResponse {
    pub key: String,
    #[serde(flatten)]
    pub definition: LanguageDefinitionDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LanguageDefinitionDto {
    pub iso_639_1: Vec<String>,
    pub iso_639_2: Vec<String>,
    pub radarr_id: u32,
    pub sonarr_id: u32,
}

impl From<LanguageDefinition> for LanguageDefinitionDto {
    fn from(def: LanguageDefinition) -> Self {
        Self {
            iso_639_1: def.iso_639_1,
            iso_639_2: def.iso_639_2,
            radarr_id: def.radarr_id,
            sonarr_id: def.sonarr_id,
        }
    }
}

impl From<LanguageDefinitionDto> for LanguageDefinition {
    fn from(dto: LanguageDefinitionDto) -> Self {
        Self {
            iso_639_1: dto.iso_639_1,
            iso_639_2: dto.iso_639_2,
            radarr_id: dto.radarr_id,
            sonarr_id: dto.sonarr_id,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/languages",
    tag = "languages",
    responses(
        (status = 200, description = "All language definitions", body = Vec<LanguageResponse>),
    ),
    security(("api_key" = []))
)]
pub async fn list(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let langs = state.repo.list_languages().await?;
    let body: Vec<LanguageResponse> = langs
        .into_iter()
        .map(|(key, def)| LanguageResponse {
            key,
            definition: def.into(),
        })
        .collect();
    Ok(Json(body))
}

#[utoipa::path(
    get,
    path = "/api/v1/languages/{key}",
    tag = "languages",
    params(("key" = String, Path, description = "Language key")),
    responses(
        (status = 200, description = "Language found", body = LanguageResponse),
        (status = 404, description = "Language not found"),
    ),
    security(("api_key" = []))
)]
pub async fn get(
    State(state): State<ApiState>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let def = state
        .repo
        .get_language(&key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("language '{key}' not found")))?;
    Ok(Json(LanguageResponse {
        key,
        definition: def.into(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateLanguageRequest {
    pub key: String,
    #[serde(flatten)]
    pub definition: LanguageDefinitionDto,
}

#[utoipa::path(
    post,
    path = "/api/v1/languages",
    tag = "languages",
    request_body = CreateLanguageRequest,
    responses(
        (status = 201, description = "Language created", body = LanguageResponse),
        (status = 400, description = "Validation error"),
        (status = 409, description = "Language already exists"),
    ),
    security(("api_key" = []))
)]
pub async fn create(
    State(state): State<ApiState>,
    Json(body): Json<CreateLanguageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.key.is_empty() {
        return Err(ApiError::BadRequest("key is required".to_owned()));
    }
    let def: LanguageDefinition = body.definition.clone().into();
    state.repo.insert_language(&body.key, &def).await?;
    Ok((
        StatusCode::CREATED,
        Json(LanguageResponse {
            key: body.key,
            definition: body.definition,
        }),
    ))
}

#[utoipa::path(
    put,
    path = "/api/v1/languages/{key}",
    tag = "languages",
    params(("key" = String, Path, description = "Language key")),
    request_body = LanguageDefinitionDto,
    responses(
        (status = 200, description = "Language updated", body = LanguageResponse),
        (status = 404, description = "Language not found"),
    ),
    security(("api_key" = []))
)]
pub async fn update(
    State(state): State<ApiState>,
    Path(key): Path<String>,
    Json(body): Json<LanguageDefinitionDto>,
) -> Result<impl IntoResponse, ApiError> {
    let def: LanguageDefinition = body.clone().into();
    let updated = state.repo.update_language(&key, &def).await?;
    if !updated {
        return Err(ApiError::NotFound(format!("language '{key}' not found")));
    }
    Ok(Json(LanguageResponse {
        key,
        definition: body,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/languages/{key}",
    tag = "languages",
    params(("key" = String, Path, description = "Language key")),
    responses(
        (status = 204, description = "Language deleted"),
        (status = 404, description = "Language not found"),
        (status = 409, description = "Language still referenced by an instance"),
    ),
    security(("api_key" = []))
)]
pub async fn delete(
    State(state): State<ApiState>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let deleted = state.repo.delete_language(&key).await?;
    if !deleted {
        return Err(ApiError::NotFound(format!("language '{key}' not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}
