//! Job visibility + retry endpoints.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};

use super::error::ApiError;
use super::state::ApiState;
use crate::queue::{Job, JobStatus, QueueStats};

#[derive(Debug, Serialize, ToSchema)]
pub struct JobDto {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub payload: serde_json::Value,
    pub attempts: i64,
    pub max_attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl From<Job> for JobDto {
    fn from(job: Job) -> Self {
        let payload = serde_json::from_str(&job.payload).unwrap_or(json!(null));
        Self {
            id: job.id,
            kind: job.kind,
            status: job.status,
            payload,
            attempts: job.attempts,
            max_attempts: job.max_attempts,
            last_error: job.last_error,
            created_at: job.created_at.to_rfc3339(),
            updated_at: job.updated_at.to_rfc3339(),
            completed_at: job.completed_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListQuery {
    /// Filter by job status (pending, claimed, completed, failed, `dead_letter`)
    pub status: Option<String>,
    /// Maximum number of jobs to return (default: 100)
    pub limit: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/v1/jobs",
    tag = "jobs",
    params(ListQuery),
    responses(
        (status = 200, description = "List of jobs", body = Vec<JobDto>),
        (status = 400, description = "Invalid status filter"),
    ),
    security(("api_key" = []))
)]
pub async fn list(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let status_filter = query
        .status
        .as_deref()
        .map(|s| {
            s.parse::<JobStatus>()
                .map_err(|_| ApiError::BadRequest(format!("invalid status '{s}'")))
        })
        .transpose()?;
    let limit = query.limit.unwrap_or(100);
    let jobs = state.job_store.list_jobs(status_filter, limit).await?;
    let body: Vec<JobDto> = jobs.into_iter().map(JobDto::from).collect();
    Ok(Json(body))
}

#[utoipa::path(
    get,
    path = "/api/v1/jobs/{id}",
    tag = "jobs",
    params(("id" = i64, Path, description = "Job ID")),
    responses(
        (status = 200, description = "Job found", body = JobDto),
        (status = 404, description = "Job not found"),
    ),
    security(("api_key" = []))
)]
pub async fn get(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let job = state
        .job_store
        .get(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("job {id} not found")))?;
    Ok(Json(JobDto::from(job)))
}

#[utoipa::path(
    get,
    path = "/api/v1/jobs/stats",
    tag = "jobs",
    responses(
        (status = 200, description = "Queue statistics", body = QueueStats),
    ),
    security(("api_key" = []))
)]
pub async fn queue_stats(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let counts = state.job_store.stats().await?;
    Ok(Json(counts))
}

#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/retry",
    tag = "jobs",
    params(("id" = i64, Path, description = "Job ID")),
    responses(
        (status = 200, description = "Job retried"),
        (status = 404, description = "Job not found or not in terminal state"),
    ),
    security(("api_key" = []))
)]
pub async fn retry(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let retried = state.job_store.retry_job(id).await?;
    if !retried {
        return Err(ApiError::NotFound(format!(
            "job {id} not found or not in a terminal state"
        )));
    }
    Ok((StatusCode::OK, Json(json!({ "retried": true, "id": id }))))
}

#[utoipa::path(
    post,
    path = "/api/v1/jobs/reprocess",
    tag = "admin",
    responses(
        (status = 200, description = "All terminal jobs reset to pending"),
    ),
    security(("api_key" = []))
)]
pub async fn reprocess_all(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let count = state.job_store.reprocess_all().await?;
    Ok(Json(json!({ "reprocessed": count })))
}
