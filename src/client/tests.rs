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
    AddMovieOptions, AddMovieRequest, AddOutcome, AddSeriesOptions, AddSeriesRequest, ArrClient,
    ArrError, HttpCore, RadarrClient, RetryPolicy, SonarrClient,
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
    let outcome = client.add_movie(&req).await.unwrap();
    assert!(
        matches!(outcome, AddOutcome::Created(ref m) if m.id == 99),
        "expected Created(id=99), got {outcome:?}"
    );
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

// ---------------- 409 race regression ----------------
//
// Reproduces the cross-instance webhook race: when N concurrent
// `add_series` (or `add_movie`) calls hit the same external id
// (`tvdb_id` / `tmdb_id`), the database UNIQUE constraint lets
// exactly one win with 201 and returns 409 to the others. Callers
// must observe N successful outcomes (1 Created + N-1 AlreadyExisted),
// never a hard error — otherwise the losers silently drop downstream
// cross-library link work.

/// Body shape Sonarr returns when its `Series.TitleSlug` UNIQUE
/// constraint fires. The classifier branches on status==409, not on
/// the body — this constant exists so the test responder can mimic
/// the wire payload faithfully (debug, logs, error chains).
const SONARR_TITLE_SLUG_409_BODY: &str = r#"[{"propertyName":"TitleSlug","errorMessage":"constraint failed\nUNIQUE constraint failed: Series.TitleSlug","errorCode":"DbUpdateException","severity":"error"}]"#;

/// Synthesised Radarr-style 409 body. Modern Radarr usually returns
/// 400 from `MovieExistsValidator` before the DB INSERT, so the wire
/// shape of a real Radarr 409 is unverified — when one fires in
/// production, capture and tighten this string.
const RADARR_TMDB_409_BODY: &str = r#"[{"propertyName":"TmdbId","errorMessage":"UNIQUE constraint failed: Movies.TmdbId","errorCode":"DbUpdateException","severity":"error"}]"#;

fn add_series_request(tvdb_id: u32) -> AddSeriesRequest {
    AddSeriesRequest {
        title: format!("Series {tvdb_id}"),
        year: Some(2024),
        tvdb_id,
        quality_profile_id: 1,
        root_folder_path: "/tv".to_owned(),
        season_folder: true,
        monitored: true,
        seasons: vec![],
        add_options: AddSeriesOptions {
            search_for_missing_episodes: true,
        },
    }
}

fn add_movie_request(tmdb_id: u32) -> AddMovieRequest {
    AddMovieRequest {
        title: format!("Movie {tmdb_id}"),
        year: 2024,
        tmdb_id,
        quality_profile_id: 1,
        root_folder_path: "/movies".to_owned(),
        monitored: true,
        add_options: AddMovieOptions {
            search_for_movie: true,
        },
    }
}

/// A wiremock responder that returns 201 (Created) for the **first**
/// caller and 409 (UNIQUE constraint) for every subsequent caller.
/// Determinism comes from `AtomicU32::fetch_add`, NOT from wiremock's
/// arrival order — whichever Tokio task lands `0` is the winner.
struct OneCreatedThenConflictResponder {
    counter: Arc<AtomicU32>,
    created_body: serde_json::Value,
    conflict_body: &'static str,
}

impl Respond for OneCreatedThenConflictResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            ResponseTemplate::new(201).set_body_json(self.created_body.clone())
        } else {
            ResponseTemplate::new(409).set_body_string(self.conflict_body)
        }
    }
}

// ----- Sonarr / add_series -----

