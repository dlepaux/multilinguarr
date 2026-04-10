//! HTTP webhook server + event routing.
//!
//! Receives Radarr/Sonarr webhook POSTs, decodes them into typed
//! events, and enqueues a job for the worker pool to handle later.
//! See `plan/active/multilinguarr-rust-rewrite/07-webhook-server-event-routing.md`.

mod error;
mod events;
pub mod server;

#[cfg(test)]
mod tests;

pub use error::WebhookError;
pub use events::{
    RadarrDownload, RadarrEvent, RadarrMovieDelete, RadarrMovieFileDelete, RadarrMovieFileRef,
    RadarrMovieRef, RadarrTest, RadarrWebhookJob, SonarrDownload, SonarrEpisodeFileDelete,
    SonarrEpisodeFileRef, SonarrEpisodeRef, SonarrEvent, SonarrSeriesDelete, SonarrSeriesRef,
    SonarrTest, SonarrWebhookJob,
};
pub use server::{router, serve_http, AppState};
