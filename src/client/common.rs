//! API shapes shared by Radarr and Sonarr.
//!
//! Only the fields the service actually reads are modelled — everything
//! else is silently ignored by serde. We deliberately do **not** use
//! `deny_unknown_fields` here: the arr APIs add fields between minor
//! versions and we should not crash on that.

use serde::{Deserialize, Serialize};

/// A Radarr/Sonarr quality profile. Used when selecting which profile
/// to assign on `addMovie` / `addSeries`.
#[derive(Debug, Clone, Deserialize)]
pub struct QualityProfile {
    pub id: u32,
    pub name: String,
}

/// A root folder path configured on an arr instance.
#[derive(Debug, Clone, Deserialize)]
pub struct RootFolder {
    pub path: String,
}

/// A language entry as returned by arr media file metadata. The numeric
/// `id` is the key that ties back to config `radarr_id` / `sonarr_id`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Language {
    pub id: u32,
    pub name: String,
}

/// Custom format attached to a media file.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomFormat {
    pub id: u32,
    pub name: String,
}

/// Quality block present on movie/episode file records.
#[derive(Debug, Clone, Deserialize)]
pub struct MediaQuality {
    pub quality: QualityDetail,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QualityDetail {
    pub id: u32,
    pub name: String,
}

/// Idempotent outcome of an `add_*` call. Both variants are success;
/// `AlreadyExisted` carries the record found by a follow-up
/// lookup-by-external-id after a 409 race loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome<T> {
    Created(T),
    AlreadyExisted(T),
}

impl<T> AddOutcome<T> {
    pub fn record(&self) -> &T {
        match self {
            Self::Created(r) | Self::AlreadyExisted(r) => r,
        }
    }

    pub fn into_record(self) -> T {
        match self {
            Self::Created(r) | Self::AlreadyExisted(r) => r,
        }
    }
}
