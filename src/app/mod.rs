//! Application startup — wires all modules into a running server.
//!
//! `build` creates the combined Axum router. `run` opens the database,
//! loads config, starts the worker pool, and serves HTTP until shutdown.

#[cfg(test)]
mod tests;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::api;
use crate::api::state::ApiState;
use crate::config::{Bootstrap, Config, ConfigRepo};
use crate::db::Database;
use crate::detection::{LanguageDetector, SystemFfprobe};
use crate::handler::HandlerRegistry;
use crate::jellyfin::NoopMediaServer;
use crate::observability;
use crate::queue::{spawn_sweeper, spawn_worker_pool, JobStore, WorkerPoolConfig};
use crate::webhook;

/// Everything needed to run the server. Returned by [`build`] so
/// tests can inspect state before calling [`run`].
pub struct App {
    pub listener: TcpListener,
    pub job_store: JobStore,
    pub cancel: CancellationToken,
    router: axum::Router,
    worker: tokio::task::JoinHandle<()>,
    sweeper: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("job_store", &self.job_store)
            .finish_non_exhaustive()
    }
}

/// Build the app from bootstrap config. Opens the database, loads
/// config from `SQLite`, starts the worker pool, and prepares the
/// combined HTTP router (webhooks + API + health + metrics).
///
/// # Errors
///
/// Returns an error if the database cannot be opened, config loading
/// fails, the worker pool cannot start, or the TCP listener cannot bind.
pub async fn build(bootstrap: Bootstrap) -> Result<App, Box<dyn std::error::Error>> {
    // Database
    let db = Database::open(&bootstrap.database_path).await?;
    let store = JobStore::new(db.clone());
    let repo = ConfigRepo::new(db.pool().clone());

    // Log pending jobs from previous run
    let stats = store.stats().await?;
    if stats.pending > 0 {
        info!(
            pending = stats.pending,
            "found pending jobs from previous run — will process"
        );
    }
    if stats.dead_letter > 0 {
        info!(
            dead_letter = stats.dead_letter,
            "dead-letter jobs exist — inspect via GET /api/v1/jobs?status=dead_letter"
        );
    }

    // Load config from SQLite (may be None if setup not complete)
    let config = repo
        .load_config(
            bootstrap.port,
            bootstrap.log_level.clone(),
            bootstrap.media_base_path.clone(),
            bootstrap.database_path.clone(),
            bootstrap.api_key.clone(),
        )
        .await?;

    // Detector
    let ffprobe = SystemFfprobe::locate();
    if ffprobe.is_none() {
        tracing::warn!("ffprobe not found on PATH — language detection and regeneration will fail");
    }

    let cancel = CancellationToken::new();

    // Worker pool + sweeper (only if config is loaded)
    let worker;
    let sweeper;
    if let Some(ref config) = config {
        let config = Arc::new(config.clone());
        let detector = match ffprobe.clone() {
            Some(fp) => LanguageDetector::new(Arc::new(config.languages.clone()), fp),
            None => {
                return Err("ffprobe is required when instances are configured".into());
            }
        };
        let registry = HandlerRegistry::build(config.clone(), detector, Arc::new(NoopMediaServer))?;

        worker = spawn_worker_pool(
            store.clone(),
            Arc::new(registry),
            WorkerPoolConfig::new(config.queue.concurrency, "worker"),
            cancel.clone(),
        );
        sweeper = spawn_sweeper(store.clone(), Duration::from_secs(30), cancel.clone());

        info!(
            instances = config.instances.len(),
            primary = %config.languages.primary,
            "config loaded — worker pool started"
        );
    } else {
        // No config yet — webhooks will be accepted and queued,
        // but nothing processes them until config is set and
        // the container restarts.
        worker = tokio::spawn(async {});
        sweeper = tokio::spawn(async {});
        info!("no config found — setup required via /api/v1/setup/complete");
    }

    // Prometheus metrics
    let metrics_handle = observability::install();

    // Combined router
    let router = build_router(
        &bootstrap,
        config.as_ref(),
        ffprobe,
        repo,
        store.clone(),
        metrics_handle,
    );

    // Bind
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), bootstrap.port);
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "server ready");

    Ok(App {
        listener,
        job_store: store,
        cancel,
        router,
        worker,
        sweeper,
    })
}

