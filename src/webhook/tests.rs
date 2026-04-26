//! HTTP-layer tests for the webhook server.
//!
//! Exercises the router via `tower::ServiceExt::oneshot` so no real
//! TCP socket is bound. Each test builds an in-memory `JobStore`,
//! constructs the router, and asserts on the response + the
//! resulting jobs row.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;

use super::events::{RadarrEvent, RadarrWebhookJob, SonarrEvent, SonarrWebhookJob};
use super::server::{router, AppState};
use crate::config::{
    Config, InstanceConfig, InstanceKind, JellyfinConfig, LanguageDefinition, LanguagesConfig,
    LinkStrategy, QueueConfig,
};
use crate::db::Database;
use crate::queue::{JobPayload, JobStatus, JobStore};

// ---------- fixtures ----------

fn instance(name: &str, kind: InstanceKind) -> InstanceConfig {
    InstanceConfig {
        name: name.to_owned(),
        kind,
        language: "fr".to_owned(),
        url: "http://localhost".to_owned(),
        api_key: "k".to_owned(),
        storage_path: "/tmp/storage".into(),
        library_path: "/tmp/library".into(),
        link_strategy: LinkStrategy::Symlink,
        propagate_delete: true,
    }
}

fn test_config() -> Arc<Config> {
    let mut defs = HashMap::new();
    defs.insert(
        "fr".to_owned(),
        LanguageDefinition {
            iso_639_1: vec!["fr".to_owned()],
            iso_639_2: vec!["fra".to_owned()],
            radarr_id: 2,
            sonarr_id: 2,
        },
    );
    Arc::new(Config {
        port: 3100,
        log_level: "info".to_owned(),
        media_base_path: "/tmp".into(),
        database_path: ":memory:".into(),
        api_key: "root-key".to_owned(),
        queue: QueueConfig { concurrency: 2 },
        languages: LanguagesConfig {
            primary: "fr".to_owned(),
            alternates: vec![],
            definitions: defs,
        },
        instances: vec![
            instance("radarr-fr", InstanceKind::Radarr),
            instance("sonarr-fr", InstanceKind::Sonarr),
        ],
        jellyfin: None::<JellyfinConfig>,
    })
}

async fn fresh_app() -> (Router, JobStore) {
    let db = Database::in_memory().await.expect("in-memory db");
    let store = JobStore::new(db);
    let state = AppState::new(test_config(), store.clone());
    (router(state), store)
}

async fn read_body(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}))
}

// ---------- /health ----------

#[tokio::test]
async fn health_returns_ok_with_status() {
    let (app, _) = fresh_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_body(response.into_body()).await;
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
    assert!(body["timestamp"].is_string());
}

// ---------- routing ----------

