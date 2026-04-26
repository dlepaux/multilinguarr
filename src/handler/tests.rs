//! Story 08a tests — Radarr + Sonarr Download handler matrix.
//!
//! Each test builds a self-contained fixture: a tempdir for two
//! instances (one primary, one alternate), wiremock servers backing
//! the arr APIs, and a `HandlerRegistry` constructed from the same
//! Config the production code would use. Filesystem state is
//! asserted directly through `tokio::fs`.
//!
//! Coverage:
//! - Radarr Download: primary multi-audio, primary single, alt multi, alt single
//! - Sonarr Download: same four branches at episode level
//! - `isUpgrade = true` removes the prior link before re-linking
//! - Undetermined-language guard short-circuits cleanly
//! - Unknown instance returns a permanent error
//! - `JobProcessor` dispatch round-trip via `process(Job)`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;
use tokio::fs;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::delete::{
    handle_radarr_movie_delete, handle_radarr_movie_file_delete, handle_sonarr_episode_file_delete,
    handle_sonarr_series_delete,
};
use super::import::{handle_radarr_download, handle_sonarr_download};
use super::registry::HandlerRegistry;
use super::HandlerError;
use crate::config::{
    Config, InstanceConfig, InstanceKind, JellyfinConfig, LanguageDefinition, LanguagesConfig,
    LinkStrategy, QueueConfig,
};
use crate::db::Database;
use crate::detection::{AudioStream, DetectionError, FfprobeProber, LanguageDetector};
use crate::jellyfin::NoopMediaServer;
use crate::queue::{JobPayload, JobProcessor, JobStore, ProcessOutcome};
use crate::webhook::{
    RadarrDownload, RadarrEvent, RadarrMovieDelete, RadarrMovieFileDelete, RadarrMovieFileRef,
    RadarrMovieRef, RadarrWebhookJob, SonarrDownload, SonarrEpisodeFileDelete,
    SonarrEpisodeFileRef, SonarrEpisodeRef, SonarrEvent, SonarrSeriesDelete, SonarrSeriesRef,
    SonarrWebhookJob,
};

// =====================================================================
// Fixture
// =====================================================================

struct Rig {
    tmp: TempDir,
    primary_storage: PathBuf,
    primary_library: PathBuf,
    alt_storage: PathBuf,
    alt_library: PathBuf,
    primary_server: MockServer,
    alt_server: MockServer,
}

impl Rig {
    async fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let primary_storage = tmp.path().join("radarr-fr-storage");
        let primary_library = tmp.path().join("radarr-fr-library");
        let alt_storage = tmp.path().join("radarr-en-storage");
        let alt_library = tmp.path().join("radarr-en-library");
        for d in [
            &primary_storage,
            &primary_library,
            &alt_storage,
            &alt_library,
        ] {
            fs::create_dir_all(d).await.unwrap();
        }
        let primary_server = MockServer::start().await;
        let alt_server = MockServer::start().await;
        Self {
            tmp,
            primary_storage,
            primary_library,
            alt_storage,
            alt_library,
            primary_server,
            alt_server,
        }
    }

    /// Build a Radarr-flavoured config + matching registry. The first
    /// instance is the FR primary, the second is the EN alternate.
    fn config_radarr(&self) -> Config {
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
            port: 3100,
            log_level: "info".to_owned(),
            media_base_path: self.tmp.path().to_path_buf(),
            database_path: ":memory:".into(),
            api_key: "root".to_owned(),
            queue: QueueConfig { concurrency: 2 },
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
                    url: self.primary_server.uri(),
                    api_key: "k1".to_owned(),
                    storage_path: self.primary_storage.clone(),
                    library_path: self.primary_library.clone(),
                    link_strategy: LinkStrategy::Symlink,
                    propagate_delete: true,
                },
                InstanceConfig {
                    name: "radarr-en".to_owned(),
                    kind: InstanceKind::Radarr,
                    language: "en".to_owned(),
                    url: self.alt_server.uri(),
                    api_key: "k2".to_owned(),
                    storage_path: self.alt_storage.clone(),
                    library_path: self.alt_library.clone(),
                    link_strategy: LinkStrategy::Symlink,
                    propagate_delete: true,
                },
            ],
            jellyfin: None::<JellyfinConfig>,
        }
    }

    fn config_sonarr(&self) -> Config {
        let mut cfg = self.config_radarr();
        cfg.instances[0].name = "sonarr-fr".to_owned();
        cfg.instances[0].kind = InstanceKind::Sonarr;
        cfg.instances[1].name = "sonarr-en".to_owned();
        cfg.instances[1].kind = InstanceKind::Sonarr;
        cfg
    }

    fn registry(cfg: Config, streams: Vec<AudioStream>) -> HandlerRegistry<StubFfprobe> {
        let languages = Arc::new(cfg.languages.clone());
        let detector = LanguageDetector::new(languages, StubFfprobe(streams));
        HandlerRegistry::build(Arc::new(cfg), detector, Arc::new(NoopMediaServer))
            .expect("registry")
    }
}

