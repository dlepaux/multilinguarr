//! Config API — `/api/v1/*` endpoints for managing languages,
//! instances, general config, setup, and jobs.
//!
//! Schema endpoints are public (no auth). Everything else requires
//! `X-Api-Key` header.

mod auth;
mod config;
mod error;
mod instances;
mod jobs;
mod languages;
pub mod openapi;
mod regenerate;
mod schema;
mod setup;
pub mod state;

#[cfg(test)]
mod tests;

use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use utoipa::OpenApi;

use self::openapi::ApiDoc;
use self::state::ApiState;

/// Build the `/api/v1` router.
pub fn router(state: ApiState) -> Router {
    // Schema + OpenAPI endpoints — no auth, self-documenting
    let public = Router::new()
        .route("/api/v1/openapi.json", get(openapi_spec))
        .route("/api/v1/languages/schema", get(schema::languages_schema))
        .route("/api/v1/instances/schema", get(schema::instances_schema))
        .route("/api/v1/config/schema", get(schema::config_schema));

    // Authenticated endpoints
    let authed = Router::new()
        // Languages
        .route("/api/v1/languages", get(languages::list).post(languages::create))
        .route(
            "/api/v1/languages/{key}",
            get(languages::get)
                .put(languages::update)
                .delete(languages::delete),
        )
        // Instances
        .route(
            "/api/v1/instances",
            get(instances::list).post(instances::create),
        )
        .route(
            "/api/v1/instances/{name}",
            get(instances::get)
                .put(instances::update)
                .delete(instances::delete),
        )
        // General config
        .route(
            "/api/v1/config",
            get(config::get).put(config::update),
        )
        // Setup
        .route("/api/v1/setup/status", get(setup::status))
        .route("/api/v1/setup/complete", post(setup::complete))
        // Jobs
        .route("/api/v1/jobs", get(jobs::list))
        .route("/api/v1/jobs/stats", get(jobs::queue_stats))
        .route("/api/v1/jobs/reprocess", post(jobs::reprocess_all))
        .route("/api/v1/jobs/{id}", get(jobs::get))
        .route("/api/v1/jobs/{id}/retry", post(jobs::retry))
        // Admin
        .route("/api/v1/admin/regenerate", post(regenerate::trigger))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    public.merge(authed).with_state(state)
}

// Axum requires `async fn` for routing even though this handler has no await points.
#[allow(clippy::unused_async)]
async fn openapi_spec() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}
