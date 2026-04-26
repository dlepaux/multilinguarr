//! Errors surfaced by the Radarr/Sonarr API clients.
//!
//! Every variant is labelled as **transient** (retryable — the caller
//! should back off and try again) or **permanent** (not worth retrying).
//! The [`ArrError::is_transient`] helper is what the retry loop checks.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArrError {
    /// The base URL could not be parsed or joined with an endpoint path.
    /// Permanent — a retry will never fix a malformed URL.
    #[error("invalid url for instance `{instance}`: {source}")]
    InvalidUrl {
        instance: String,
        #[source]
        source: url::ParseError,
    },

    /// Network-layer failure before we got a response. Permanent only if
    /// the underlying error says so; most of these (DNS hiccups, connection
    /// reset) are transient.
    #[error("request error talking to `{instance}`: {source}")]
    Request {
        instance: String,
        #[source]
        source: reqwest::Error,
    },

    /// The request exceeded the per-call timeout. Transient.
    #[error("timeout after {timeout_ms}ms talking to `{instance}`")]
    Timeout { instance: String, timeout_ms: u64 },

    /// A 5xx response from the arr instance — the server is unhappy but
    /// the problem is on its side. Transient.
    #[error("server error {status} from `{instance}` on `{endpoint}`: {body}")]
    Server {
        instance: String,
        endpoint: String,
        status: u16,
        body: String,
    },

    /// A 4xx response that is not 404 or 409 — bad request, auth failure, etc.
    /// Permanent — retrying an unauthorized request keeps it unauthorized.
    #[error("client error {status} from `{instance}` on `{endpoint}`: {body}")]
    Client {
        instance: String,
        endpoint: String,
        status: u16,
        body: String,
    },

    /// 409 Conflict. Split from `Client` so callers can implement
    /// idempotent retries structurally (no body-string matching).
    /// Permanent — wrappers absorb it into `AddOutcome::AlreadyExisted`
    /// for the cross-instance add case.
    #[error("conflict 409 from `{instance}` on `{endpoint}`: {body}")]
    Conflict {
        instance: String,
        endpoint: String,
        body: String,
    },

    /// The requested resource does not exist. Permanent — callers handle
    /// this as a domain signal ("movie absent"), not an error to retry.
    #[error("not found: `{endpoint}` on `{instance}`")]
    NotFound { instance: String, endpoint: String },

    /// The response body could not be deserialized into the expected
    /// shape. Permanent — the schema drifted or the data is corrupt;
    /// retrying will give the same bytes back.
    #[error("failed to deserialize response from `{instance}` on `{endpoint}`: {source}")]
    Deserialize {
        instance: String,
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
}

impl ArrError {
    /// Retry-loop predicate: true if backing off and trying again has any
    /// chance of succeeding.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Timeout { .. } | Self::Server { .. } => true,
            Self::Request { source, .. } => {
                source.is_timeout() || source.is_connect() || source.is_request()
            }
            Self::InvalidUrl { .. }
            | Self::Client { .. }
            | Self::Conflict { .. }
            | Self::NotFound { .. }
            | Self::Deserialize { .. } => false,
        }
    }
}