#[tokio::test]
async fn unknown_instance_returns_404() {
    let (app, _) = fresh_app().await;
    let body = json!({ "eventType": "Test" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/does-not-exist")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = read_body(response.into_body()).await;
    assert_eq!(body["error"], "unknown_instance");
}

#[tokio::test]
async fn malformed_json_returns_400() {
    let (app, store) = fresh_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let stats = store.stats().await.unwrap();
    assert_eq!(stats.pending, 0);
}

// ---------- radarr ----------

#[tokio::test]
async fn radarr_test_event_is_acked_without_enqueue() {
    let (app, store) = fresh_app().await;
    let body = json!({ "eventType": "Test" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json_body = read_body(response.into_body()).await;
    assert_eq!(json_body["status"], "ignored");
    assert_eq!(store.stats().await.unwrap().pending, 0);
}

#[tokio::test]
async fn radarr_unknown_event_type_is_acked_without_enqueue() {
    let (app, store) = fresh_app().await;
    let body = json!({ "eventType": "ApplicationUpdate", "newVersion": "5.0" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json_body = read_body(response.into_body()).await;
    assert_eq!(json_body["status"], "ignored");
    assert_eq!(store.stats().await.unwrap().pending, 0);
}

#[tokio::test]
async fn radarr_download_event_is_enqueued() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "Download",
        "isUpgrade": false,
        "movie": {
            "id": 1,
            "title": "The Matrix",
            "year": 1999,
            "tmdbId": 603
        },
        "movieFile": {
            "id": 11,
            "relativePath": "The Matrix.mkv"
        }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let resp_body = read_body(response.into_body()).await;
    assert_eq!(resp_body["instance"], "radarr-fr");
    assert_eq!(resp_body["kind"], "radarr_webhook");

    let stats = store.stats().await.unwrap();
    assert_eq!(stats.pending, 1);

    // Decode the persisted payload and check the variant.
    let claimed = store
        .claim_next("test", chrono::TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.kind, RadarrWebhookJob::KIND);
    let payload: RadarrWebhookJob = claimed.decode_payload().unwrap();
    assert_eq!(payload.instance, "radarr-fr");
    let RadarrEvent::Download(d) = payload.event else {
        panic!("expected Download variant");
    };
    assert_eq!(d.movie.unwrap().tmdb_id, 603);
}

#[tokio::test]
async fn radarr_movie_delete_event_is_enqueued() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "MovieDelete",
        "deletedFiles": true,
        "movie": {
            "id": 1,
            "title": "Inception",
            "year": 2010,
            "tmdbId": 27205
        }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);
}

// ---------- sonarr ----------

#[tokio::test]
async fn sonarr_download_event_is_enqueued() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "Download",
        "isUpgrade": false,
        "series": { "id": 7, "title": "Breaking Bad", "tvdbId": 81189 },
        "episodes": [{ "id": 1, "episodeNumber": 1, "seasonNumber": 1 }],
        "episodeFile": { "id": 100, "relativePath": "Season 01/S01E01.mkv" }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);

    let claimed = store
        .claim_next("t", chrono::TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.kind, SonarrWebhookJob::KIND);
    let payload: SonarrWebhookJob = claimed.decode_payload().unwrap();
    let SonarrEvent::Download(d) = payload.event else {
        panic!("expected Download variant");
    };
    assert_eq!(d.series.unwrap().tvdb_id, 81189);
}

#[tokio::test]
async fn sonarr_test_event_is_acked_without_enqueue() {
    let (app, store) = fresh_app().await;
    let body = json!({ "eventType": "Test" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 0);
}

// ---------- end-to-end via JobStore ----------

#[tokio::test]
async fn enqueued_radarr_job_is_observable_via_stats_and_get() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "Download",
        "isUpgrade": true,
        "movie": { "id": 1, "title": "Dune", "year": 2021, "tmdbId": 438_631 },
        "movieFile": { "id": 99 }
    });
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/webhook/radarr-fr")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap();
    let job = store.get(1).await.unwrap().unwrap();
    assert_eq!(job.status_typed().unwrap(), JobStatus::Pending);
    assert_eq!(job.attempts, 0);
}

// ---------- legacy event type aliases ----------

#[tokio::test]
async fn radarr_movie_file_imported_alias_enqueues_as_download() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "MovieFileImported",
        "isUpgrade": false,
        "movie": { "id": 1, "title": "Test", "year": 2024, "tmdbId": 42 },
        "movieFile": { "id": 11, "relativePath": "test.mkv" }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);

    let claimed = store
        .claim_next("test", chrono::TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    let payload: RadarrWebhookJob = claimed.decode_payload().unwrap();
    assert!(matches!(payload.event, RadarrEvent::Download(_)));
}

#[tokio::test]
async fn radarr_movie_file_upgrade_alias_enqueues_as_download() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "MovieFileUpgrade",
        "isUpgrade": true,
        "movie": { "id": 1, "title": "Test", "year": 2024, "tmdbId": 42 },
        "movieFile": { "id": 11, "relativePath": "test.mkv" }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);
}

