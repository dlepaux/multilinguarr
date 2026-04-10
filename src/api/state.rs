//! Shared state for the config API.

use std::sync::Arc;

use crate::config::{Config, ConfigRepo};
use crate::detection::{LanguageDetector, SystemFfprobe};
use crate::queue::JobStore;

/// State shared across all `/api/v1/*` handlers.
#[derive(Debug, Clone)]
pub struct ApiState {
    pub repo: ConfigRepo,
    pub job_store: JobStore,
    pub api_key: Arc<str>,
    /// Loaded config — single source of truth for instances, languages,
    /// and media paths. Used by the regenerate endpoint instead of
    /// querying the DB, keeping it consistent with how handlers work.
    pub config: Option<Arc<Config>>,
    /// Detector for regeneration endpoint. `None` if ffprobe is not
    /// available on the host (regenerate will return an error).
    pub detector: Option<LanguageDetector<SystemFfprobe>>,
}

impl ApiState {
    #[must_use]
    pub fn new(
        repo: ConfigRepo,
        job_store: JobStore,
        api_key: String,
        config: Option<Arc<Config>>,
        detector: Option<LanguageDetector<SystemFfprobe>>,
    ) -> Self {
        Self {
            repo,
            job_store,
            api_key: Arc::from(api_key),
            config,
            detector,
        }
    }
}
