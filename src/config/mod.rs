//! Configuration: env var bootstrap + `SQLite` persistence + API.
//!
//! Bootstrap env vars (`MULTILINGUARR_*`) are parsed at startup.
//! Everything else (languages, instances) is managed via the
//! `/api/v1/*` endpoints and stored in `SQLite`.

mod bootstrap;
mod error;
mod repo;
mod types;

pub use bootstrap::Bootstrap;
pub use error::ConfigError;
pub use repo::ConfigRepo;
pub use types::{
    Config, InstanceConfig, InstanceKind, JellyfinConfig, LanguageDefinition, LanguagesConfig,
    LinkStrategy, QueueConfig,
};
