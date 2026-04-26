//! Radarr / Sonarr webhook payload types.
//!
//! Two tagged enums (one per arr engine) capture the subset of v3
//! webhook events the service consumes. Anything else — `Test`,
//! `Health`, `ApplicationUpdate`, future event types — falls through
//! to the `Unknown` variant via `#[serde(other)]`, so the HTTP layer
//! can log and 200 them rather than 400.
//!
//! Field shapes are deliberately permissive (every non-essential field
//! is `Option<_>` with `#[serde(default)]`) because Radarr and Sonarr
//! evolve their payloads between minor versions. We only care about
//! the identifiers we need to fetch the file from the arr API later.

use serde::{Deserialize, Serialize};

use crate::queue::JobPayload;

// ---------------------------------------------------------------------
// Radarr
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "eventType")]
pub enum RadarrEvent {
    Test(RadarrTest),
    #[serde(alias = "MovieFileImported", alias = "MovieFileUpgrade")]
    Download(RadarrDownload),
    MovieDelete(RadarrMovieDelete),
    MovieFileDelete(RadarrMovieFileDelete),
    // Documented Radarr events we deliberately do not act on. Named so
    // they deserialize cleanly and stay out of the `Unknown` bucket —
    // the unknown-events counter is then a real signal of arr-version
    // drift, not background noise.
    Grab,
    Rename,
    MovieAdded,
    MovieFileRenamed,
    Health,
    HealthRestored,
    ApplicationUpdate,
    ManualInteractionRequired,
    /// Last-resort catch-all for event types not enumerated above.
    /// Logged with the raw `eventType` and counted via
    /// `multilinguarr_webhook_unknown_event_total`.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrTest {
    pub instance_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrDownload {
    pub movie: Option<RadarrMovieRef>,
    pub movie_file: Option<RadarrMovieFileRef>,
    pub is_upgrade: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrMovieDelete {
    pub movie: Option<RadarrMovieRef>,
    pub deleted_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrMovieFileDelete {
    pub movie: Option<RadarrMovieRef>,
    pub movie_file: Option<RadarrMovieFileRef>,
    pub delete_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrMovieRef {
    pub id: u32,
    pub title: String,
    pub year: u32,
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub folder_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RadarrMovieFileRef {
    pub id: u32,
    pub relative_path: Option<String>,
    pub path: Option<String>,
    pub quality: Option<String>,
}

// ---------------------------------------------------------------------
// Sonarr
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "eventType")]
pub enum SonarrEvent {
    Test(SonarrTest),
    #[serde(alias = "EpisodeFileImported", alias = "EpisodeFileUpgrade")]
    Download(SonarrDownload),
    SeriesDelete(SonarrSeriesDelete),
    EpisodeFileDelete(SonarrEpisodeFileDelete),
    // See `RadarrEvent` — same rationale.
    Grab,
    Rename,
    SeriesAdd,
    Health,
    HealthRestored,
    ApplicationUpdate,
    ManualInteractionRequired,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrTest {
    pub instance_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrDownload {
    pub series: Option<SonarrSeriesRef>,
    pub episodes: Vec<SonarrEpisodeRef>,
    pub episode_file: Option<SonarrEpisodeFileRef>,
    pub is_upgrade: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrSeriesDelete {
    pub series: Option<SonarrSeriesRef>,
    pub deleted_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrEpisodeFileDelete {
    pub series: Option<SonarrSeriesRef>,
    pub episodes: Vec<SonarrEpisodeRef>,
    pub episode_file: Option<SonarrEpisodeFileRef>,
    pub delete_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrSeriesRef {
    pub id: u32,
    pub title: String,
    pub tvdb_id: u32,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrEpisodeRef {
    pub id: u32,
    pub episode_number: u32,
    pub season_number: u32,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SonarrEpisodeFileRef {
    pub id: u32,
    pub relative_path: Option<String>,
    pub path: Option<String>,
    pub quality: Option<String>,
}

// ---------------------------------------------------------------------
// Job payload wrappers
// ---------------------------------------------------------------------

/// What gets persisted in the queue when a Radarr webhook arrives.
/// Carries the instance name so the worker (story 08) knows which
/// configured instance the event came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarrWebhookJob {
    pub instance: String,
    pub event: RadarrEvent,
}

impl JobPayload for RadarrWebhookJob {
    const KIND: &'static str = "radarr_webhook";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonarrWebhookJob {
    pub instance: String,
    pub event: SonarrEvent,
}

impl JobPayload for SonarrWebhookJob {
    const KIND: &'static str = "sonarr_webhook";
}