#[tokio::test]
async fn sonarr_episode_file_imported_alias_enqueues_as_download() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "EpisodeFileImported",
        "isUpgrade": false,
        "series": { "id": 1, "title": "Show", "tvdbId": 81189 },
        "episodes": [{ "id": 1, "episodeNumber": 1, "seasonNumber": 1 }],
        "episodeFile": { "id": 100, "relativePath": "Season 01/S01E01.mkv" }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);

    let claimed = store
        .claim_next("test", chrono::TimeDelta::seconds(60))
        .await
        .unwrap()
        .unwrap();
    let payload: SonarrWebhookJob = claimed.decode_payload().unwrap();
    assert!(matches!(payload.event, SonarrEvent::Download(_)));
}

#[tokio::test]
async fn sonarr_episode_file_upgrade_alias_enqueues_as_download() {
    let (app, store) = fresh_app().await;
    let body = json!({
        "eventType": "EpisodeFileUpgrade",
        "isUpgrade": true,
        "series": { "id": 1, "title": "Show", "tvdbId": 81189 },
        "episodes": [{ "id": 1, "episodeNumber": 1, "seasonNumber": 1 }],
        "episodeFile": { "id": 100, "relativePath": "Season 01/S01E01.mkv" }
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 1);
}

// ---------- unknown event surfacing ----------

#[tokio::test]
#[tracing_test::traced_test]
async fn sonarr_unknown_event_logs_raw_event_type() {
    let (app, store) = fresh_app().await;
    let body = json!({ "eventType": "TotallyMadeUpEvent", "noise": 1 });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 0);
    assert!(
        logs_contain("event_type=TotallyMadeUpEvent"),
        "expected raw eventType in structured log"
    );
    assert!(
        logs_contain("ignoring sonarr webhook"),
        "expected the ignored-webhook log line"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn radarr_unknown_event_logs_raw_event_type() {
    let (app, store) = fresh_app().await;
    let body = json!({ "eventType": "BrandNewArrEvent" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/radarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 0);
    assert!(
        logs_contain("event_type=BrandNewArrEvent"),
        "expected raw eventType in structured log"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn sonarr_missing_event_type_warns_with_truncated_body() {
    let (app, store) = fresh_app().await;
    // 300+ byte payload with no `eventType` field. Long enough to trip
    // the 256-byte truncation cap.
    let filler = "x".repeat(300);
    let body = json!({ "filler": filler });
    let body_str = body.to_string();
    assert!(body_str.len() > 256);
    let total_bytes = body_str.len();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();

    // 200 + WARN, not 400 — Sonarr retries on 4xx and would amplify a
    // single bad payload into a tight loop.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.stats().await.unwrap().pending, 0);
    assert!(
        logs_contain("undecodable sonarr webhook body"),
        "expected the undecodable-body WARN line"
    );
    assert!(
        logs_contain("event_type=<missing>"),
        "expected <missing> sentinel for absent eventType"
    );
    assert!(
        logs_contain(&format!("[truncated, {total_bytes} bytes total]")),
        "expected truncation marker with byte count"
    );
}

#[tokio::test]
async fn unknown_event_counter_uses_bounded_label() {
    use metrics_exporter_prometheus::PrometheusBuilder;

    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let (app, _store) = fresh_app().await;
    let body = json!({ "eventType": "WeirdNewEvent" });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/sonarr-fr")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let render = handle.render();
    assert!(
        render.contains("multilinguarr_webhook_unknown_event_total"),
        "counter should be present in render:\n{render}"
    );
    assert!(
        render.contains("event_type=\"unknown\""),
        "event_type label must collapse to the bounded sentinel:\n{render}"
    );
    assert!(
        render.contains("source=\"sonarr\""),
        "source label should be set:\n{render}"
    );
    // The raw event name MUST NOT appear as a label — that would let a
    // misconfigured arr or hostile payload explode Prometheus
    // cardinality.
    assert!(
        !render.contains("WeirdNewEvent"),
        "raw event name leaked into the metric — cardinality bound violated:\n{render}"
    );
}
