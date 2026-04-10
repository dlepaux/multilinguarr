//! Integration tests for the full application lifecycle.
//!
//! Tests boot a real server on an ephemeral port with an in-memory
//! database, configure it via the API, and verify webhook processing.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use crate::config::{
    Config, InstanceConfig, InstanceKind, LanguageDefinition, LanguagesConfig, LinkStrategy,
    QueueConfig,
};

use super::build_test;

fn test_config(storage_path: &std::path::Path, library_path: &std::path::Path) -> Config {
    let mut defs = HashMap::new();
    defs.insert(
        "fr".to_owned(),
        LanguageDefinition {
            iso_639_1: vec!["fr".to_owned()],
            iso_639_2: vec!["fre".to_owned(), "fra".to_owned()],
            radarr_id: 2,
            sonarr_id: 2,
        },
    );
    defs.insert(
        "en".to_owned(),
        LanguageDefinition {
            iso_639_1: vec!["en".to_owned()],
            iso_639_2: vec!["eng".to_owned()],
            radarr_id: 1,
            sonarr_id: 1,
        },
    );
    Config {
        port: 0,
        log_level: "warn".to_owned(),
        media_base_path: storage_path.parent().unwrap().to_path_buf(),
        database_path: ":memory:".into(),
        api_key: "test-key".to_owned(),
        queue: QueueConfig { concurrency: 1 },
        languages: LanguagesConfig {
            primary: "fr".to_owned(),
            alternates: vec!["en".to_owned()],
            definitions: defs,
        },
        instances: vec![InstanceConfig {
            name: "radarr-fr".to_owned(),
            kind: InstanceKind::Radarr,
            language: "fr".to_owned(),
            url: "http://localhost:7878".to_owned(),
            api_key: "unused".to_owned(),
            storage_path: storage_path.to_path_buf(),
            library_path: library_path.to_path_buf(),
            link_strategy: LinkStrategy::Symlink,
            propagate_delete: true,
        }],
        jellyfin: None,
    }
}

async fn http(
    method: &str,
    url: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let req = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        _ => panic!("unsupported method"),
    }
    .header("X-Api-Key", "test-key");

    let req = if let Some(b) = body {
        req.json(&b)
    } else {
        req
    };

    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(json!(null));
    (status, body)
}

#[tokio::test]
async fn health_endpoint_responds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));

    // Wait for server
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = http("GET", &format!("{base}/health"), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");

    cancel.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn metrics_endpoint_responds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("multilinguarr") || text.is_empty());

    cancel.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn api_config_flow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Schema endpoint (no auth)
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/languages/schema"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Auth required
    let (status, _) = http("GET", &format!("{base}/api/v1/languages"), None).await;
    assert_eq!(status, 200);

    // No auth → 401
    let resp = client
        .get(format!("{base}/api/v1/languages"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    cancel.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn webhook_unknown_instance_returns_404() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/webhook/nonexistent"))
        .json(&json!({"eventType": "Test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    cancel.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn webhook_enqueues_job() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let job_store = app.job_store.clone();

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a webhook
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/webhook/radarr-fr"))
        .json(&json!({
            "eventType": "Download",
            "movie": {
                "id": 1,
                "title": "Test",
                "tmdbId": 42,
                "folderPath": storage.join("Test (2024)").display().to_string()
            },
            "movieFile": {
                "id": 1,
                "path": storage.join("Test (2024)/test.mkv").display().to_string(),
                "relativePath": "test.mkv"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("job_id").is_some());

    // Verify job exists in the store
    let stats = job_store.stats().await.unwrap();
    assert!(stats.pending + stats.claimed + stats.completed + stats.failed > 0);

    cancel.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn jobs_api_shows_enqueued_webhook() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = tmp.path().join("storage");
    let library = tmp.path().join("library");
    tokio::fs::create_dir_all(&storage).await.unwrap();
    tokio::fs::create_dir_all(&library).await.unwrap();

    let app = build_test(test_config(&storage, &library)).await.unwrap();
    let addr = app.listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let cancel = app.cancel.clone();
    let server = tokio::spawn(super::run(app));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Enqueue via webhook
    let client = reqwest::Client::new();
    client
        .post(format!("{base}/webhook/radarr-fr"))
        .json(&json!({
            "eventType": "Download",
            "movie": { "id": 1, "title": "T", "tmdbId": 1, "folderPath": "/x" },
            "movieFile": { "id": 1, "path": "/x/f.mkv", "relativePath": "f.mkv" }
        }))
        .send()
        .await
        .unwrap();

    // Query via jobs API
    let (status, body) = http("GET", &format!("{base}/api/v1/jobs"), None).await;
    assert_eq!(status, 200);
    let jobs = body.as_array().unwrap();
    assert!(!jobs.is_empty());
    assert_eq!(jobs[0]["kind"], "radarr_webhook");

    cancel.cancel();
    server.await.unwrap().unwrap();
}
