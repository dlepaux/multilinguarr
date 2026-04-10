//! Jellyfin integration errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum JellyfinError {
    #[error("invalid jellyfin url: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("jellyfin request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("jellyfin server returned {status}")]
    Status { status: u16 },
}
