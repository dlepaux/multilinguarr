//! Thin arr API helper for E2E test setup: root folders, quality
//! relaxation, movie/series lookup + add + delete. The handler uses
//! ffprobe directly (file path from webhook) — no arr rescan needed.

use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};

use super::containers::ArrInstance;
use super::Result;

/// Extra operations the test harness needs that the production
/// `ArrClient` does not expose — mostly "make the arr instance do
/// something so we can observe the side effect".
#[derive(Debug, Clone)]
pub struct ArrHarnessClient {
    http: Client,
    pub base_url: String,
    pub api_key: String,
    pub name: String,
}

impl ArrHarnessClient {
    pub fn new(instance: &ArrInstance) -> Result<Self> {
        let http = Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self {
            http,
            base_url: instance.base_url.clone(),
            api_key: instance.api_key.clone(),
            name: instance.name.clone(),
        })
    }

    fn url(&self, endpoint: &str) -> String {
        format!("{}{}", self.base_url, endpoint)
    }

    async fn get_json(&self, endpoint: &str) -> Result<Value> {
        let resp = self
            .http
            .get(self.url(endpoint))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn post_json(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(self.url(endpoint))
            .header("X-Api-Key", &self.api_key)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("{} POST {endpoint} -> {status}: {text}", self.name).into());
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn put_json(&self, endpoint: &str, body: &Value) -> Result<()> {
        let resp = self
            .http
            .put(self.url(endpoint))
            .header("X-Api-Key", &self.api_key)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("{} PUT {endpoint} -> {status}: {text}", self.name).into());
        }
        Ok(())
    }

    async fn delete(&self, endpoint: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(endpoint))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(format!("{} DELETE {endpoint} -> {}", self.name, resp.status()).into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Root folders / quality definitions — seed bootstrap
    // -----------------------------------------------------------------

    pub async fn add_root_folder(&self, path: &Path) -> Result<()> {
        // Idempotent: only add when absent.
        let existing = self.get_json("/api/v3/rootfolder").await?;
        if let Some(arr) = existing.as_array() {
            for f in arr {
                if f.get("path").and_then(Value::as_str) == Some(&path.display().to_string()) {
                    return Ok(());
                }
            }
        }
        let body = json!({ "path": path.display().to_string() });
        self.post_json("/api/v3/rootfolder", &body).await?;
        Ok(())
    }

    /// Lower every quality definition's `minSize` to 0 so the
    /// small test fixtures import without being rejected as samples.
    pub async fn relax_quality_definitions(&self) -> Result<()> {
        let defs = self.get_json("/api/v3/qualitydefinition").await?;
        let Value::Array(items) = defs else {
            return Ok(());
        };
        let relaxed: Vec<Value> = items
            .into_iter()
            .map(|mut v| {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("minSize".to_owned(), json!(0));
                }
                v
            })
            .collect();
        // Radarr accepts the whole array on /qualitydefinition/update.
        self.put_json("/api/v3/qualitydefinition/update", &Value::Array(relaxed))
            .await?;
        Ok(())
    }

    pub async fn relax_media_management(&self) -> Result<()> {
        let mut cfg = self.get_json("/api/v3/config/mediamanagement").await?;
        if let Some(obj) = cfg.as_object_mut() {
            // Radarr's validator requires >= 100 MB here; 100 is
            // the safest non-zero minimum that passes on a fresh
            // install.
            obj.insert("minimumFreeSpaceWhenImporting".to_owned(), json!(100));
        }
        self.put_json("/api/v3/config/mediamanagement", &cfg)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Movies
    // -----------------------------------------------------------------

    pub async fn find_movie_by_tmdb(&self, tmdb_id: u32) -> Result<Option<Value>> {
        let list = self
            .get_json(&format!("/api/v3/movie?tmdbId={tmdb_id}"))
            .await?;
        Ok(list.as_array().and_then(|a| a.first().cloned()))
    }

    /// Look up a movie by search term (e.g. `"Big Buck Bunny"`).
    /// Returns the first result, which is the full payload Radarr
    /// expects for a subsequent `add` call.
    pub async fn movie_lookup(&self, term: &str) -> Result<Value> {
        let url = format!("/api/v3/movie/lookup?term={}", urlencoding(term));
        let result = self.get_json(&url).await?;
        result
            .as_array()
            .and_then(|a| a.first().cloned())
            .ok_or_else(|| format!("movie lookup for `{term}` returned no results").into())
    }

    /// Add a movie using the metadata from a prior `movie_lookup`.
    /// Augments the lookup payload with the fields Radarr requires on
    /// `POST /api/v3/movie`.
    pub async fn add_movie_from_lookup(&self, lookup: &Value, root_folder: &Path) -> Result<Value> {
        let mut body = lookup.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("qualityProfileId".to_owned(), json!(1));
            obj.insert(
                "rootFolderPath".to_owned(),
                json!(root_folder.display().to_string()),
            );
            obj.insert("monitored".to_owned(), json!(true));
            obj.insert(
                "addOptions".to_owned(),
                json!({ "searchForMovie": false, "monitor": "none" }),
            );
        }
        self.post_json("/api/v3/movie", &body).await
    }

    pub async fn delete_movie(&self, id: u32, delete_files: bool) -> Result<()> {
        self.delete(&format!("/api/v3/movie/{id}?deleteFiles={delete_files}"))
            .await
    }

    // -----------------------------------------------------------------
    // Series
    // -----------------------------------------------------------------

    pub async fn find_series_by_tvdb(&self, tvdb_id: u32) -> Result<Option<Value>> {
        let list = self
            .get_json(&format!("/api/v3/series?tvdbId={tvdb_id}"))
            .await?;
        Ok(list.as_array().and_then(|a| a.first().cloned()))
    }

    /// Look up a series by search term. Same rationale as
    /// `movie_lookup` — Sonarr's TVDB validation rejects arbitrary
    /// ids, so the test harness asks Sonarr to resolve a real
    /// series first.
    pub async fn series_lookup(&self, term: &str) -> Result<Value> {
        let url = format!("/api/v3/series/lookup?term={}", urlencoding(term));
        let result = self.get_json(&url).await?;
        result
            .as_array()
            .and_then(|a| a.first().cloned())
            .ok_or_else(|| format!("series lookup for `{term}` returned no results").into())
    }

    pub async fn add_series_from_lookup(
        &self,
        lookup: &Value,
        root_folder: &Path,
    ) -> Result<Value> {
        let mut body = lookup.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("qualityProfileId".to_owned(), json!(1));
            obj.insert("languageProfileId".to_owned(), json!(1));
            obj.insert(
                "rootFolderPath".to_owned(),
                json!(root_folder.display().to_string()),
            );
            obj.insert("monitored".to_owned(), json!(true));
            obj.insert("seasonFolder".to_owned(), json!(true));
            obj.insert(
                "addOptions".to_owned(),
                json!({
                    "searchForMissingEpisodes": false,
                    "monitor": "none",
                    "ignoreEpisodesWithFiles": false,
                    "ignoreEpisodesWithoutFiles": false
                }),
            );
        }
        self.post_json("/api/v3/series", &body).await
    }

    pub async fn delete_series(&self, id: u32, delete_files: bool) -> Result<()> {
        self.delete(&format!("/api/v3/series/{id}?deleteFiles={delete_files}"))
            .await
    }
}

/// Minimal percent-encoding sufficient for search terms. Avoids
/// pulling in the `urlencoding` crate for one function.
fn urlencoding(input: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => write!(out, "%{byte:02X}").expect("write to String"),
        }
    }
    out
}
