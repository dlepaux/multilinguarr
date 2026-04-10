//! Bootstrap configuration from environment variables.
//!
//! These are the only env vars multilinguarr reads. Everything else
//! (languages, instances, jellyfin) is configured via the API and
//! stored in `SQLite`.

use std::path::PathBuf;

/// Minimal config parsed from env vars at startup — enough to open
/// the database and start the HTTP server.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub port: u16,
    pub api_key: String,
    pub media_base_path: PathBuf,
    pub database_path: PathBuf,
    pub log_level: String,
}

const DEFAULT_PORT: u16 = 3100;
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_DATABASE_PATH: &str = "/data/multilinguarr.db";

impl Bootstrap {
    /// Parse bootstrap config from environment variables.
    ///
    /// Required: `MULTILINGUARR_API_KEY`, `MULTILINGUARR_MEDIA_BASE_PATH`.
    /// Optional: `MULTILINGUARR_PORT`, `MULTILINGUARR_DATABASE_PATH`, `MULTILINGUARR_LOG_LEVEL`.
    ///
    /// # Errors
    ///
    /// - [`BootstrapError::Missing`] if a required env var is not set.
    /// - [`BootstrapError::InvalidValue`] if `MULTILINGUARR_PORT` is not a valid `u16`.
    pub fn from_env() -> Result<Self, BootstrapError> {
        let api_key = require_env("MULTILINGUARR_API_KEY")?;
        let media_base_path = PathBuf::from(require_env("MULTILINGUARR_MEDIA_BASE_PATH")?);

        let port = opt_env("MULTILINGUARR_PORT")
            .map(|v| {
                v.parse::<u16>().map_err(|_| BootstrapError::InvalidValue {
                    var: "MULTILINGUARR_PORT".to_owned(),
                    value: v,
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_PORT);

        let database_path = opt_env("MULTILINGUARR_DATABASE_PATH")
            .map_or_else(|| PathBuf::from(DEFAULT_DATABASE_PATH), PathBuf::from);

        let log_level =
            opt_env("MULTILINGUARR_LOG_LEVEL").unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_owned());

        Ok(Self {
            port,
            api_key,
            media_base_path,
            database_path,
            log_level,
        })
    }
}

fn require_env(var: &str) -> Result<String, BootstrapError> {
    std::env::var(var).map_err(|_| BootstrapError::Missing(var.to_owned()))
}

fn opt_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// Errors during bootstrap env parsing.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("required environment variable {0} is not set")]
    Missing(String),

    #[error("environment variable {var} has invalid value: {value}")]
    InvalidValue { var: String, value: String },
}
