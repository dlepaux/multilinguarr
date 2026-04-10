//! Validated, resolved configuration types shared across the crate.
//!
//! These types are constructed by [`super::loader`] and then wrapped in
//! `Arc<Config>` for cheap sharing across async tasks.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Instance engine — Radarr or Sonarr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceKind {
    Radarr,
    Sonarr,
}

/// Linking strategy between the arr storage tree and the media library tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkStrategy {
    Symlink,
    Hardlink,
}

/// Fully validated, secret-resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub log_level: String,
    pub media_base_path: PathBuf,
    pub database_path: PathBuf,
    pub api_key: String,
    pub queue: QueueConfig,
    pub languages: LanguagesConfig,
    pub instances: Vec<InstanceConfig>,
    pub jellyfin: Option<JellyfinConfig>,
}

#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub concurrency: usize,
}

#[derive(Debug, Clone)]
pub struct LanguagesConfig {
    pub primary: String,
    pub alternates: Vec<String>,
    pub definitions: HashMap<String, LanguageDefinition>,
}

#[derive(Debug, Clone)]
pub struct LanguageDefinition {
    pub iso_639_1: Vec<String>,
    pub iso_639_2: Vec<String>,
    pub radarr_id: u32,
    pub sonarr_id: u32,
}

#[derive(Debug, Clone)]
pub struct InstanceConfig {
    pub name: String,
    pub kind: InstanceKind,
    pub language: String,
    pub url: String,
    pub api_key: String,
    pub storage_path: PathBuf,
    pub library_path: PathBuf,
    pub link_strategy: LinkStrategy,
    /// When `true`, deletes on this instance fan out to other
    /// instances via the arr API. Defaults to `true`. See story 08b.
    pub propagate_delete: bool,
}

#[derive(Debug, Clone)]
pub struct JellyfinConfig {
    pub url: String,
    pub api_key: String,
}
