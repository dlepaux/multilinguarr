//! Thin HTTP wrapper around the Jellyfin API.
//!
//! Only the endpoints multilinguarr actually hits are modelled —
//! currently just `POST /Library/Refresh`. More methods land as the
//! feature surface grows.

use std::time::Duration;

use reqwest::Client;
use url::Url;

use super::error::JellyfinError;

/// Cloneable Jellyfin HTTP client.
///
/// Owns a single `reqwest::Client` with pooled connections, the
/// configured base URL, API key, and per-request timeout. The client
/// is deliberately small — the debounce + retry concerns live in the
/// `JellyfinService` wrapper.
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    http: Client,
    base_url: Url,
    api_key: String,
    timeout: Duration,
}

impl JellyfinClient {
    /// Build a client for `base_url` authenticated with `api_key`.
    ///
    /// # Errors
    ///
    /// Returns [`JellyfinError::InvalidUrl`] if `base_url` cannot be parsed.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest::Client` builder fails (should never happen
    /// with default TLS settings).
    pub fn new(
        base_url: &str,
        api_key: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, JellyfinError> {
        let base_url = Url::parse(base_url)?;
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client builder");
        Ok(Self {
            http,
            base_url,
            api_key: api_key.into(),
            timeout,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Trigger a full library refresh on the Jellyfin server.
    ///
    /// Errors are surfaced for logging but not propagated beyond the
    /// caller — library refresh is best-effort and never fails a
    /// webhook.
    ///
    /// # Errors
    ///
    /// - [`JellyfinError::InvalidUrl`] if the refresh URL cannot be joined.
    /// - [`JellyfinError::Request`] on network / connection failure.
    /// - [`JellyfinError::Status`] if the server returns a non-2xx status.
    pub async fn refresh_all_libraries(&self) -> Result<(), JellyfinError> {
        let url = self
            .base_url
            .join("Library/Refresh")
            .map_err(JellyfinError::InvalidUrl)?;
        let response = self
            .http
            .post(url)
            .header("X-Emby-Token", &self.api_key)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(JellyfinError::Status {
                status: response.status().as_u16(),
            });
        }
        Ok(())
    }
}
