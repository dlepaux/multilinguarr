//! Config repository — typed CRUD operations against `SQLite` config tables.
//!
//! No HTTP, no validation beyond DB constraints. Higher layers (API
//! handlers, setup flow) add validation on top.

use std::collections::HashMap;
use std::path::PathBuf;

use sqlx::{Pool, Sqlite};

use super::types::{
    Config, InstanceConfig, InstanceKind, JellyfinConfig, LanguageDefinition, LanguagesConfig,
    LinkStrategy, QueueConfig,
};

/// Config repository backed by `SQLite`.
#[derive(Debug, Clone)]
pub struct ConfigRepo {
    pool: Pool<Sqlite>,
}

// ---------------------------------------------------------------------------
// Language rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct LanguageRow {
    key: String,
    iso_639_1: String, // JSON array
    iso_639_2: String, // JSON array
    radarr_id: i64,
    sonarr_id: i64,
}

impl LanguageRow {
    fn into_definition(self) -> (String, LanguageDefinition) {
        let iso1: Vec<String> = serde_json::from_str(&self.iso_639_1).unwrap_or_else(|e| {
            tracing::warn!(key = %self.key, error = %e, "corrupt iso_639_1 in DB — defaulting to empty");
            vec![]
        });
        let iso2: Vec<String> = serde_json::from_str(&self.iso_639_2).unwrap_or_else(|e| {
            tracing::warn!(key = %self.key, error = %e, "corrupt iso_639_2 in DB — defaulting to empty");
            vec![]
        });
        (
            self.key,
            LanguageDefinition {
                iso_639_1: iso1,
                iso_639_2: iso2,
                radarr_id: u32::try_from(self.radarr_id).unwrap_or(0),
                sonarr_id: u32::try_from(self.sonarr_id).unwrap_or(0),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Instance rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct InstanceRow {
    name: String,
    kind: String,
    language: String,
    url: String,
    api_key: String,
    storage_path: String,
    library_path: String,
    link_strategy: String,
    propagate_delete: bool,
}

impl InstanceRow {
    fn into_config(self) -> InstanceConfig {
        let kind = match self.kind.as_str() {
            "radarr" => InstanceKind::Radarr,
            "sonarr" => InstanceKind::Sonarr,
            other => {
                tracing::warn!(instance = %self.name, kind = other, "unknown instance kind in DB — defaulting to radarr");
                InstanceKind::Radarr
            }
        };
        let link_strategy = match self.link_strategy.as_str() {
            "symlink" => LinkStrategy::Symlink,
            "hardlink" => LinkStrategy::Hardlink,
            other => {
                tracing::warn!(instance = %self.name, strategy = other, "unknown link strategy in DB — defaulting to symlink");
                LinkStrategy::Symlink
            }
        };
        InstanceConfig {
            name: self.name,
            kind,
            language: self.language,
            url: self.url,
            api_key: self.api_key,
            storage_path: PathBuf::from(self.storage_path),
            library_path: PathBuf::from(self.library_path),
            link_strategy,
            propagate_delete: self.propagate_delete,
        }
    }
}

// ---------------------------------------------------------------------------
// Jellyfin rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct JellyfinRow {
    url: String,
    api_key: String,
}

impl ConfigRepo {
    #[must_use]
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    // -----------------------------------------------------------------------
    // Languages
    // -----------------------------------------------------------------------

    /// List all language definitions ordered by key.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn list_languages(&self) -> Result<Vec<(String, LanguageDefinition)>, sqlx::Error> {
        let rows = sqlx::query_as::<_, LanguageRow>("SELECT * FROM languages ORDER BY key")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(LanguageRow::into_definition).collect())
    }

    /// Fetch a single language definition by key.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn get_language(&self, key: &str) -> Result<Option<LanguageDefinition>, sqlx::Error> {
        let row = sqlx::query_as::<_, LanguageRow>("SELECT * FROM languages WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.into_definition().1))
    }

    /// Insert a new language definition.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure (e.g. duplicate key).
    pub async fn insert_language(
        &self,
        key: &str,
        def: &LanguageDefinition,
    ) -> Result<(), sqlx::Error> {
        let iso1 = serde_json::to_string(&def.iso_639_1).unwrap_or_default();
        let iso2 = serde_json::to_string(&def.iso_639_2).unwrap_or_default();
        sqlx::query(
            "INSERT INTO languages (key, iso_639_1, iso_639_2, radarr_id, sonarr_id) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(key)
        .bind(&iso1)
        .bind(&iso2)
        .bind(i64::from(def.radarr_id))
        .bind(i64::from(def.sonarr_id))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update an existing language definition. Returns `true` if a row was updated.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn update_language(
        &self,
        key: &str,
        def: &LanguageDefinition,
    ) -> Result<bool, sqlx::Error> {
        let iso1 = serde_json::to_string(&def.iso_639_1).unwrap_or_default();
        let iso2 = serde_json::to_string(&def.iso_639_2).unwrap_or_default();
        let result = sqlx::query(
            "UPDATE languages SET iso_639_1 = ?, iso_639_2 = ?, radarr_id = ?, sonarr_id = ? \
             WHERE key = ?",
        )
        .bind(&iso1)
        .bind(&iso2)
        .bind(i64::from(def.radarr_id))
        .bind(i64::from(def.sonarr_id))
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete a language by key. Returns `true` if a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn delete_language(&self, key: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM languages WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // -----------------------------------------------------------------------
    // Instances
    // -----------------------------------------------------------------------

    /// List all instance configs ordered by name.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn list_instances(&self) -> Result<Vec<InstanceConfig>, sqlx::Error> {
        let rows = sqlx::query_as::<_, InstanceRow>("SELECT * FROM instances ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(InstanceRow::into_config).collect())
    }

    /// Fetch a single instance config by name.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn get_instance(&self, name: &str) -> Result<Option<InstanceConfig>, sqlx::Error> {
        let row = sqlx::query_as::<_, InstanceRow>("SELECT * FROM instances WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(InstanceRow::into_config))
    }

    /// Insert a new instance config.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure (e.g. duplicate name).
    pub async fn insert_instance(&self, inst: &InstanceConfig) -> Result<(), sqlx::Error> {
        let kind = match inst.kind {
            InstanceKind::Radarr => "radarr",
            InstanceKind::Sonarr => "sonarr",
        };
        let strategy = match inst.link_strategy {
            LinkStrategy::Symlink => "symlink",
            LinkStrategy::Hardlink => "hardlink",
        };
        sqlx::query(
            "INSERT INTO instances \
             (name, kind, language, url, api_key, storage_path, library_path, link_strategy, propagate_delete) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&inst.name)
        .bind(kind)
        .bind(&inst.language)
        .bind(&inst.url)
        .bind(&inst.api_key)
        .bind(inst.storage_path.to_str().unwrap_or_default())
        .bind(inst.library_path.to_str().unwrap_or_default())
        .bind(strategy)
        .bind(inst.propagate_delete)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update an existing instance config. Returns `true` if a row was updated.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn update_instance(&self, inst: &InstanceConfig) -> Result<bool, sqlx::Error> {
        let kind = match inst.kind {
            InstanceKind::Radarr => "radarr",
            InstanceKind::Sonarr => "sonarr",
        };
        let strategy = match inst.link_strategy {
            LinkStrategy::Symlink => "symlink",
            LinkStrategy::Hardlink => "hardlink",
        };
        let result = sqlx::query(
            "UPDATE instances SET kind = ?, language = ?, url = ?, api_key = ?, \
             storage_path = ?, library_path = ?, link_strategy = ?, propagate_delete = ? \
             WHERE name = ?",
        )
        .bind(kind)
        .bind(&inst.language)
        .bind(&inst.url)
        .bind(&inst.api_key)
        .bind(inst.storage_path.to_str().unwrap_or_default())
        .bind(inst.library_path.to_str().unwrap_or_default())
        .bind(strategy)
        .bind(inst.propagate_delete)
        .bind(&inst.name)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete an instance by name. Returns `true` if a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn delete_instance(&self, name: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM instances WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // -----------------------------------------------------------------------
    // Jellyfin
    // -----------------------------------------------------------------------

    /// Fetch the Jellyfin connection config (singleton row).
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn get_jellyfin(&self) -> Result<Option<JellyfinConfig>, sqlx::Error> {
        let row =
            sqlx::query_as::<_, JellyfinRow>("SELECT url, api_key FROM jellyfin WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| JellyfinConfig {
            url: r.url,
            api_key: r.api_key,
        }))
    }

    /// Upsert the Jellyfin connection config.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn set_jellyfin(&self, jf: &JellyfinConfig) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO jellyfin (id, url, api_key) VALUES (1, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET url = excluded.url, api_key = excluded.api_key",
        )
        .bind(&jf.url)
        .bind(&jf.api_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete the Jellyfin config. Returns `true` if a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn delete_jellyfin(&self) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM jellyfin WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // -----------------------------------------------------------------------
    // General config (key-value)
    // -----------------------------------------------------------------------

    /// Fetch a single key-value config entry.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn get_config_value(&self, key: &str) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM config WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    /// Upsert a key-value config entry.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn set_config_value(&self, key: &str, value: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO config (key, value) VALUES (?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Setup
    // -----------------------------------------------------------------------

    /// Check whether initial setup has been completed.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn is_setup_complete(&self) -> Result<bool, sqlx::Error> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM setup WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Mark setup as complete (idempotent).
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` on database failure.
    pub async fn mark_setup_complete(&self) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT OR IGNORE INTO setup (id, completed_at) VALUES (1, datetime('now'))")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Build full Config from DB state
    // -----------------------------------------------------------------------

    /// Assemble a complete `Config` from `SQLite` state + bootstrap env vars.
    /// Returns `None` if setup is not complete or required data is missing.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if any underlying query fails.
    pub async fn load_config(
        &self,
        port: u16,
        log_level: String,
        media_base_path: PathBuf,
        database_path: PathBuf,
        api_key: String,
    ) -> Result<Option<Config>, sqlx::Error> {
        if !self.is_setup_complete().await? {
            return Ok(None);
        }

        let lang_pairs = self.list_languages().await?;
        if lang_pairs.is_empty() {
            return Ok(None);
        }

        let definitions: HashMap<String, LanguageDefinition> = lang_pairs.into_iter().collect();

        let primary = self
            .get_config_value("primary_language")
            .await?
            .unwrap_or_default();
        if primary.is_empty() || !definitions.contains_key(&primary) {
            return Ok(None);
        }

        let alternates: Vec<String> = definitions
            .keys()
            .filter(|k| *k != &primary)
            .cloned()
            .collect();

        let concurrency: usize = self
            .get_config_value("queue_concurrency")
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);

        let instances = self.list_instances().await?;
        let jellyfin = self.get_jellyfin().await?;

        Ok(Some(Config {
            port,
            log_level,
            media_base_path,
            database_path,
            api_key,
            queue: QueueConfig { concurrency },
            languages: LanguagesConfig {
                primary,
                alternates,
                definitions,
            },
            instances,
            jellyfin,
        }))
    }
}
