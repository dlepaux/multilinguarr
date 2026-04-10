//! Radarr v3 API client.

use serde::{Deserialize, Serialize};

use super::common::{CustomFormat, Language, MediaQuality, QualityProfile, RootFolder};
use super::error::ArrError;
use super::http::HttpCore;

const PATH_MOVIE: &str = "/api/v3/movie";
const PATH_QUALITY_PROFILE: &str = "/api/v3/qualityprofile";
const PATH_ROOT_FOLDER: &str = "/api/v3/rootfolder";

/// Movie file metadata — used for audio language detection.
#[derive(Debug, Clone, Deserialize)]
pub struct MovieFile {
    pub id: u32,
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

/// A full movie record as returned by Radarr.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarrMovie {
    pub id: u32,
    pub title: String,
    pub year: u32,
    pub tmdb_id: u32,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub root_folder_path: Option<String>,
    pub quality_profile_id: u32,
    pub has_file: bool,
    #[serde(default)]
    pub movie_file: Option<MovieFile>,
}

/// Payload for `POST /api/v3/movie`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMovieRequest {
    pub title: String,
    pub year: u32,
    pub tmdb_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: String,
    pub monitored: bool,
    pub add_options: AddMovieOptions,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMovieOptions {
    pub search_for_movie: bool,
}

/// Typed Radarr client. Cheap to clone — wraps a cloneable `HttpCore`.
#[derive(Debug, Clone)]
pub struct RadarrClient {
    http: HttpCore,
}

impl RadarrClient {
    #[must_use]
    pub fn new(http: HttpCore) -> Self {
        Self { http }
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        self.http.instance()
    }

    /// Fetch all movies from the Radarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn list_movies(&self) -> Result<Vec<RadarrMovie>, ArrError> {
        self.http.get_json(PATH_MOVIE).await
    }

    /// Fetch a movie by TMDB id. Radarr returns an array; we pick the
    /// first match and return `Ok(None)` if the array is empty. Real
    /// 404s (unknown endpoint) become `ArrError::NotFound`.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn get_movie_by_tmdb_id(
        &self,
        tmdb_id: u32,
    ) -> Result<Option<RadarrMovie>, ArrError> {
        let endpoint = format!("{PATH_MOVIE}?tmdbId={tmdb_id}");
        let movies: Vec<RadarrMovie> = self.http.get_json(&endpoint).await?;
        Ok(movies.into_iter().next())
    }

    /// Add a movie to the Radarr instance.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network, HTTP, or deserialization failure.
    pub async fn add_movie(&self, req: &AddMovieRequest) -> Result<RadarrMovie, ArrError> {
        self.http.post_json(PATH_MOVIE, req).await
    }

    /// Delete a movie by its Radarr internal id.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network or HTTP failure.
    pub async fn delete_movie(&self, id: u32, delete_files: bool) -> Result<(), ArrError> {
        let endpoint = format!("{PATH_MOVIE}/{id}?deleteFiles={delete_files}");
        self.http.delete(&endpoint).await
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