fn build_router(
    bootstrap: &Bootstrap,
    config: Option<&Config>,
    ffprobe: Option<SystemFfprobe>,
    repo: ConfigRepo,
    store: JobStore,
    metrics_handle: metrics_exporter_prometheus::PrometheusHandle,
) -> axum::Router {
    use std::collections::HashMap;

    let webhook_state = if let Some(c) = config {
        webhook::AppState::new(Arc::new(c.clone()), store.clone())
    } else {
        let empty = Config {
            port: bootstrap.port,
            log_level: bootstrap.log_level.clone(),
            media_base_path: bootstrap.media_base_path.clone(),
            database_path: bootstrap.database_path.clone(),
            api_key: bootstrap.api_key.clone(),
            queue: crate::config::QueueConfig { concurrency: 2 },
            languages: crate::config::LanguagesConfig {
                primary: String::new(),
                alternates: vec![],
                definitions: HashMap::new(),
            },
            instances: vec![],
            jellyfin: None,
        };
        webhook::AppState::new(Arc::new(empty), store.clone())
    };

    let lang_config = config.map_or_else(
        || {
            Arc::new(crate::config::LanguagesConfig {
                primary: String::new(),
                alternates: vec![],
                definitions: HashMap::new(),
            })
        },
        |c| Arc::new(c.languages.clone()),
    );
    let detector_for_api = ffprobe.map(|fp| LanguageDetector::new(lang_config, fp));

    let config_arc = config.map(|c| Arc::new(c.clone()));
    let api_state = ApiState::new(
        repo,
        store,
        bootstrap.api_key.clone(),
        config_arc,
        detector_for_api,
    );

    webhook::router(webhook_state)
        .merge(api::router(api_state))
        .route(
            "/metrics",
            axum::routing::get(observability::metrics_handler).with_state(metrics_handle),
        )
}

/// Run the server until cancelled (SIGINT/SIGTERM). After the HTTP
/// server stops accepting connections, in-flight workers are awaited
/// so the current job can finish before the process exits.
///
/// # Errors
///
/// Returns an `io::Error` if the underlying TCP server encounters a
/// fatal I/O failure.
pub async fn run(app: App) -> Result<(), std::io::Error> {
    let cancel = app.cancel.clone();
    let result = axum::serve(app.listener, app.router)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
        })
        .await;

    info!("HTTP server stopped — draining worker pool");
    let _ = tokio::join!(app.worker, app.sweeper);
    info!("worker pool drained");

    result
}

/// Build with an in-memory database for testing. Returns the app
/// bound to an ephemeral port on localhost.
///
/// # Errors
///
/// Returns an error if the in-memory database cannot be created, the
/// worker pool fails to start, or the TCP listener cannot bind.
pub async fn build_test(config: Config) -> Result<App, Box<dyn std::error::Error>> {
    let db = Database::in_memory().await?;
    let store = JobStore::new(db.clone());
    let config = Arc::new(config);

    let ffprobe = SystemFfprobe::locate();
    let cancel = CancellationToken::new();

    let detector = ffprobe
        .clone()
        .map(|fp| LanguageDetector::new(Arc::new(config.languages.clone()), fp));

    if let Some(ref det) = detector {
        let registry =
            HandlerRegistry::build(config.clone(), det.clone(), Arc::new(NoopMediaServer))?;

        let _worker = spawn_worker_pool(
            store.clone(),
            Arc::new(registry),
            WorkerPoolConfig::new(config.queue.concurrency, "test-worker"),
            cancel.clone(),
        );
        let _sweeper = spawn_sweeper(store.clone(), Duration::from_secs(30), cancel.clone());
    }

    let metrics_handle = observability::install();

    let repo = ConfigRepo::new(db.pool().clone());
    let webhook_state = webhook::AppState::new(config.clone(), store.clone());
    let api_state = ApiState::new(
        repo,
        store.clone(),
        config.api_key.clone(),
        Some(config.clone()),
        detector,
    );

    let router = webhook::router(webhook_state)
        .merge(api::router(api_state))
        .route(
            "/metrics",
            axum::routing::get(observability::metrics_handler).with_state(metrics_handle),
        );

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let listener = TcpListener::bind(addr).await?;

    Ok(App {
        listener,
        job_store: store,
        cancel,
        router,
        worker: tokio::spawn(async {}),
        sweeper: tokio::spawn(async {}),
    })
}