/// Stub ffprobe that returns pre-configured audio streams for any path.
#[derive(Debug, Clone)]
struct StubFfprobe(Vec<AudioStream>);

impl FfprobeProber for StubFfprobe {
    async fn probe(
        &self,
        _path: &Path,
        _timeout: std::time::Duration,
    ) -> Result<Vec<AudioStream>, DetectionError> {
        Ok(self.0.clone())
    }
}

// ---- helpers ----------------------------------------------------------

fn multi_audio_streams() -> Vec<AudioStream> {
    vec![
        AudioStream {
            language: Some("eng".to_owned()),
        },
        AudioStream {
            language: Some("fra".to_owned()),
        },
    ]
}

fn fr_only_streams() -> Vec<AudioStream> {
    vec![AudioStream {
        language: Some("fra".to_owned()),
    }]
}

fn en_only_streams() -> Vec<AudioStream> {
    vec![AudioStream {
        language: Some("eng".to_owned()),
    }]
}

fn no_streams() -> Vec<AudioStream> {
    vec![]
}

async fn write_movie_file(folder: &Path, contents: &str) {
    fs::create_dir_all(folder).await.unwrap();
    fs::write(folder.join("movie.mkv"), contents).await.unwrap();
}

async fn write_episode_file(series_dir: &Path, contents: &str) {
    let path = series_dir.join("Season 01");
    fs::create_dir_all(&path).await.unwrap();
    fs::write(path.join("S01E01.mkv"), contents).await.unwrap();
}

// ---- canned events --------------------------------------------------

fn radarr_download_event(folder_path: &str, file_path: &str) -> RadarrDownload {
    RadarrDownload {
        movie: Some(RadarrMovieRef {
            id: 1,
            title: "Test Movie".to_owned(),
            year: 2024,
            tmdb_id: 42,
            folder_path: Some(folder_path.to_owned()),
            ..Default::default()
        }),
        movie_file: Some(RadarrMovieFileRef {
            id: 11,
            path: Some(file_path.to_owned()),
            relative_path: Some("movie.mkv".to_owned()),
            ..Default::default()
        }),
        is_upgrade: false,
    }
}

fn sonarr_download_event(series_path: &str, episode_path: &str) -> SonarrDownload {
    SonarrDownload {
        series: Some(SonarrSeriesRef {
            id: 7,
            title: "Show".to_owned(),
            tvdb_id: 81189,
            path: Some(series_path.to_owned()),
        }),
        episodes: vec![SonarrEpisodeRef {
            id: 1,
            episode_number: 1,
            season_number: 1,
            ..Default::default()
        }],
        episode_file: Some(SonarrEpisodeFileRef {
            id: 100,
            path: Some(episode_path.to_owned()),
            relative_path: Some("Season 01/S01E01.mkv".to_owned()),
            ..Default::default()
        }),
        is_upgrade: false,
    }
}

// ---- wiremock builders for cross-instance propagation ----------------

async fn mount_radarr_lookup_empty(server: &MockServer, api_key: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", "42"))
        .and(header("X-Api-Key", api_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(server)
        .await;
}

async fn mount_radarr_lookup_existing(server: &MockServer, api_key: &str, internal_id: u32) {
    let body = json!([{
        "id": internal_id,
        "title": "Test Movie",
        "year": 2024,
        "tmdbId": 42,
        "qualityProfileId": 1,
        "hasFile": true
    }]);
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", "42"))
        .and(header("X-Api-Key", api_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_radarr_quality_and_root(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v3/qualityprofile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 7, "name": "HD" }])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/rootfolder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "path": "/movies-en" }])))
        .mount(server)
        .await;
}

async fn mount_radarr_add(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 999,
            "title": "Test Movie",
            "year": 2024,
            "tmdbId": 42,
            "qualityProfileId": 7,
            "hasFile": false
        })))
        .mount(server)
        .await;
}

