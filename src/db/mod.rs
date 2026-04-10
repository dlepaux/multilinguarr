//! `SQLite` access layer.
//!
//! Owns the connection pool, applies embedded migrations, and exposes
//! a thin `Database` handle that downstream modules (queue, admin
//! endpoints) clone around. Every read/write hits this pool — there is
//! one `SQLite` database per multilinguarr process.

mod error;

#[cfg(test)]
mod tests;

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};

pub use error::DbError;

/// Embedded migrations bundled into the binary at compile time.
///
/// Pointing `sqlx::migrate!()` at `./migrations` means new `.sql`
/// files dropped into that directory are picked up automatically on
/// the next `cargo build`.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Cloneable database handle — wraps a shared `Pool<Sqlite>`.
#[derive(Debug, Clone)]
pub struct Database {
    pool: Pool<Sqlite>,
}

impl Database {
    /// Open a database at `path`, create the file if missing, apply
    /// all pending migrations, and return a ready-to-use handle.
    ///
    /// Tuning applied on every new connection:
    /// - `journal_mode = WAL`   → concurrent readers + single writer
    /// - `synchronous = NORMAL` → durable but not fsync-per-write
    /// - `foreign_keys = ON`    → enforce referential integrity
    ///
    /// # Errors
    ///
    /// - [`DbError::CreateParent`] if the parent directory cannot be created.
    /// - [`DbError::Sqlx`] if the connection fails.
    /// - [`DbError::Migrate`] if a migration fails.
    pub async fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|source| {
                    DbError::CreateParent {
                        path: parent.to_path_buf(),
                        source,
                    }
                })?;
            }
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));

        Self::connect_with(options, 5).await
    }

    /// Open an in-memory database for tests. Every call returns a
    /// fresh, isolated database. Pinned to a single connection because
    /// each new connection to `:memory:` opens a fresh, unrelated
    /// database — sharing state across connections would require the
    /// `cache=shared` URI variant, which we do not need for tests.
    ///
    /// # Errors
    ///
    /// - [`DbError::Sqlx`] if the connection fails.
    /// - [`DbError::Migrate`] if a migration fails.
    pub async fn in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        Self::connect_with(options, 1).await
    }

    async fn connect_with(
        options: SqliteConnectOptions,
        max_connections: u32,
    ) -> Result<Self, DbError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await?;

        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }
}
