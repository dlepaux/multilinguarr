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

/// Outcome of an idempotent `add_series` / `add_movie` call.
///
/// Both variants are success: the caller wanted the resource to exist
/// in the target instance, and after the call, it does. The split
/// exists so the handler can log and meter the two cases separately
/// (cross-instance race winner vs loser).
///
/// Wrapping the wire-level result in this enum lets the client layer
/// absorb a 409 race loss without surfacing it as an error — the loser
/// of a race performs a follow-up GET-by-external-id and returns the
/// existing record as `AlreadyExisted`. If the GET also returns nothing
/// (true title-slug collision between two genuinely different shows /
/// movies), the original `ArrError::Conflict` is propagated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome<T> {
    /// The POST succeeded — this caller created the resource.
    Created(T),
    /// A prior concurrent caller created the resource; the lookup-by-
    /// external-id confirmed it exists. Carries the existing record.
    AlreadyExisted(T),
}

impl<T> AddOutcome<T> {
    /// Borrow the inner record regardless of which branch fired.
    pub fn record(&self) -> &T {
        match self {
            Self::Created(r) | Self::AlreadyExisted(r) => r,
        }
    }

    /// Move the inner record out, discarding the variant tag.
    pub fn into_record(self) -> T {
        match self {
            Self::Created(r) | Self::AlreadyExisted(r) => r,
        }
    }
}
