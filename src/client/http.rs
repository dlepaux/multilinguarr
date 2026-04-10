//! Shared HTTP plumbing for Radarr/Sonarr clients.
//!
//! `HttpCore` owns a cloneable `reqwest::Client` (which internally pools
//! connections), a base URL, an API key, and a retry policy. It knows
//! nothing about Radarr vs Sonarr — it just speaks JSON over HTTP and
//! classifies responses into [`ArrError`] variants.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use super::error::ArrError;

/// Default per-request timeout. Tuned to be forgiving for arr instances
/// behind slow storage, but tight enough to fail fast on dead hosts.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Retry policy for transient failures.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl RetryPolicy {
    /// Sensible defaults: three attempts, 200ms → 400ms → 800ms (capped).
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(5),
        }
    }

    /// Zero-retry policy — for tests that want to observe one attempt.
    #[must_use]
    pub const fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(0),
            max_backoff: Duration::from_millis(0),
        }
    }

    fn backoff_for(&self, attempt: u32) -> Duration {
        let exp = self
            .initial_backoff
            .saturating_mul(2_u32.saturating_pow(attempt.saturating_sub(1)));
        exp.min(self.max_backoff)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Cloneable, shareable HTTP core. `reqwest::Client` already implements
/// `Clone` cheaply via an internal `Arc`, so deriving `Clone` on the
/// whole struct is free.
#[derive(Debug, Clone)]
pub struct HttpCore {
    instance: String,
    base_url: Url,
    api_key: String,
    client: Client,
    retry: RetryPolicy,
    timeout: Duration,
}

impl HttpCore {
    /// Build an `HttpCore` from an instance URL string. Returns
    /// `ArrError::InvalidUrl` if the URL cannot be parsed.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest` client builder fails — should not happen
    /// with the default options used here.
    ///
    /// # Errors
    ///
    /// Returns `ArrError::InvalidUrl` if `base_url` is not a valid URL.
    pub fn new(
        instance: impl Into<String>,
        base_url: &str,
        api_key: impl Into<String>,
        timeout: Duration,
        retry: RetryPolicy,
    ) -> Result<Self, ArrError> {
        let instance = instance.into();
        let base_url = Url::parse(base_url).map_err(|source| ArrError::InvalidUrl {
            instance: instance.clone(),
            source,
        })?;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client construction should never fail with these options");
        Ok(Self {
            instance,
            base_url,
            api_key: api_key.into(),
            client,
            retry,
            timeout,
        })
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Build a full URL for an API endpoint path (e.g. `/api/v3/movie`).
    fn endpoint_url(&self, endpoint: &str) -> Result<Url, ArrError> {
        self.base_url
            .join(endpoint.trim_start_matches('/'))
            .map_err(|source| ArrError::InvalidUrl {
                instance: self.instance.clone(),
                source,
            })
    }

    /// GET an endpoint and deserialize the JSON body into `T`.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network failure, timeout, non-success HTTP
    /// status, or JSON deserialization failure.
    pub async fn get_json<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T, ArrError> {
        self.execute_json::<_, T>(Method::GET, endpoint, None::<&()>)
            .await
    }

    /// POST a JSON body to an endpoint and deserialize the JSON response.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network failure, timeout, non-success HTTP
    /// status, or JSON deserialization failure.
    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &B,
    ) -> Result<T, ArrError> {
        self.execute_json::<_, T>(Method::POST, endpoint, Some(body))
            .await
    }

    /// DELETE an endpoint, discarding the response body.
    ///
    /// # Errors
    ///
    /// Returns `ArrError` on network failure, timeout, or non-success
    /// HTTP status.
    pub async fn delete(&self, endpoint: &str) -> Result<(), ArrError> {
        self.execute_ignoring_body(Method::DELETE, endpoint).await
    }

    async fn execute_json<B, T>(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&B>,
    ) -> Result<T, ArrError>
    where
        B: Serialize,
        T: DeserializeOwned,
    {
        let response = self.execute_with_retry(method, endpoint, body).await?;
        response
            .json::<T>()
            .await
            .map_err(|source| ArrError::Deserialize {
                instance: self.instance.clone(),
                endpoint: endpoint.to_owned(),
                source,
            })
    }

    async fn execute_ignoring_body(&self, method: Method, endpoint: &str) -> Result<(), ArrError> {
        self.execute_with_retry::<()>(method, endpoint, None)
            .await?;
        Ok(())
    }

    async fn execute_with_retry<B>(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&B>,
    ) -> Result<reqwest::Response, ArrError>
    where
        B: Serialize,
    {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let result = self.execute_once(method.clone(), endpoint, body).await;
            match result {
                Ok(response) => return Ok(response),
                Err(err) if err.is_transient() && attempt < self.retry.max_attempts => {
                    tokio::time::sleep(self.retry.backoff_for(attempt)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn execute_once<B>(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&B>,
    ) -> Result<reqwest::Response, ArrError>
    where
        B: Serialize,
    {
        let url = self.endpoint_url(endpoint)?;
        let mut request = self
            .client
            .request(method, url)
            .header("X-Api-Key", &self.api_key);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|source| {
            if source.is_timeout() {
                ArrError::Timeout {
                    instance: self.instance.clone(),
                    timeout_ms: self.timeout.as_millis().try_into().unwrap_or(u64::MAX),
                }
            } else {
                ArrError::Request {
                    instance: self.instance.clone(),
                    source,
                }
            }
        })?;

        classify_status(&self.instance, endpoint, response).await
    }
}

async fn classify_status(
    instance: &str,
    endpoint: &str,
    response: reqwest::Response,
) -> Result<reqwest::Response, ArrError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(ArrError::NotFound {
            instance: instance.to_owned(),
            endpoint: endpoint.to_owned(),
        });
    }
    let code = status.as_u16();
    let body = response.text().await.unwrap_or_default();
    if status.is_server_error() {
        Err(ArrError::Server {
            instance: instance.to_owned(),
            endpoint: endpoint.to_owned(),
            status: code,
            body,
        })
    } else {
        Err(ArrError::Client {
            instance: instance.to_owned(),
            endpoint: endpoint.to_owned(),
            status: code,
            body,
        })
    }
}
