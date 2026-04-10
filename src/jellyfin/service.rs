//! `MediaServer` trait + concrete implementations.
//!
//! The trait is the abstraction story 08 depends on (held as
//! `Arc<dyn MediaServer>` inside the `HandlerRegistry`). Two
//! implementations live here:
//!
//! - [`NoopMediaServer`]: the default when Jellyfin is not
//!   configured; every `refresh()` call is a no-op. Also used by
//!   unit tests.
//! - [`JellyfinService`]: debounced wrapper around a
//!   [`JellyfinClient`]. Multiple `refresh()` calls within a short
//!   window collapse into a single HTTP call.
//!
//! Forward-compatibility: story 15 adds Plex. A `PlexService` will
//! be a third implementation of the same trait — no refactor of the
//! caller side is needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::client::JellyfinClient;

/// Boxed future returned by [`MediaServer::refresh`]. Using a boxed
/// future keeps the trait `dyn`-compatible.
pub type RefreshFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A media server whose library can be refreshed. Implementations
/// debounce internally — the caller fires-and-forgets.
pub trait MediaServer: std::fmt::Debug + Send + Sync + 'static {
    /// Schedule a library refresh. Returns immediately; the actual
    /// refresh happens asynchronously on the implementation's own
    /// schedule (e.g. after a debounce window).
    fn refresh(&self) -> RefreshFuture<'_>;
}

// ---------------------------------------------------------------------
// Noop
// ---------------------------------------------------------------------

/// No-op media server — used when Jellyfin is not configured in the
/// current environment, and in every unit test that does not care
/// about actual refresh side effects.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMediaServer;

impl MediaServer for NoopMediaServer {
    fn refresh(&self) -> RefreshFuture<'_> {
        Box::pin(async {})
    }
}

// ---------------------------------------------------------------------
// JellyfinService
// ---------------------------------------------------------------------

/// Debounced Jellyfin refresh service.
///
/// Holds an `mpsc::Sender<()>` connected to a background task running
/// [`debounce_pump`]. Every `refresh()` call enqueues one signal; the
/// pump drains signals and fires the actual HTTP call after the
/// quiet window elapses. A burst of 50 calls over a few hundred
/// milliseconds collapses into a single refresh.
#[derive(Debug, Clone)]
pub struct JellyfinService {
    trigger: mpsc::Sender<()>,
}

impl JellyfinService {
    /// Spawn the debouncer task and return a `JellyfinService` that
    /// feeds into it. The task runs until `cancel` fires (or the
    /// returned service is dropped and all its clones are dropped).
    #[must_use]
    pub fn spawn(client: JellyfinClient, window: Duration, cancel: CancellationToken) -> Self {
        let (service, _handle) = Self::spawn_with_handle(client, window, cancel);
        service
    }

    /// Variant that returns the background task handle too, so
    /// callers can await graceful shutdown during teardown.
    #[must_use]
    pub fn spawn_with_handle(
        client: JellyfinClient,
        window: Duration,
        cancel: CancellationToken,
    ) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(32);
        let client = Arc::new(client);
        let handle = tokio::spawn(debounce_pump(
            rx,
            window,
            move || {
                let client = client.clone();
                async move {
                    match client.refresh_all_libraries().await {
                        Ok(()) => info!("jellyfin library refresh ok"),
                        Err(err) => warn!(error = %err, "jellyfin library refresh failed"),
                    }
                }
            },
            cancel,
        ));
        (Self { trigger: tx }, handle)
    }
}

impl MediaServer for JellyfinService {
    fn refresh(&self) -> RefreshFuture<'_> {
        let tx = self.trigger.clone();
        Box::pin(async move {
            // `try_send` drops silently on a full channel — by design,
            // because the channel already holds a pending refresh and
            // any additional trigger would coalesce anyway.
            let _ = tx.try_send(());
        })
    }
}

// ---------------------------------------------------------------------
// Debounce pump
// ---------------------------------------------------------------------

/// Generic debouncer. Waits for a trigger on `rx`, then keeps
/// resetting a `window` sleep until the channel stays quiet for the
/// full duration. At that point the `on_fire` closure is awaited,
/// then the pump returns to waiting for the next trigger.
///
/// Cancellation aware — `cancel` fires at any point and the pump
/// returns immediately. Any pending refresh is discarded.
///
/// Generic over the closure so unit tests can substitute an
/// `AtomicU32` increment without going through HTTP.
pub(super) async fn debounce_pump<F, Fut>(
    mut rx: mpsc::Receiver<()>,
    window: Duration,
    on_fire: F,
    cancel: CancellationToken,
) where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    loop {
        // Outer wait: block until either we are cancelled or a trigger
        // arrives.
        let first = tokio::select! {
            () = cancel.cancelled() => return,
            maybe = rx.recv() => maybe,
        };
        if first.is_none() {
            return;
        }

        // A trigger arrived. Start the debounce window and keep
        // resetting it on every additional trigger.
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                maybe = rx.recv() => {
                    if maybe.is_none() {
                        return;
                    }
                    // Additional trigger — reset the window by looping.
                }
                () = tokio::time::sleep(window) => {
                    on_fire().await;
                    break;
                }
            }
        }
    }
}
