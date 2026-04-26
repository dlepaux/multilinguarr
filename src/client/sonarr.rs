//! Sonarr v3 API client.

use serde::{Deserialize, Serialize};

use super::common::{AddOutcome, CustomFormat, Language, MediaQuality, QualityProfile, RootFolder};
use super::error::ArrError;
use super::http::HttpCore;

const PATH_SERIES: &str = "/api/v3/series";
const PATH_EPISODE_FILE: &str = "/api/v3/episodefile";
const PATH_QUALITY_PROFILE: &str = "/api/v3/qualityprofile";
const PATH_ROOT_FOLDER: &str = "/api/v3/rootfolder";

/// An episode file — used for audio language detection.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeFile {
    pub id: u32,
    pub series_id: u32,
    #[serde(default)]
    pub season_number: Option<u32>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub quality: MediaQuality,
    #[serde(default)]
    pub languages: Vec<Language>,
    #[serde(default)]
    pub custom_formats: Vec<CustomFormat>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonInfo {
    pub season_number: u32,
    pub monitored: bool,
}

/// A full series record as returned by Sonarr.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrSeries {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub year: Option<u32>,
    pub tvdb_id: u32,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub root_folder_path: Option<String>,
    pub quality_profile_id: u32,
    pub season_folder: bool,
    pub monitored: bool,
    #[serde(default)]
    pub seasons: Vec<SeasonInfo>,
}

/// Payload for `POST /api/v3/series`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSeriesRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    pub tvdb_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: String,
    pub season_folder: bool,
    pub monitored: bool,
    pub seasons: Vec<SeasonInfo>,
    pub add_options: AddSeriesOptions,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSeriesOptions {
    pub search_for_missing_episodes: bool,
}

#[derive(Debug, Clone)]
pub struct SonarrClient {
    http: HttpCore,
}

impl SonarrClient {
    #[must_use]
    pub fn new(http: HttpCore) -> Self {
        Self { http }
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        self.http.instance()
    }

    /// Fetch all series from the Sonarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn list_series(&self) -> Result<Vec<SonarrSeries>, ArrError> {
        self.http.get_json(PATH_SERIES).await
    }

    /// Fetch a series by TVDB id. Returns `Ok(None)` if no match found.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn get_series_by_tvdb_id(
        &self,
        tvdb_id: u32,
    ) -> Result<Option<SonarrSeries>, ArrError> {
        let endpoint = format!("{PATH_SERIES}?tvdbId={tvdb_id}");
        let series: Vec<SonarrSeries> = self.http.get_json(&endpoint).await?;
        Ok(series.into_iter().next())
    }

    /// Add a series. 409 on POST is resolved via `get_series_by_tvdb_id`
    /// and returned as `AddOutcome::AlreadyExisted`; if the lookup
    /// finds nothing the 409 propagates (different unique constraint).
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn add_series(
        &self,
        req: &AddSeriesRequest,
    ) -> Result<AddOutcome<SonarrSeries>, ArrError> {
        match self
            .http
            .post_json::<_, SonarrSeries>(PATH_SERIES, req)
            .await
        {
            Ok(series) => Ok(AddOutcome::Created(series)),
            Err(ArrError::Conflict {
                instance,
                endpoint,
                body,
            }) => match self.get_series_by_tvdb_id(req.tvdb_id).await? {
                Some(existing) => Ok(AddOutcome::AlreadyExisted(existing)),
                None => Err(ArrError::Conflict {
                    instance,
                    endpoint,
                    body,
                }),
            },
            Err(err) => Err(err),
        }
    }

    /// Delete a series by its Sonarr internal id.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network or HTTP failure.
    pub async fn delete_series(&self, id: u32, delete_files: bool) -> Result<(), ArrError> {
        let endpoint = format!("{PATH_SERIES}/{id}?deleteFiles={delete_files}");
        self.http.delete(&endpoint).await
    }

    /// Fetch all episode files for a given series.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn list_episode_files(&self, series_id: u32) -> Result<Vec<EpisodeFile>, ArrError> {
        let endpoint = format!("{PATH_EPISODE_FILE}?seriesId={series_id}");
        self.http.get_json(&endpoint).await
    }

    /// Fetch available quality profiles.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn quality_profiles(&self) -> Result<Vec<QualityProfile>, ArrError> {
        self.http.get_json(PATH_QUALITY_PROFILE).await
    }

    /// Fetch configured root folders.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn root_folders(&self) -> Result<Vec<RootFolder>, ArrError> {
        self.http.get_json(PATH_ROOT_FOLDER).await
    }
}
