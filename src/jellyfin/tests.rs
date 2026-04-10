//! Jellyfin integration tests.
//!
//! The debounce pump is exercised directly with an atomic counter —
//! no HTTP involved. The HTTP client is exercised against a wiremock
//! server. End-to-end (service + HTTP) is covered by a small
//! integration-style test that spins up wiremock and a service.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::client::JellyfinClient;
use super::service::{debounce_pump, JellyfinService, MediaServer, NoopMediaServer};

// ---------- helpers ----------

async fn wait_until(mut probe: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("wait_until timed out");
}

// ---------- NoopMediaServer ----------

#[tokio::test]
async fn noop_media_server_refresh_is_a_no_op() {
    let server = NoopMediaServer;
    // Just make sure calling it is harmless. The future resolves
    // immediately with no side effects.
    server.refresh().await;
    server.refresh().await;
    server.refresh().await;
}

// ---------- debounce_pump ----------

#[tokio::test]
async fn debounce_pump_collapses_a_burst_into_one_call() {
    let (tx, rx) = mpsc::channel::<()>(64);
    let calls = Arc::new(AtomicU32::new(0));
    let calls_inner = calls.clone();
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();

    let window = Duration::from_millis(50);
    let handle = tokio::spawn(debounce_pump(
        rx,
        window,
        move || {
            let calls = calls_inner.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        },
        cancel_inner,
    ));

    // Burst of 20 triggers in rapid succession — all must collapse
    // into a single on_fire call after the quiet window elapses.
    for _ in 0..20 {
        tx.send(()).await.unwrap();
    }

    wait_until(|| calls.load(Ordering::SeqCst) == 1).await;
    // Give the pump a moment past the window; it must NOT fire a
    // second time off the burst.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn debounce_pump_resets_on_every_trigger() {
    let (tx, rx) = mpsc::channel::<()>(64);
    let calls = Arc::new(AtomicU32::new(0));
    let calls_inner = calls.clone();
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();

    let window = Duration::from_millis(60);
    let handle = tokio::spawn(debounce_pump(
        rx,
        window,
        move || {
            let calls = calls_inner.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        },
        cancel_inner,
    ));

    // Send a trigger, wait half the window, send again. The second
    // trigger MUST reset the sleep — meaning on_fire should not fire
    // at the first half-window mark.
    tx.send(()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    tx.send(()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    // Total elapsed: 60ms after the first trigger, but only 30ms
    // since the reset. Must not have fired yet.
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Now let the full window elapse past the last trigger.
    wait_until(|| calls.load(Ordering::SeqCst) == 1).await;

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn debounce_pump_fires_multiple_times_for_separate_bursts() {
    let (tx, rx) = mpsc::channel::<()>(64);
    let calls = Arc::new(AtomicU32::new(0));
    let calls_inner = calls.clone();
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();

    let window = Duration::from_millis(40);
    let handle = tokio::spawn(debounce_pump(
        rx,
        window,
        move || {
            let calls = calls_inner.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        },
        cancel_inner,
    ));

    // First burst.
    tx.send(()).await.unwrap();
    wait_until(|| calls.load(Ordering::SeqCst) == 1).await;
    // Quiet period longer than the window.
    tokio::time::sleep(Duration::from_millis(60)).await;
    // Second burst.
    tx.send(()).await.unwrap();
    wait_until(|| calls.load(Ordering::SeqCst) == 2).await;

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn debounce_pump_exits_on_cancel_without_firing_pending_work() {
    let (tx, rx) = mpsc::channel::<()>(64);
    let calls = Arc::new(AtomicU32::new(0));
    let calls_inner = calls.clone();
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();

    let handle = tokio::spawn(debounce_pump(
        rx,
        Duration::from_secs(10),
        move || {
            let calls = calls_inner.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        },
        cancel_inner,
    ));

    tx.send(()).await.unwrap();
    // Cancel before the 10-second window elapses — pending trigger
    // should be dropped, on_fire never called.
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();
    handle.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

// ---------- JellyfinClient (HTTP) ----------

#[tokio::test]
async fn jellyfin_client_refresh_hits_library_refresh_with_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .and(header("X-Emby-Token", "my-key"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = JellyfinClient::new(
        &format!("{}/", server.uri()),
        "my-key",
        Duration::from_secs(5),
    )
    .unwrap();
    client.refresh_all_libraries().await.unwrap();
}

#[tokio::test]
async fn jellyfin_client_surfaces_non_2xx_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client =
        JellyfinClient::new(&format!("{}/", server.uri()), "k", Duration::from_secs(5)).unwrap();
    let err = client.refresh_all_libraries().await.unwrap_err();
    assert!(matches!(
        err,
        super::error::JellyfinError::Status { status: 500 }
    ));
}

#[tokio::test]
async fn jellyfin_client_rejects_invalid_url() {
    let err = JellyfinClient::new("not a url", "k", Duration::from_secs(5)).unwrap_err();
    assert!(matches!(err, super::error::JellyfinError::InvalidUrl(_)));
}

// ---------- JellyfinService (service + HTTP, end-to-end) ----------

#[tokio::test]
async fn jellyfin_service_debounces_real_http_refreshes() {
    let server = MockServer::start().await;
    // Only expect a single POST — the burst must collapse.
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client =
        JellyfinClient::new(&format!("{}/", server.uri()), "k", Duration::from_secs(5)).unwrap();

    let cancel = CancellationToken::new();
    let (service, handle) =
        JellyfinService::spawn_with_handle(client, Duration::from_millis(30), cancel.clone());

    // Burst of refreshes — enqueues are fire-and-forget.
    for _ in 0..10 {
        service.refresh().await;
    }

    // Let the debounce window elapse and the refresh land.
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancel.cancel();
    handle.await.unwrap();

    // `server` verifies the `.expect(1)` on drop — if the service
    // failed to collapse or fired zero times, that assertion fires.
    drop(server);
}

#[tokio::test]
async fn jellyfin_service_refresh_does_not_fail_on_http_error() {
    // The service swallows HTTP failures (warn + move on). Verify
    // that a 500 response does not propagate.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client =
        JellyfinClient::new(&format!("{}/", server.uri()), "k", Duration::from_secs(5)).unwrap();
    let cancel = CancellationToken::new();
    let (service, handle) =
        JellyfinService::spawn_with_handle(client, Duration::from_millis(20), cancel.clone());

    service.refresh().await;
    // Second refresh after the first fails — the pump must still be
    // alive and ready.
    tokio::time::sleep(Duration::from_millis(60)).await;
    service.refresh().await;
    tokio::time::sleep(Duration::from_millis(60)).await;

    cancel.cancel();
    handle.await.unwrap();
}