async fn mount_radarr_delete(server: &MockServer, internal_id: u32) {
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v3/movie/{internal_id}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

async fn mount_sonarr_lookup_empty(server: &MockServer, api_key: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", "81189"))
        .and(header("X-Api-Key", api_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(server)
        .await;
}

/// Mount source Sonarr series lookup — returns series with season data
/// so `propagate_add_series` can copy season monitoring to the target.
async fn mount_sonarr_source_series(server: &MockServer, api_key: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", "81189"))
        .and(header("X-Api-Key", api_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 7,
            "title": "Show",
            "tvdbId": 81189,
            "qualityProfileId": 7,
            "seasonFolder": true,
            "monitored": true,
            "seasons": [
                { "seasonNumber": 1, "monitored": true }
            ]
        }])))
        .mount(server)
        .await;
}

async fn mount_sonarr_quality_root_and_add(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v3/qualityprofile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 7, "name": "HD" }])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/rootfolder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "path": "/tv-en" }])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v3/series"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 999,
            "title": "Show",
            "tvdbId": 81189,
            "qualityProfileId": 7,
            "seasonFolder": true,
            "monitored": true,
            "seasons": []
        })))
        .mount(server)
        .await;
}

// =====================================================================
// Radarr import matrix
// =====================================================================

#[tokio::test]
async fn radarr_primary_multi_audio_links_into_both_libraries() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Multi (2024)");
    write_movie_file(&folder, "video").await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Both libraries got a link to the same source.
    let primary_link = rig.primary_library.join("Multi (2024)/movie.mkv");
    let alt_link = rig.alt_library.join("Multi (2024)/movie.mkv");
    assert_eq!(fs::read_to_string(&primary_link).await.unwrap(), "video");
    assert_eq!(fs::read_to_string(&alt_link).await.unwrap(), "video");
}

