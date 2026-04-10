//! Boot an in-process multilinguarr server wired against the arr
//! containers + sandbox directory tree. Mirrors what `main.rs` will
//! eventually do, but inlined here so the harness stays self-
//! contained until a proper `src/app.rs` lands.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use multilinguarr::api;
use multilinguarr::api::state::ApiState;
use multilinguarr::config::ConfigRepo;
use multilinguarr::config::{
    Config, InstanceConfig, InstanceKind, JellyfinConfig, LanguageDefinition, LanguagesConfig,
    LinkStrategy, QueueConfig,
};
use multilinguarr::db::Database;
use multilinguarr::detection::{LanguageDetector, SystemFfprobe};
use multilinguarr::handler::HandlerRegistry;
use multilinguarr::jellyfin::NoopMediaServer;
use multilinguarr::queue::{spawn_sweeper, spawn_worker_pool, JobStore, WorkerPoolConfig};
use multilinguarr::webhook::{router, AppState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::containers::ArrContainers;
use super::sandbox::Sandbox;
use super::Result;

#[derive(Debug)]
pub struct ServerHandle {
    pub base_url: String,
    pub job_store: JobStore,
    _worker: JoinHandle<()>,
    _sweeper: JoinHandle<()>,
    _server: JoinHandle<std::io::Result<()>>,
}

pub async fn spawn_server(sandbox: &Sandbox, arr: &ArrContainers) -> Result<ServerHandle> {
    let config = build_config(sandbox, arr);
    let config = Arc::new(config);

    // DB: in-memory for isolation per test session.
    let db = Database::in_memory().await?;
    let db_pool = db.pool().clone();
    let store = JobStore::new(db);

    // Detector: real SystemFfprobe — ffprobe is the single source of
    // truth for language detection.
    let ffprobe = SystemFfprobe::locate()
        .ok_or("ffprobe binary not found on PATH — required for E2E tests")?;
    let ffprobe_for_api = Some(ffprobe.clone());
    let detector =
        LanguageDetector::<SystemFfprobe>::new(Arc::new(config.languages.clone()), ffprobe);

    let registry = HandlerRegistry::build(config.clone(), detector, Arc::new(NoopMediaServer))?;

    let cancel = CancellationToken::new();
    let worker = spawn_worker_pool(
        store.clone(),
        Arc::new(registry),
        WorkerPoolConfig::new(config.queue.concurrency, "e2e-worker"),
        cancel.clone(),
    );
    let sweeper = spawn_sweeper(store.clone(), Duration::from_secs(30), cancel.clone());

    // Bind to an ephemeral port on 127.0.0.1 so the test can learn
    // the actual port after `TcpListener::bind`.
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let local_addr = listener.local_addr()?;
    let base_url = format!("http://{local_addr}");

    let webhook_state = AppState::new(config.clone(), store.clone());
    let repo = ConfigRepo::new(db_pool);
    let detector_for_api =
        ffprobe_for_api.map(|fp| LanguageDetector::new(Arc::new(config.languages.clone()), fp));
    let management_state = ApiState::new(
        repo,
        store.clone(),
        config.api_key.clone(),
        Some(config.clone()),
        detector_for_api,
    );
    let app = router(webhook_state).merge(api::router(management_state));

    let server_cancel = cancel.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                server_cancel.cancelled().await;
            })
            .await
    });

    // Quick readiness poll: hit /health until we get 200.
    wait_until_healthy(&base_url).await?;

    Ok(ServerHandle {
        base_url,
        job_store: store,
        _worker: worker,
        _sweeper: sweeper,
        _server: server,
    })
}

async fn wait_until_healthy(base_url: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let url = format!("{base_url}/health");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("multilinguarr /health did not respond in time".into());
        }
        if let Ok(resp) = http.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn build_config(sandbox: &Sandbox, arr: &ArrContainers) -> Config {
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
        log_level: "info".to_owned(),
        media_base_path: sandbox.media.clone(),
        database_path: ":memory:".into(),
        api_key: "e2e-root".to_owned(),
        queue: QueueConfig { concurrency: 1 },
        languages: LanguagesConfig {
            primary: "fr".to_owned(),
            alternates: vec!["en".to_owned()],
            definitions: defs,
        },
        instances: vec![
            InstanceConfig {
                name: "radarr-fr".to_owned(),
                kind: InstanceKind::Radarr,
                language: "fr".to_owned(),
                url: arr.radarr_fr.base_url.clone(),
                api_key: arr.radarr_fr.api_key.clone(),
                storage_path: sandbox.storage.radarr_fr.clone(),
                library_path: sandbox.library.movies_fr.clone(),
                link_strategy: LinkStrategy::Symlink,
                propagate_delete: true,
            },
            InstanceConfig {
                name: "radarr-en".to_owned(),
                kind: InstanceKind::Radarr,
                language: "en".to_owned(),
                url: arr.radarr_en.base_url.clone(),
                api_key: arr.radarr_en.api_key.clone(),
                storage_path: sandbox.storage.radarr_en.clone(),
                library_path: sandbox.library.movies_en.clone(),
                link_strategy: LinkStrategy::Symlink,
                propagate_delete: true,
            },
            InstanceConfig {
                name: "sonarr-fr".to_owned(),
                kind: InstanceKind::Sonarr,
                language: "fr".to_owned(),
                url: arr.sonarr_fr.base_url.clone(),
                api_key: arr.sonarr_fr.api_key.clone(),
                storage_path: sandbox.storage.sonarr_fr.clone(),
                library_path: sandbox.library.tv_fr.clone(),
                link_strategy: LinkStrategy::Symlink,
                propagate_delete: true,
            },
            InstanceConfig {
                name: "sonarr-en".to_owned(),
                kind: InstanceKind::Sonarr,
                language: "en".to_owned(),
                url: arr.sonarr_en.base_url.clone(),
                api_key: arr.sonarr_en.api_key.clone(),
                storage_path: sandbox.storage.sonarr_en.clone(),
                library_path: sandbox.library.tv_en.clone(),
                link_strategy: LinkStrategy::Symlink,
                propagate_delete: true,
            },
        ],
        jellyfin: None::<JellyfinConfig>,
    }
}
