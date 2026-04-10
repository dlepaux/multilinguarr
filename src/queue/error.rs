//! Queue errors.

use thiserror::Error;

use crate::db::DbError;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("database error: {0}")]
    Db(#[from] DbError),

    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("failed to (de)serialize job payload: {0}")]
    Payload(#[from] serde_json::Error),

    #[error("invalid job kind in database: `{0}` — handler registry out of sync with schema")]
    UnknownKind(String),

    #[error("invalid job status in database: `{0}` — schema check should have prevented this")]
    InvalidStatus(String),
}
