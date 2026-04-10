//! Config API integration tests.
//!
//! Each test spins up an in-memory `SQLite` DB + Axum test server.
//! No network, no Docker — pure request/response assertions.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::ConfigRepo;
use crate::db::Database;
use crate::queue::JobStore;

use super::router;
use super::state::ApiState;

const API_KEY: &str = "test-key";

async fn setup() -> (axum::Router, ConfigRepo) {
    let db = Database::in_memory().await.unwrap();
    let repo = ConfigRepo::new(db.pool().clone());
    let job_store = JobStore::new(db);
    let state = ApiState::new(repo.clone(), job_store, API_KEY.to_owned(), None, None);
    let app = router(state);
    (app, repo)
}

fn authed_request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Api-Key", API_KEY)
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap()
}

fn authed_json(method: Method, uri: &str, body: impl Serialize) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Api-Key", API_KEY)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn unauthed_request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn response_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// =====================================================================
// Auth
// =====================================================================

#[tokio::test]
async fn missing_api_key_returns_401() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(unauthed_request(Method::GET, "/api/v1/languages"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_api_key_returns_401() {
    let (app, _) = setup().await;
    let req = Request::builder()
        .uri("/api/v1/languages")
        .header("X-Api-Key", "wrong")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn schema_endpoints_need_no_auth() {
    let (app, _) = setup().await;
    let resp = app
        .clone()
        .oneshot(unauthed_request(Method::GET, "/api/v1/languages/schema"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert!(!body.get("fields").unwrap().as_array().unwrap().is_empty());
}

// =====================================================================
// Languages CRUD
// =====================================================================

#[tokio::test]
async fn language_crud_lifecycle() {
    let (app, _) = setup().await;

    // List — empty
    let resp = app
        .clone()
        .oneshot(authed_request(Method::GET, "/api/v1/languages"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0);

    // Create
    let resp = app
        .clone()
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/languages",
            json!({
                "key": "fr",
                "iso_639_1": ["fr"],
                "iso_639_2": ["fre", "fra"],
                "radarr_id": 2,
                "sonarr_id": 2
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_json(resp).await;
    assert_eq!(body["key"], "fr");

    // Get
    let resp = app
        .clone()
        .oneshot(authed_request(Method::GET, "/api/v1/languages/fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["key"], "fr");
    assert_eq!(body["radarr_id"], 2);

    // Update
    let resp = app
        .clone()
        .oneshot(authed_json(
            Method::PUT,
            "/api/v1/languages/fr",
            json!({
                "iso_639_1": ["fr"],
                "iso_639_2": ["fre", "fra"],
                "radarr_id": 3,
                "sonarr_id": 3
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["radarr_id"], 3);

    // List — one entry
    let resp = app
        .clone()
        .oneshot(authed_request(Method::GET, "/api/v1/languages"))
        .await
        .unwrap();
    let body = response_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Delete
    let resp = app
        .clone()
        .oneshot(authed_request(Method::DELETE, "/api/v1/languages/fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Get after delete — 404
    let resp = app
        .clone()
        .oneshot(authed_request(Method::GET, "/api/v1/languages/fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn duplicate_language_returns_conflict() {
    let (app, _) = setup().await;
    let body = json!({
        "key": "en",
        "iso_639_1": ["en"],
        "iso_639_2": ["eng"],
        "radarr_id": 1,
        "sonarr_id": 1
    });
    app.clone()
        .oneshot(authed_json(Method::POST, "/api/v1/languages", &body))
        .await
        .unwrap();
    let resp = app
        .oneshot(authed_json(Method::POST, "/api/v1/languages", &body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// =====================================================================
// Instances CRUD
// =====================================================================

async fn seed_language(app: &axum::Router) {
    app.clone()
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/languages",
            json!({
                "key": "fr",
                "iso_639_1": ["fr"],
                "iso_639_2": ["fre"],
                "radarr_id": 2,
                "sonarr_id": 2
            }),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn instance_crud_lifecycle() {
    let (app, _) = setup().await;
    seed_language(&app).await;

    // Create
    let resp = app
        .clone()
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/instances",
            json!({
                "name": "radarr-fr",
                "type": "radarr",
                "language": "fr",
                "url": "http://radarr-fr:7878",
                "api_key": "secret",
                "storage_path": "/srv/media/storage/radarr-fr",
                "library_path": "/srv/media/library/movies/fr",
                "link_strategy": "symlink"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Get
    let resp = app
        .clone()
        .oneshot(authed_request(Method::GET, "/api/v1/instances/radarr-fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["name"], "radarr-fr");
    assert_eq!(body["type"], "radarr");
    assert_eq!(body["link_strategy"], "symlink");
    assert_eq!(body["propagate_delete"], true);

    // Update
    let resp = app
        .clone()
        .oneshot(authed_json(
            Method::PUT,
            "/api/v1/instances/radarr-fr",
            json!({
                "name": "radarr-fr",
                "type": "radarr",
                "language": "fr",
                "url": "http://radarr-fr:7878",
                "api_key": "new-secret",
                "storage_path": "/srv/media/storage/radarr-fr",
                "library_path": "/srv/media/library/movies/fr",
                "link_strategy": "hardlink",
                "propagate_delete": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["link_strategy"], "hardlink");
    assert_eq!(body["propagate_delete"], false);

    // Delete
    let resp = app
        .clone()
        .oneshot(authed_request(
            Method::DELETE,
            "/api/v1/instances/radarr-fr",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn instance_with_unknown_language_returns_400() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/instances",
            json!({
                "name": "radarr-es",
                "type": "radarr",
                "language": "es",
                "url": "http://radarr-es:7878",
                "api_key": "k",
                "storage_path": "/a",
                "library_path": "/b",
                "link_strategy": "symlink"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn instance_with_invalid_type_returns_400() {
    let (app, _) = setup().await;
    seed_language(&app).await;
    let resp = app
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/instances",
            json!({
                "name": "x",
                "type": "lidarr",
                "language": "fr",
                "url": "http://x:1234",
                "api_key": "k",
                "storage_path": "/a",
                "library_path": "/b",
                "link_strategy": "symlink"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_language_referenced_by_instance_returns_conflict() {
    let (app, _) = setup().await;
    seed_language(&app).await;
    // Create instance referencing "fr"
    app.clone()
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/instances",
            json!({
                "name": "radarr-fr",
                "type": "radarr",
                "language": "fr",
                "url": "http://x:1234",
                "api_key": "k",
                "storage_path": "/a",
                "library_path": "/b",
                "link_strategy": "symlink"
            }),
        ))
        .await
        .unwrap();
    // Try to delete the language — should fail
    let resp = app
        .oneshot(authed_request(Method::DELETE, "/api/v1/languages/fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// =====================================================================
// General config
// =====================================================================

#[tokio::test]
async fn config_get_returns_defaults() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/config"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["primary_language"], "");
    assert_eq!(body["queue_concurrency"], 2);
}

#[tokio::test]
async fn config_update_validates_primary_language() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_json(
            Method::PUT,
            "/api/v1/config",
            json!({ "primary_language": "nonexistent", "queue_concurrency": 2 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn config_update_with_valid_language() {
    let (app, _) = setup().await;
    seed_language(&app).await;
    let resp = app
        .oneshot(authed_json(
            Method::PUT,
            "/api/v1/config",
            json!({ "primary_language": "fr", "queue_concurrency": 4 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["primary_language"], "fr");
    assert_eq!(body["queue_concurrency"], 4);
}

// =====================================================================
// Setup
// =====================================================================

#[tokio::test]
async fn setup_status_shows_missing_items() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/setup/status"))
        .await
        .unwrap();
    let body = response_json(resp).await;
    assert_eq!(body["complete"], false);
    assert!(!body["missing"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn setup_complete_fails_without_config() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::POST, "/api/v1/setup/complete"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn setup_complete_succeeds_with_full_config() {
    let (app, _) = setup().await;
    seed_language(&app).await;

    // Set primary language
    app.clone()
        .oneshot(authed_json(
            Method::PUT,
            "/api/v1/config",
            json!({ "primary_language": "fr", "queue_concurrency": 2 }),
        ))
        .await
        .unwrap();

    // Add instance
    app.clone()
        .oneshot(authed_json(
            Method::POST,
            "/api/v1/instances",
            json!({
                "name": "radarr-fr",
                "type": "radarr",
                "language": "fr",
                "url": "http://radarr-fr:7878",
                "api_key": "k",
                "storage_path": "/a",
                "library_path": "/b",
                "link_strategy": "symlink"
            }),
        ))
        .await
        .unwrap();

    // Complete setup
    let resp = app
        .clone()
        .oneshot(authed_request(Method::POST, "/api/v1/setup/complete"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["complete"], true);

    // Status confirms
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/setup/status"))
        .await
        .unwrap();
    let body = response_json(resp).await;
    assert_eq!(body["complete"], true);
    assert_eq!(body["missing"].as_array().unwrap().len(), 0);
}

// =====================================================================
// Schema
// =====================================================================

#[tokio::test]
async fn all_schema_endpoints_return_fields() {
    let (app, _) = setup().await;
    for path in [
        "/api/v1/languages/schema",
        "/api/v1/instances/schema",
        "/api/v1/config/schema",
    ] {
        let resp = app
            .clone()
            .oneshot(unauthed_request(Method::GET, path))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "failed for {path}");
        let body = response_json(resp).await;
        assert!(
            !body.get("fields").unwrap().as_array().unwrap().is_empty(),
            "no fields for {path}"
        );
    }
}

// =====================================================================
// Jobs
// =====================================================================

#[tokio::test]
async fn jobs_list_empty() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/jobs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn jobs_stats_returns_counts() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/jobs/stats"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["pending"], 0);
    assert_eq!(body["completed"], 0);
}

#[tokio::test]
async fn jobs_get_not_found() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::GET, "/api/v1/jobs/999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn jobs_retry_not_found() {
    let (app, _) = setup().await;
    let resp = app
        .oneshot(authed_request(Method::POST, "/api/v1/jobs/999/retry"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
