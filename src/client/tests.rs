//! Wiremock-backed tests for the Radarr/Sonarr HTTP clients.
//!
//! These do not depend on live arr instances. Every test spins up a
//! mock server, attaches a mock response, and verifies the client
//! issues the request we expect and deserializes the response we
//! expect. No process-wide state is mutated.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::{
    AddMovieOptions, AddMovieRequest, ArrClient, ArrError, HttpCore, RadarrClient, RetryPolicy,
    SonarrClient,
};
use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};

fn test_http(server: &MockServer, retry: RetryPolicy) -> HttpCore {
    HttpCore::new(
        "test",
        &server.uri(),
        "secret",
        Duration::from_secs(5),
        retry,
    )
    .expect("http core")
}

fn test_instance(kind: InstanceKind, url: String) -> InstanceConfig {
    InstanceConfig {
        name: "test".to_owned(),
        kind,
        language: "fr".to_owned(),
        url,
        api_key: "secret".to_owned(),
        storage_path: "/tmp/storage".into(),
        library_path: "/tmp/library".into(),
        link_strategy: LinkStrategy::Symlink,
        propagate_delete: true,
    }
}

// ---------------- Radarr ----------------

#[tokio::test]
async fn radarr_get_movie_by_tmdb_id_returns_first_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", "603"))
        .and(header("X-Api-Key", "secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "The Matrix",
            "year": 1999,
            "tmdbId": 603,
            "qualityProfileId": 1,
            "hasFile": true
        }])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let movie = client.get_movie_by_tmdb_id(603).await.unwrap().unwrap();
    assert_eq!(movie.id, 1);
    assert_eq!(movie.title, "The Matrix");
    assert_eq!(movie.tmdb_id, 603);
}

#[tokio::test]
async fn radarr_get_movie_by_tmdb_id_returns_none_on_empty_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    assert!(client.get_movie_by_tmdb_id(42).await.unwrap().is_none());
}

#[tokio::test]
async fn radarr_add_movie_serializes_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .and(header("X-Api-Key", "secret"))
        .and(body_json(json!({
            "title": "Inception",
            "year": 2010,
            "tmdbId": 27205,
            "qualityProfileId": 1,
            "rootFolderPath": "/movies",
            "monitored": true,
            "addOptions": { "searchForMovie": true }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 99,
            "title": "Inception",
            "year": 2010,
            "tmdbId": 27205,
            "qualityProfileId": 1,
            "hasFile": false
        })))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let req = AddMovieRequest {
        title: "Inception".to_owned(),
        year: 2010,
        tmdb_id: 27205,
        quality_profile_id: 1,
        root_folder_path: "/movies".to_owned(),
        monitored: true,
        add_options: AddMovieOptions {
            search_for_movie: true,
        },
    };
    let movie = client.add_movie(&req).await.unwrap();
    assert_eq!(movie.id, 99);
}

#[tokio::test]
async fn radarr_delete_movie_issues_delete_with_flag() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v3/movie/42"))
        .and(query_param("deleteFiles", "true"))
        .and(header("X-Api-Key", "secret"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    client.delete_movie(42, true).await.unwrap();
}

#[tokio::test]
async fn radarr_quality_profiles_and_root_folders() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/qualityprofile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "HD-1080p" },
            { "id": 2, "name": "Ultra-HD" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/rootfolder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "path": "/movies" }
        ])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let profiles = client.quality_profiles().await.unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[1].name, "Ultra-HD");
    let folders = client.root_folders().await.unwrap();
    assert_eq!(folders[0].path, "/movies");
}

// ---------------- Sonarr ----------------

#[tokio::test]
async fn sonarr_list_series_deserializes_camel_case() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 7,
            "title": "Breaking Bad",
            "year": 2008,
            "tvdbId": 81189,
            "qualityProfileId": 1,
            "seasonFolder": true,
            "monitored": true,
            "seasons": [
                { "seasonNumber": 1, "monitored": true }
            ]
        }])))
        .mount(&server)
        .await;

    let client = SonarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let series = client.list_series().await.unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].tvdb_id, 81189);
    assert_eq!(series[0].seasons.len(), 1);
    assert_eq!(series[0].seasons[0].season_number, 1);
}

