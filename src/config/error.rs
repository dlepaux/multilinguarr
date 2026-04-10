//! Config validation errors with actionable messages.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    MissingEnvVar(String),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("duplicate instance name `{0}` — every instance must have a unique name")]
    DuplicateInstance(String),

    #[error(
        "instance `{instance}` references language `{language}` which does not exist \
         — create it first via POST /api/v1/languages"
    )]
    UnknownInstanceLanguage { instance: String, language: String },

    #[error(
        "primary language `{0}` does not exist \
         — create it via POST /api/v1/languages, then set it via PUT /api/v1/config"
    )]
    UnknownPrimaryLanguage(String),

    #[error(
        "hardlink instance `{instance}`: storage `{storage}` and library `{library}` \
         are on different filesystems — hardlinks cannot cross devices. \
         Use link_strategy: \"symlink\" instead, or move both paths to the same filesystem."
    )]
    CrossFilesystemHardlink {
        instance: String,
        storage: PathBuf,
        library: PathBuf,
    },

    #[error("cannot stat `{path}` for instance `{instance}`: {source}")]
    PathStat {
        instance: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no languages configured — add at least one via POST /api/v1/languages")]
    NoLanguages,

    #[error("no instances configured — add at least one via POST /api/v1/instances")]
    NoInstances,

    #[error(
        "primary language not set — configure it via PUT /api/v1/config \
         with {{ \"primary_language\": \"fr\" }}"
    )]
    NoPrimaryLanguage,

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}
