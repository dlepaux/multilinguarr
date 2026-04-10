//! Jellyfin integration — HTTP client + debounced `MediaServer`.
//!
//! The [`MediaServer`] trait is the abstraction the handler layer
//! depends on (`Arc<dyn MediaServer>` lives inside the
//! `HandlerRegistry`). Production deployments wire up a
//! [`JellyfinService`] constructed from config; deployments without
//! Jellyfin configured use [`NoopMediaServer`]. Story 15 will add a
//! `PlexService` as a third implementation.

mod client;
mod error;
mod service;

#[cfg(test)]
mod tests;

pub use client::JellyfinClient;
pub use error::JellyfinError;
pub use service::{JellyfinService, MediaServer, NoopMediaServer, RefreshFuture};