#[tokio::test]
async fn sonarr_list_episode_files_for_series() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/episodefile"))
        .and(query_param("seriesId", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 11,
            "seriesId": 7,
            "seasonNumber": 1,
            "relativePath": "Season 01/S01E01.mkv",
            "path": "/tv/Breaking Bad/Season 01/S01E01.mkv",
            "quality": { "quality": { "id": 1, "name": "HDTV-720p" } },
            "languages": [ { "id": 2, "name": "French" } ]
        }])))
        .mount(&server)
        .await;

    let client = SonarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let files = client.list_episode_files(7).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].languages[0].id, 2);
}

// ---------------- Error paths + retries ----------------

#[tokio::test]
async fn not_found_maps_to_not_found_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let err = client.list_movies().await.unwrap_err();
    assert!(matches!(err, ArrError::NotFound { .. }));
    assert!(!err.is_transient());
}

#[tokio::test]
async fn client_4xx_does_not_retry_and_is_permanent() {
    let server = MockServer::start().await;
    let hit_count = Arc::new(AtomicU32::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(CountingResponder::new(
            Arc::clone(&hit_count),
            ResponseTemplate::new(401).set_body_string("unauthorized"),
        ))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(
        &server,
        RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        },
    ));
    let err = client.list_movies().await.unwrap_err();
    assert!(matches!(err, ArrError::Client { status: 401, .. }));
    assert!(!err.is_transient());
    assert_eq!(hit_count.load(Ordering::SeqCst), 1, "no retries on 4xx");
}

#[tokio::test]
async fn server_5xx_is_retried_up_to_max_attempts_then_fails() {
    let server = MockServer::start().await;
    let hit_count = Arc::new(AtomicU32::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(CountingResponder::new(
            Arc::clone(&hit_count),
            ResponseTemplate::new(503).set_body_string("maintenance"),
        ))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(
        &server,
        RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        },
    ));
    let err = client.list_movies().await.unwrap_err();
    assert!(matches!(err, ArrError::Server { status: 503, .. }));
    assert!(err.is_transient());
    assert_eq!(hit_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn transient_failure_followed_by_success_is_retried() {
    let server = MockServer::start().await;
    // First request: 503; second: 200 with empty array.
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(
        &server,
        RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        },
    ));
    let movies = client.list_movies().await.unwrap();
    assert!(movies.is_empty());
}

// ---------------- ArrClient enum construction ----------------

#[tokio::test]
async fn arr_client_from_instance_dispatches_on_kind() {
    let server = MockServer::start().await;
    let radarr = ArrClient::from_instance(&test_instance(InstanceKind::Radarr, server.uri()));
    let sonarr = ArrClient::from_instance(&test_instance(InstanceKind::Sonarr, server.uri()));
    assert!(matches!(radarr, Ok(ArrClient::Radarr(_))));
    assert!(matches!(sonarr, Ok(ArrClient::Sonarr(_))));
}

#[tokio::test]
async fn arr_client_from_instance_rejects_invalid_url() {
    let instance = test_instance(InstanceKind::Radarr, "not a url".to_owned());
    let err = ArrClient::from_instance(&instance).unwrap_err();
    assert!(matches!(err, ArrError::InvalidUrl { .. }));
}

// ---------------- helpers ----------------

/// A wiremock responder that increments a counter on every call and then
/// delegates to a static template. Lets us assert attempt counts for
/// retry tests.
struct CountingResponder {
    counter: Arc<AtomicU32>,
    template: ResponseTemplate,
}

impl CountingResponder {
    fn new(counter: Arc<AtomicU32>, template: ResponseTemplate) -> Self {
        Self { counter, template }
    }
}

impl Respond for CountingResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.counter.fetch_add(1, Ordering::SeqCst);
        self.template.clone()
    }
}