#[tokio::test]
async fn add_series_first_call_returns_created_subsequent_409_become_already_existed() {
    let server = MockServer::start().await;
    let tvdb_id: u32 = 468_226;
    let seeded = json!({
        "id": 21,
        "title": "Seeded Series",
        "tvdbId": tvdb_id,
        "qualityProfileId": 1,
        "seasonFolder": true,
        "monitored": true,
        "seasons": []
    });

    let counter = Arc::new(AtomicU32::new(0));
    Mock::given(method("POST"))
        .and(path("/api/v3/series"))
        .respond_with(OneCreatedThenConflictResponder {
            counter: Arc::clone(&counter),
            created_body: seeded.clone(),
            conflict_body: SONARR_TITLE_SLUG_409_BODY,
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", tvdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([seeded])))
        .mount(&server)
        .await;

    let client = SonarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let req = add_series_request(tvdb_id);

    let first = client.add_series(&req).await.unwrap();
    assert!(matches!(first, AddOutcome::Created(ref s) if s.id == 21));

    let second = client.add_series(&req).await.unwrap();
    assert!(matches!(second, AddOutcome::AlreadyExisted(ref s) if s.id == 21));

    let third = client.add_series(&req).await.unwrap();
    assert!(matches!(third, AddOutcome::AlreadyExisted(ref s) if s.id == 21));
}

/// Concurrent regression test for the cross-instance 409 race.
///
/// Idempotent semantics demand: N concurrent `add_series` calls for
/// the same series → exactly 1 Created + N-1 AlreadyExisted, 0 errors.
/// Pre-fix, this asserted 1 ok / 3 err — the silent-data-loss bug.
#[tokio::test]
async fn concurrent_add_series_one_created_n_minus_1_already_existed() {
    let server = MockServer::start().await;
    let tvdb_id: u32 = 468_226;
    let seeded = json!({
        "id": 21,
        "title": "Seeded Series",
        "tvdbId": tvdb_id,
        "qualityProfileId": 1,
        "seasonFolder": true,
        "monitored": true,
        "seasons": []
    });

    let counter = Arc::new(AtomicU32::new(0));
    Mock::given(method("POST"))
        .and(path("/api/v3/series"))
        .respond_with(OneCreatedThenConflictResponder {
            counter: Arc::clone(&counter),
            created_body: seeded.clone(),
            conflict_body: SONARR_TITLE_SLUG_409_BODY,
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", tvdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([seeded])))
        .mount(&server)
        .await;

    let client = Arc::new(SonarrClient::new(test_http(
        &server,
        RetryPolicy::no_retry(),
    )));
    let req = add_series_request(tvdb_id);

    // 4 concurrent adds. Whichever Tokio task lands AtomicU32==0 is the
    // Created winner; the other three race onto 409 and recover via
    // get_series_by_tvdb_id → AlreadyExisted.
    let (r1, r2, r3, r4) = tokio::join!(
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_series(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_series(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_series(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_series(&q).await }
        },
    );

    let results = [r1, r2, r3, r4];
    let mut created = 0u32;
    let mut already = 0u32;
    let mut errors = 0u32;
    for r in &results {
        match r {
            Ok(AddOutcome::Created(_)) => created += 1,
            Ok(AddOutcome::AlreadyExisted(_)) => already += 1,
            Err(_) => errors += 1,
        }
    }
    assert_eq!(created, 1, "exactly one caller wins the create");
    assert_eq!(already, 3, "three losers absorb 409 as AlreadyExisted");
    assert_eq!(errors, 0, "no caller surfaces an error to the handler");
}

#[tokio::test]
async fn add_series_409_with_unrelated_constraint_propagates_error() {
    let server = MockServer::start().await;
    let tvdb_id: u32 = 999_999;

    Mock::given(method("POST"))
        .and(path("/api/v3/series"))
        .respond_with(ResponseTemplate::new(409).set_body_string(SONARR_TITLE_SLUG_409_BODY))
        .mount(&server)
        .await;
    // GET-by-tvdbId returns empty → the 409 was for a *different*
    // unique constraint (e.g. true title-slug collision between two
    // genuinely different shows). Propagate as Conflict.
    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .and(query_param("tvdbId", tvdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = SonarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let err = client
        .add_series(&add_series_request(tvdb_id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ArrError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );
    assert!(!err.is_transient());
}

// ----- Radarr / add_movie (mirror set) -----

#[tokio::test]
async fn add_movie_first_call_returns_created_subsequent_409_become_already_existed() {
    let server = MockServer::start().await;
    let tmdb_id: u32 = 27_205;
    let seeded = json!({
        "id": 17,
        "title": "Seeded Movie",
        "year": 2010,
        "tmdbId": tmdb_id,
        "qualityProfileId": 1,
        "hasFile": false
    });

    let counter = Arc::new(AtomicU32::new(0));
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .respond_with(OneCreatedThenConflictResponder {
            counter: Arc::clone(&counter),
            created_body: seeded.clone(),
            conflict_body: RADARR_TMDB_409_BODY,
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", tmdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([seeded])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let req = add_movie_request(tmdb_id);

    let first = client.add_movie(&req).await.unwrap();
    assert!(matches!(first, AddOutcome::Created(ref m) if m.id == 17));

    let second = client.add_movie(&req).await.unwrap();
    assert!(matches!(second, AddOutcome::AlreadyExisted(ref m) if m.id == 17));

    let third = client.add_movie(&req).await.unwrap();
    assert!(matches!(third, AddOutcome::AlreadyExisted(ref m) if m.id == 17));
}

#[tokio::test]
async fn concurrent_add_movie_one_created_n_minus_1_already_existed() {
    let server = MockServer::start().await;
    let tmdb_id: u32 = 27_205;
    let seeded = json!({
        "id": 17,
        "title": "Seeded Movie",
        "year": 2010,
        "tmdbId": tmdb_id,
        "qualityProfileId": 1,
        "hasFile": false
    });

    let counter = Arc::new(AtomicU32::new(0));
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .respond_with(OneCreatedThenConflictResponder {
            counter: Arc::clone(&counter),
            created_body: seeded.clone(),
            conflict_body: RADARR_TMDB_409_BODY,
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", tmdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([seeded])))
        .mount(&server)
        .await;

    let client = Arc::new(RadarrClient::new(test_http(
        &server,
        RetryPolicy::no_retry(),
    )));
    let req = add_movie_request(tmdb_id);

    let (r1, r2, r3, r4) = tokio::join!(
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_movie(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_movie(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_movie(&q).await }
        },
        {
            let c = Arc::clone(&client);
            let q = req.clone();
            async move { c.add_movie(&q).await }
        },
    );

    let results = [r1, r2, r3, r4];
    let mut created = 0u32;
    let mut already = 0u32;
    let mut errors = 0u32;
    for r in &results {
        match r {
            Ok(AddOutcome::Created(_)) => created += 1,
            Ok(AddOutcome::AlreadyExisted(_)) => already += 1,
            Err(_) => errors += 1,
        }
    }
    assert_eq!(created, 1);
    assert_eq!(already, 3);
    assert_eq!(errors, 0);
}

#[tokio::test]
async fn add_movie_409_with_unrelated_constraint_propagates_error() {
    let server = MockServer::start().await;
    let tmdb_id: u32 = 999_999;

    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(409).set_body_string(RADARR_TMDB_409_BODY))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/movie"))
        .and(query_param("tmdbId", tmdb_id.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let client = RadarrClient::new(test_http(&server, RetryPolicy::no_retry()));
    let err = client
        .add_movie(&add_movie_request(tmdb_id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ArrError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );
    assert!(!err.is_transient());
}