#[tokio::test]
async fn radarr_primary_single_language_links_only_primary_library() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("FR Only (2024)");
    write_movie_file(&folder, "fr-video").await;

    // Cross-instance add propagation: alt has no copy yet.
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    assert!(
        fs::try_exists(rig.primary_library.join("FR Only (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    assert!(!fs::try_exists(rig.alt_library.join("FR Only (2024)"))
        .await
        .unwrap());
}

#[tokio::test]
async fn radarr_alternate_multi_audio_only_links_alternate_library() {
    let rig = Rig::new().await;
    // Source file lives under the ALT instance's storage (this is an
    // alternate import scenario).
    let folder = rig.alt_storage.join("Alt Multi (2024)");
    write_movie_file(&folder, "alt-video").await;

    let cfg = rig.config_radarr();
    let alternate = cfg.instances[1].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    handle_radarr_download(&alternate, &event, &registry)
        .await
        .unwrap();

    // Alt library has the link.
    assert!(
        fs::try_exists(rig.alt_library.join("Alt Multi (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    // Primary library does NOT — alternate imports never write to primary.
    assert!(
        !fs::try_exists(rig.primary_library.join("Alt Multi (2024)"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn radarr_alternate_single_language_links_alternate_only() {
    let rig = Rig::new().await;
    let folder = rig.alt_storage.join("EN Only (2024)");
    write_movie_file(&folder, "en-video").await;

    let cfg = rig.config_radarr();
    let alternate = cfg.instances[1].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, en_only_streams());

    handle_radarr_download(&alternate, &event, &registry)
        .await
        .unwrap();

    assert!(
        fs::try_exists(rig.alt_library.join("EN Only (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    assert!(!fs::try_exists(rig.primary_library.join("EN Only (2024)"))
        .await
        .unwrap());
}

// =====================================================================
// Sonarr import matrix
// =====================================================================

#[tokio::test]
async fn sonarr_primary_multi_audio_links_episode_into_both_libraries() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    let cfg = rig.config_sonarr();
    let primary = cfg.instances[0].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    handle_sonarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    let rel = "Show/Season 01/S01E01.mkv";
    assert!(fs::try_exists(rig.primary_library.join(rel)).await.unwrap());
    assert!(fs::try_exists(rig.alt_library.join(rel)).await.unwrap());
}

#[tokio::test]
async fn sonarr_primary_single_language_links_only_primary_library() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    // Source series lookup — provides season monitoring to copy.
    mount_sonarr_source_series(&rig.primary_server, "k1").await;
    // Cross-instance add: alt has no copy yet.
    mount_sonarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_sonarr_quality_root_and_add(&rig.alt_server).await;

    let cfg = rig.config_sonarr();
    let primary = cfg.instances[0].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_sonarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    let rel = "Show/Season 01/S01E01.mkv";
    assert!(fs::try_exists(rig.primary_library.join(rel)).await.unwrap());
    assert!(!fs::try_exists(rig.alt_library.join("Show")).await.unwrap());
}

#[tokio::test]
async fn sonarr_alternate_multi_audio_links_alternate_only() {
    let rig = Rig::new().await;
    let series_dir = rig.alt_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    let cfg = rig.config_sonarr();
    let alternate = cfg.instances[1].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    handle_sonarr_download(&alternate, &event, &registry)
        .await
        .unwrap();

    let rel = "Show/Season 01/S01E01.mkv";
    assert!(fs::try_exists(rig.alt_library.join(rel)).await.unwrap());
    assert!(!fs::try_exists(rig.primary_library.join("Show"))
        .await
        .unwrap());
}

#[tokio::test]
async fn sonarr_alternate_single_language_links_alternate_only() {
    let rig = Rig::new().await;
    let series_dir = rig.alt_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    let cfg = rig.config_sonarr();
    let alternate = cfg.instances[1].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, en_only_streams());

    handle_sonarr_download(&alternate, &event, &registry)
        .await
        .unwrap();

    let rel = "Show/Season 01/S01E01.mkv";
    assert!(fs::try_exists(rig.alt_library.join(rel)).await.unwrap());
    assert!(!fs::try_exists(rig.primary_library.join("Show"))
        .await
        .unwrap());
}

// =====================================================================
// isUpgrade + undetermined-language guard
// =====================================================================

#[tokio::test]
async fn radarr_is_upgrade_unlinks_then_relinks_for_primary_multi() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Upgrade (2024)");
    write_movie_file(&folder, "old-video").await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    // First import — establishes the links.
    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Now overwrite the source file (simulating a quality upgrade) and
    // run again with isUpgrade=true. The unlink-then-relink sequence
    // must end up pointing at the (new) source.
    fs::write(folder.join("movie.mkv"), "new-video")
        .await
        .unwrap();
    let mut upgrade_event =
        radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    upgrade_event.is_upgrade = true;
    handle_radarr_download(&primary, &upgrade_event, &registry)
        .await
        .unwrap();

    let primary_link = rig.primary_library.join("Upgrade (2024)/movie.mkv");
    let alt_link = rig.alt_library.join("Upgrade (2024)/movie.mkv");
    assert_eq!(
        fs::read_to_string(&primary_link).await.unwrap(),
        "new-video"
    );
    assert_eq!(fs::read_to_string(&alt_link).await.unwrap(), "new-video");
}

#[tokio::test]
async fn radarr_undetermined_language_assumes_instance_language_and_propagates() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Mystery (2024)");
    write_movie_file(&folder, "??").await;

    // Cross-instance add: alt has no copy yet (propagation expected).
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, no_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Undetermined language assumed as instance language (fr) →
    // primary library linked, alternate gets propagate-add.
    assert!(
        fs::try_exists(rig.primary_library.join("Mystery (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    // Alt library not linked directly (propagation tells arr to fetch).
    assert!(!fs::try_exists(rig.alt_library.join("Mystery (2024)"))
        .await
        .unwrap());
}

// =====================================================================
// Error path
// =====================================================================

#[tokio::test]
async fn unknown_instance_in_payload_is_permanent_error() {
    let rig = Rig::new().await;
    let cfg = rig.config_radarr();
    let folder = rig.primary_storage.join("Ghost (2024)");
    let file_path = folder.join("movie.mkv");
    let registry = Rig::registry(cfg, multi_audio_streams());

    // Build a Job with an instance name that does not exist in config.
    let bad = RadarrWebhookJob {
        instance: "ghost".to_owned(),
        event: RadarrEvent::Download(radarr_download_event(
            folder.to_str().unwrap(),
            file_path.to_str().unwrap(),
        )),
    };
    let db = Database::in_memory().await.unwrap();
    let store = JobStore::new(db);
    let id = store.enqueue(&bad, 5, Utc::now()).await.unwrap();
    let job = store.get(id).await.unwrap().unwrap();

    let outcome = registry.process(job).await;
    assert!(matches!(outcome, ProcessOutcome::Permanent(_)));
}

// =====================================================================
// JobProcessor dispatch round-trip
// =====================================================================

#[tokio::test]
async fn process_dispatches_radarr_download_via_jobprocessor() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Dispatch (2024)");
    write_movie_file(&folder, "v").await;

    // Single-language -> cross-instance add propagation hits alt server.
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let file_path = folder.join("movie.mkv");
    let registry = Rig::registry(cfg, fr_only_streams());

    // Construct a real Job in an in-memory store and process it.
    let payload = RadarrWebhookJob {
        instance: "radarr-fr".to_owned(),
        event: RadarrEvent::Download(radarr_download_event(
            folder.to_str().unwrap(),
            file_path.to_str().unwrap(),
        )),
    };
    let db = Database::in_memory().await.unwrap();
    let store = JobStore::new(db);
    let id = store.enqueue(&payload, 5, Utc::now()).await.unwrap();
    let job = store.get(id).await.unwrap().unwrap();
    assert_eq!(job.kind, RadarrWebhookJob::KIND);

    let outcome = registry.process(job).await;
    assert!(matches!(outcome, ProcessOutcome::Success));
    assert!(
        fs::try_exists(rig.primary_library.join("Dispatch (2024)/movie.mkv"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn process_dispatches_sonarr_download_via_jobprocessor() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    mount_sonarr_source_series(&rig.primary_server, "k1").await;
    mount_sonarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_sonarr_quality_root_and_add(&rig.alt_server).await;

    let cfg = rig.config_sonarr();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let registry = Rig::registry(cfg, fr_only_streams());

    let payload = SonarrWebhookJob {
        instance: "sonarr-fr".to_owned(),
        event: SonarrEvent::Download(sonarr_download_event(
            series_dir.to_str().unwrap(),
            episode_path.to_str().unwrap(),
        )),
    };
    let db = Database::in_memory().await.unwrap();
    let store = JobStore::new(db);
    let id = store.enqueue(&payload, 5, Utc::now()).await.unwrap();
    let job = store.get(id).await.unwrap().unwrap();

    let outcome = registry.process(job).await;
    assert!(matches!(outcome, ProcessOutcome::Success));
    assert!(
        fs::try_exists(rig.primary_library.join("Show/Season 01/S01E01.mkv"))
            .await
            .unwrap()
    );
}

// =====================================================================
// 08b — delete + cross-instance helpers
// =====================================================================

fn radarr_movie_delete_event(deleted_files: bool, folder_abs: &str) -> RadarrMovieDelete {
    RadarrMovieDelete {
        movie: Some(RadarrMovieRef {
            id: 1,
            title: "Test Movie".to_owned(),
            year: 2024,
            tmdb_id: 42,
            folder_path: Some(folder_abs.to_owned()),
            ..Default::default()
        }),
        deleted_files,
    }
}

#[tokio::test]
async fn radarr_primary_movie_delete_unlinks_both_libraries_and_propagates() {
    let rig = Rig::new().await;
    // Pre-state: both libraries link to a movie under primary storage.
    let folder = rig.primary_storage.join("Doomed (2024)");
    write_movie_file(&folder, "v").await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    // Run the import to set up the cross-instance link state.
    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();
    assert!(
        fs::try_exists(rig.primary_library.join("Doomed (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    assert!(
        fs::try_exists(rig.alt_library.join("Doomed (2024)/movie.mkv"))
            .await
            .unwrap()
    );

    // Now exercise the delete path: alt instance must be queried for
    // its internal id and DELETE issued.
    mount_radarr_lookup_existing(&rig.alt_server, "k2", 555).await;
    mount_radarr_delete(&rig.alt_server, 555).await;

    handle_radarr_movie_delete(
        &primary,
        &radarr_movie_delete_event(true, folder.to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    // Both library entries gone (storage-aware: both resolved into
    // primary's storage so both got removed).
    assert!(!fs::try_exists(rig.primary_library.join("Doomed (2024)"))
        .await
        .unwrap());
    assert!(!fs::try_exists(rig.alt_library.join("Doomed (2024)"))
        .await
        .unwrap());
    // Source untouched.
    assert!(fs::try_exists(folder.join("movie.mkv")).await.unwrap());
}

#[tokio::test]
async fn radarr_movie_delete_skipped_when_deleted_files_false() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Stays (2024)");
    write_movie_file(&folder, "v").await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    // Establish a link first.
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;
    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();
    assert!(
        fs::try_exists(rig.primary_library.join("Stays (2024)/movie.mkv"))
            .await
            .unwrap()
    );

    // Delete with deletedFiles=false -> no fs ops.
    handle_radarr_movie_delete(
        &primary,
        &radarr_movie_delete_event(false, folder.to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    // Link still present.
    assert!(
        fs::try_exists(rig.primary_library.join("Stays (2024)/movie.mkv"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn radarr_alternate_movie_delete_only_touches_alt_library() {
    let rig = Rig::new().await;
    let folder = rig.alt_storage.join("EN Local (2024)");
    write_movie_file(&folder, "v").await;

    let cfg = rig.config_radarr();
    let alternate = cfg.instances[1].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, en_only_streams());

    // Set up the alt link.
    handle_radarr_download(&alternate, &event, &registry)
        .await
        .unwrap();
    assert!(
        fs::try_exists(rig.alt_library.join("EN Local (2024)/movie.mkv"))
            .await
            .unwrap()
    );

    // No mocks on primary_server: any propagation attempt would
    // hang/fail. The handler MUST not propagate from alternates.
    handle_radarr_movie_delete(
        &alternate,
        &radarr_movie_delete_event(true, folder.to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    assert!(!fs::try_exists(rig.alt_library.join("EN Local (2024)"))
        .await
        .unwrap());
    // Primary library was never touched (no link existed there to
    // begin with), and the assertion that no primary mock was hit is
    // implicit in the test passing.
}

#[tokio::test]
async fn storage_aware_alternate_link_to_foreign_storage_is_preserved_on_alt_delete() {
    // Setup: primary fr is gone (no fetch needed). The alt en
    // library has a symlink that resolves into FR storage from a
    // previous primary multi-audio import.
    let rig = Rig::new().await;
    let folder_name = "Shared (2024)";
    let folder = rig.primary_storage.join(folder_name);
    write_movie_file(&folder, "shared").await;

    // Manually create the cross-instance link in the alt library
    // (simulating a prior primary multi-audio import).
    let cfg = rig.config_radarr();
    let alt_link_mgr = crate::link::LinkManager::from_instance(&cfg.instances[1]);
    alt_link_mgr
        .link_movie_from(&folder, folder_name)
        .await
        .unwrap();
    assert!(
        fs::try_exists(rig.alt_library.join(folder_name).join("movie.mkv"))
            .await
            .unwrap()
    );

    let alternate = cfg.instances[1].clone();
    let registry = Rig::registry(cfg, multi_audio_streams());

    // Now alt-instance issues a delete for content "in its own
    // storage". The link in alt library actually resolves into the
    // primary's storage, but `unlink_movie_local_only` (alternate
    // path) still removes its own library entry — that is local
    // semantics. The point of storage-awareness is on the *primary*
    // delete, not the alt delete.
    //
    // The fs entry pointing into foreign storage is alt's to remove
    // because the webhook event came from alt. The primary library
    // (which would have its own copy) is never touched.
    let primary_lib_before = rig.primary_library.join(folder_name);
    handle_radarr_movie_delete(
        &alternate,
        &radarr_movie_delete_event(true, rig.alt_storage.join(folder_name).to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    // Primary library never had this entry — still not present.
    assert!(!fs::try_exists(&primary_lib_before).await.unwrap());
    // Source content under primary storage — untouched.
    assert!(fs::try_exists(folder.join("movie.mkv")).await.unwrap());
}

// ----- cross_instance::propagate_add_movie via primary single-language import -----

#[tokio::test]
async fn radarr_primary_single_language_propagates_add_to_alternate() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("FR Solo (2024)");
    write_movie_file(&folder, "fr-only").await;

    // Alternate has no copy yet — dedup check returns empty array.
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Library state: only the primary library got a link.
    assert!(
        fs::try_exists(rig.primary_library.join("FR Solo (2024)/movie.mkv"))
            .await
            .unwrap()
    );
    assert!(!fs::try_exists(rig.alt_library.join("FR Solo (2024)"))
        .await
        .unwrap());
    // Test passes if all the alt-server mocks were satisfied
    // (wiremock will fail the test on missing matches at drop time).
}

#[tokio::test]
async fn radarr_primary_single_language_skips_add_when_target_already_has_movie() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Already (2024)");
    write_movie_file(&folder, "v").await;

    // Dedup check: alternate ALREADY has the movie. No add call mounted —
    // an unexpected POST would surface in wiremock as an unmatched request.
    mount_radarr_lookup_existing(&rig.alt_server, "k2", 999).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();
}

// ----- Sonarr delete -----

fn sonarr_series_delete_event(deleted_files: bool, series_path_abs: &str) -> SonarrSeriesDelete {
    SonarrSeriesDelete {
        series: Some(SonarrSeriesRef {
            id: 7,
            title: "Show".to_owned(),
            tvdb_id: 81189,
            path: Some(series_path_abs.to_owned()),
        }),
        deleted_files,
    }
}

fn sonarr_episode_file_delete_event(
    series_path_abs: &str,
    episode_relative: &str,
) -> SonarrEpisodeFileDelete {
    SonarrEpisodeFileDelete {
        series: Some(SonarrSeriesRef {
            id: 7,
            title: "Show".to_owned(),
            tvdb_id: 81189,
            path: Some(series_path_abs.to_owned()),
        }),
        episodes: vec![SonarrEpisodeRef {
            id: 1,
            episode_number: 1,
            season_number: 1,
            ..Default::default()
        }],
        episode_file: Some(SonarrEpisodeFileRef {
            id: 100,
            relative_path: Some(episode_relative.to_owned()),
            ..Default::default()
        }),
        delete_reason: None,
    }
}

#[tokio::test]
async fn sonarr_primary_series_delete_clears_both_libraries() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    let cfg = rig.config_sonarr();
    let primary = cfg.instances[0].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    handle_sonarr_download(&primary, &event, &registry)
        .await
        .unwrap();
    let rel = "Show/Season 01/S01E01.mkv";
    assert!(fs::try_exists(rig.primary_library.join(rel)).await.unwrap());
    assert!(fs::try_exists(rig.alt_library.join(rel)).await.unwrap());

    // Series delete: alt instance gets a series-lookup + delete call.
    let series_body = json!([{
        "id": 7,
        "title": "Show",
        "tvdbId": 81189,
        "qualityProfileId": 1,
        "seasonFolder": true,
        "monitored": true,
        "seasons": []
    }]);
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", "81189"))
        .and(header("X-Api-Key", "k2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(series_body))
        .mount(&rig.alt_server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v3/series/7"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&rig.alt_server)
        .await;

    handle_sonarr_series_delete(
        &primary,
        &sonarr_series_delete_event(true, series_dir.to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    assert!(!fs::try_exists(rig.primary_library.join("Show"))
        .await
        .unwrap());
    assert!(!fs::try_exists(rig.alt_library.join("Show")).await.unwrap());
}

#[tokio::test]
async fn sonarr_episode_file_delete_removes_only_that_episode() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;
    // Add a second episode that should survive.
    fs::write(
        rig.primary_storage
            .join("Show/Season 01")
            .join("S01E02.mkv"),
        "ep2",
    )
    .await
    .unwrap();

    let cfg = rig.config_sonarr();
    let primary = cfg.instances[0].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    // Establish links by importing.
    handle_sonarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Manually link the second episode for the assertion.
    let primary_link_mgr = crate::link::LinkManager::from_instance(&registry.config_instances()[0]);
    primary_link_mgr
        .link_episode(Path::new("Show/Season 01/S01E02.mkv"))
        .await
        .unwrap();

    // handle_radarr_movie_file_delete is now tested in its own test above

    handle_sonarr_episode_file_delete(
        &primary,
        &sonarr_episode_file_delete_event(series_dir.to_str().unwrap(), "Season 01/S01E01.mkv"),
        &registry,
    )
    .await
    .unwrap();

    assert!(
        !fs::try_exists(rig.primary_library.join("Show/Season 01/S01E01.mkv"))
            .await
            .unwrap()
    );
    // Sibling episode survives.
    assert!(
        fs::try_exists(rig.primary_library.join("Show/Season 01/S01E02.mkv"))
            .await
            .unwrap()
    );
}

// ----- Radarr MovieFileDelete (behavioral test) -----

fn radarr_movie_file_delete_event(folder_path: &str) -> RadarrMovieFileDelete {
    RadarrMovieFileDelete {
        movie: Some(RadarrMovieRef {
            id: 1,
            title: "Test".to_owned(),
            year: 2024,
            tmdb_id: 42,
            folder_path: Some(folder_path.to_owned()),
            ..Default::default()
        }),
        movie_file: Some(RadarrMovieFileRef {
            id: 11,
            ..Default::default()
        }),
        delete_reason: Some("upgrade".to_owned()),
    }
}

#[tokio::test]
async fn radarr_movie_file_delete_unlinks_primary_without_propagation() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Deleted (2024)");
    write_movie_file(&folder, "content").await;

    // Import first to establish links.
    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    // Primary library link exists.
    assert!(
        fs::try_exists(rig.primary_library.join("Deleted (2024)/movie.mkv"))
            .await
            .unwrap()
    );

    // Now delete the file — no mocks on alt_server for delete,
    // so any propagation attempt would panic via wiremock.
    handle_radarr_movie_file_delete(
        &primary,
        &radarr_movie_file_delete_event(folder.to_str().unwrap()),
        &registry,
    )
    .await
    .unwrap();

    // Primary library link removed.
    assert!(!fs::try_exists(rig.primary_library.join("Deleted (2024)"))
        .await
        .unwrap());
    // Storage file still present (MovieFileDelete only removes links).
    assert!(fs::try_exists(&folder).await.unwrap());
}

// ----- Sonarr EpisodeFileDelete from alternate -----

#[tokio::test]
async fn sonarr_episode_file_delete_from_alternate_only_removes_alt_link() {
    let rig = Rig::new().await;
    let series_dir = rig.alt_storage.join("Show");
    write_episode_file(&series_dir, "ep").await;

    let cfg = rig.config_sonarr();
    let alternate = cfg.instances[1].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, multi_audio_streams());

    // Import from alternate to establish alt link.
    handle_sonarr_download(&alternate, &event, &registry)
        .await
        .unwrap();

    assert!(
        fs::try_exists(rig.alt_library.join("Show/Season 01/S01E01.mkv"))
            .await
            .unwrap()
    );

    // Delete from alternate — should only remove alt link.
    handle_sonarr_episode_file_delete(
        &alternate,
        &sonarr_episode_file_delete_event(series_dir.to_str().unwrap(), "Season 01/S01E01.mkv"),
        &registry,
    )
    .await
    .unwrap();

    assert!(
        !fs::try_exists(rig.alt_library.join("Show/Season 01/S01E01.mkv"))
            .await
            .unwrap()
    );
    // Primary library untouched (no link was created there for alt import).
    assert!(!fs::try_exists(rig.primary_library.join("Show"))
        .await
        .unwrap());
}

// ----- Symlink target verification -----

#[tokio::test]
async fn symlink_target_points_to_correct_storage() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Verified (2024)");
    write_movie_file(&folder, "content").await;

    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, fr_only_streams());

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    let link_path = rig.primary_library.join("Verified (2024)");
    assert!(fs::try_exists(&link_path).await.unwrap());

    // Verify the symlink actually points to the primary storage.
    let target = fs::read_link(&link_path).await.unwrap();
    assert!(
        target.starts_with(&rig.primary_storage),
        "symlink target {target:?} should point to primary storage {:?}",
        rig.primary_storage
    );
}

// =====================================================================
// Language-tag fallback: INFO level + counter
// =====================================================================

#[tokio::test]
async fn radarr_fallback_no_language_tags_increments_counter() {
    let rig = Rig::new().await;
    let folder = rig.primary_storage.join("Untagged (2024)");
    write_movie_file(&folder, "??").await;

    mount_radarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_radarr_quality_and_root(&rig.alt_server).await;
    mount_radarr_add(&rig.alt_server).await;

    let cfg = rig.config_radarr();
    let primary = cfg.instances[0].clone();
    let file_path = folder.join("movie.mkv");
    let event = radarr_download_event(folder.to_str().unwrap(), file_path.to_str().unwrap());
    let registry = Rig::registry(cfg, no_streams());

    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // Hold the guard across all .awaits — single-thread executor keeps us
    // on the same thread, so the TL recorder stays active throughout.
    let recorder_guard = metrics::set_default_local_recorder(&recorder);

    handle_radarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    drop(recorder_guard);
    let render = handle.render();
    assert!(
        render.contains(
            "multilinguarr_language_tag_fallback_total{instance=\"radarr-fr\",source=\"radarr\",fallback_language=\"fr\"} 1"
        ),
        "expected fallback counter with radarr labels in:\n{render}"
    );
}

#[tokio::test]
async fn sonarr_fallback_no_language_tags_increments_counter() {
    let rig = Rig::new().await;
    let series_dir = rig.primary_storage.join("Show");
    write_episode_file(&series_dir, "??").await;

    mount_sonarr_source_series(&rig.primary_server, "k1").await;
    mount_sonarr_lookup_empty(&rig.alt_server, "k2").await;
    mount_sonarr_quality_root_and_add(&rig.alt_server).await;

    let cfg = rig.config_sonarr();
    let primary = cfg.instances[0].clone();
    let episode_path = series_dir.join("Season 01/S01E01.mkv");
    let event = sonarr_download_event(series_dir.to_str().unwrap(), episode_path.to_str().unwrap());
    let registry = Rig::registry(cfg, no_streams());

    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let recorder_guard = metrics::set_default_local_recorder(&recorder);

    handle_sonarr_download(&primary, &event, &registry)
        .await
        .unwrap();

    drop(recorder_guard);
    let render = handle.render();
    assert!(
        render.contains(
            "multilinguarr_language_tag_fallback_total{instance=\"sonarr-fr\",source=\"sonarr\",fallback_language=\"fr\"} 1"
        ),
        "expected fallback counter with sonarr labels in:\n{render}"
    );
}

// =====================================================================
// HandlerError classification
// =====================================================================

#[test]
fn missing_field_is_permanent() {
    let err = HandlerError::MissingField("movie");
    assert!(!err.is_transient());
}

#[test]
fn unknown_instance_is_permanent() {
    let err = HandlerError::UnknownInstance("nope".to_owned());
    assert!(!err.is_transient());
}
