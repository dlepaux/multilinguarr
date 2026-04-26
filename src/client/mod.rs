//! HTTP clients for Radarr and Sonarr v3 APIs.
//!
//! Two concrete clients — [`RadarrClient`] and [`SonarrClient`] — share
//! a common [`HttpCore`] for transport, retry, and error classification.
//! An [`ArrClient`] enum wraps them so callers can store a heterogeneous
//! collection of instances from config.

mod common;
mod error;
mod http;
mod radarr;
mod sonarr;

#[cfg(test)]
mod tests;

pub use common::{AddOutcome, Language, QualityProfile, RootFolder};
pub use error::ArrError;
pub use http::{HttpCore, RetryPolicy, DEFAULT_TIMEOUT};
pub use radarr::{AddMovieOptions, AddMovieRequest, RadarrClient, RadarrMovie};
pub use sonarr::{
    AddSeriesOptions, AddSeriesRequest, EpisodeFile, SeasonInfo, SonarrClient, SonarrSeries,
};

use crate::config::{InstanceConfig, InstanceKind};

/// Dispatching wrapper around the two concrete clients.
///
/// Matching on `ArrClient` gives the compiler the type information it
/// needs to call kind-specific methods without runtime errors — there
/// is no "wrong kind" path at runtime.
#[derive(Debug, Clone)]
pub enum ArrClient {
    Radarr(RadarrClient),
    Sonarr(SonarrClient),
}

impl ArrClient {
    /// Build an `ArrClient` for a configured instance. Uses the default
    /// timeout and retry policy; swap with [`Self::from_instance_with`]
    /// when you need custom values (e.g. in tests).
    ///
    /// # Errors
    ///
    /// Returns `ArrError::InvalidUrl` if the instance URL cannot be parsed.
    pub fn from_instance(instance: &InstanceConfig) -> Result<Self, ArrError> {
        Self::from_instance_with(instance, DEFAULT_TIMEOUT, RetryPolicy::defaults())
    }

    /// Build an `ArrClient` with custom timeout and retry policy.
    ///
    /// # Errors
    ///
    /// Returns `ArrError::InvalidUrl` if the instance URL cannot be parsed.
    pub fn from_instance_with(
        instance: &InstanceConfig,
        timeout: std::time::Duration,
        retry: RetryPolicy,
    ) -> Result<Self, ArrError> {
        let http = HttpCore::new(
            instance.name.clone(),
            &instance.url,
            instance.api_key.clone(),
            timeout,
            retry,
        )?;
        Ok(match instance.kind {
            InstanceKind::Radarr => Self::Radarr(RadarrClient::new(http)),
            InstanceKind::Sonarr => Self::Sonarr(SonarrClient::new(http)),
        })
    }

    #[must_use]
    pub fn instance(&self) -> &str {
        match self {
            Self::Radarr(c) => c.instance(),
            Self::Sonarr(c) => c.instance(),
        }
    }
}
