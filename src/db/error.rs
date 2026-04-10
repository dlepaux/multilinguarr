//! Database errors.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to create parent directory `{path:?}`: {source}")]
    CreateParent {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
