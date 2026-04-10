//! Cross-instance arr API operations.
//!
//! Two flows live here:
//!
//! 1. **Add propagation** — when a primary instance imports
//!    single-language content, every other instance of the same kind
//!    whose configured language is *different* must add the
//!    movie/series so it can fetch its own copy.
//!
//! 2. **Delete propagation** — when a primary instance deletes,
//!    every other instance with `propagate_delete = true` (default)
//!    gets the corresponding `delete_*` call.

use tracing::{info, warn};

use super::error::HandlerError;
use super::registry::HandlerRegistry;
use crate::client::{
    AddMovieOptions, AddMovieRequest, AddSeriesOptions, AddSeriesRequest, ArrClient, SeasonInfo,
};
use crate::config::{InstanceConfig, InstanceKind};
use crate::detection::FfprobeProber;
use crate::webhook::{RadarrMovieRef, SonarrSeriesRef};

// =====================================================================
// Add propagation
// =====================================================================

/// Add a movie to every Radarr instance whose configured language is
/// different from `source_instance`'s language. Idempotent — skips
/// targets that already have the movie.
pub async fn propagate_add_movie<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    movie_ref: &RadarrMovieRef,
) -> Result<(), HandlerError> {
    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| {
            i.kind == InstanceKind::Radarr
                && i.name != source_instance.name
                && i.language != source_instance.language
        })
        .collect();

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Radarr(target_radarr) = target_client else {
            continue;
        };

        // Dedup check.
        if let Some(existing) = target_radarr
            .get_movie_by_tmdb_id(movie_ref.tmdb_id)
            .await?
        {
            info!(
                target = %target.name,
                tmdb_id = movie_ref.tmdb_id,
                existing_id = existing.id,
                "movie already exists in target instance — skipping cross-instance add"
            );
            continue;
        }

        let profiles = target_radarr.quality_profiles().await?;
        let folders = target_radarr.root_folders().await?;
        let Some(profile) = profiles.first() else {
            warn!(target = %target.name, "target has no quality profiles — cannot add movie");
            continue;
        };
        let Some(folder) = folders.first() else {
            warn!(target = %target.name, "target has no root folders — cannot add movie");
            continue;
        };

        let req = AddMovieRequest {
            title: movie_ref.title.clone(),
            year: movie_ref.year,
            tmdb_id: movie_ref.tmdb_id,
            quality_profile_id: profile.id,
            root_folder_path: folder.path.clone(),
            monitored: true,
            add_options: AddMovieOptions {
                search_for_movie: true,
            },
        };
        let added = target_radarr.add_movie(&req).await?;
        info!(
            source = %source_instance.name,
            target = %target.name,
            tmdb_id = movie_ref.tmdb_id,
            added_id = added.id,
            "cross-instance add_movie succeeded"
        );
    }
    Ok(())
}

/// Add a series to every Sonarr instance whose configured language is
/// different from `source_instance`. Copies season monitoring from the
/// source instance so all monitored seasons are searched on the target.
pub async fn propagate_add_series<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    series_ref: &SonarrSeriesRef,
) -> Result<(), HandlerError> {
    // Fetch season monitoring from the source instance.
    let source_client = registry.client(&source_instance.name)?;
    let ArrClient::Sonarr(source_sonarr) = source_client else {
        return Ok(());
    };
    let seasons = if let Some(source_series) = source_sonarr
        .get_series_by_tvdb_id(series_ref.tvdb_id)
        .await?
    {
        source_series
            .seasons
            .into_iter()
            .map(|s| SeasonInfo {
                season_number: s.season_number,
                monitored: s.monitored,
            })
            .collect()
    } else {
        warn!(
            source = %source_instance.name,
            tvdb_id = series_ref.tvdb_id,
            "could not get series details from source — adding with empty seasons"
        );
        vec![]
    };

    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| {
            i.kind == InstanceKind::Sonarr
                && i.name != source_instance.name
                && i.language != source_instance.language
        })
        .collect();

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Sonarr(target_sonarr) = target_client else {
            continue;
        };

        if let Some(existing) = target_sonarr
            .get_series_by_tvdb_id(series_ref.tvdb_id)
            .await?
        {
            info!(
                target = %target.name,
                tvdb_id = series_ref.tvdb_id,
                existing_id = existing.id,
                "series already exists in target instance — skipping cross-instance add"
            );
            continue;
        }

        let profiles = target_sonarr.quality_profiles().await?;
        let folders = target_sonarr.root_folders().await?;
        let Some(profile) = profiles.first() else {
            warn!(target = %target.name, "target has no quality profiles — cannot add series");
            continue;
        };
        let Some(folder) = folders.first() else {
            warn!(target = %target.name, "target has no root folders — cannot add series");
            continue;
        };

        let req = AddSeriesRequest {
            title: series_ref.title.clone(),
            year: None,
            tvdb_id: series_ref.tvdb_id,
            quality_profile_id: profile.id,
            root_folder_path: folder.path.clone(),
            season_folder: true,
            monitored: true,
            seasons: seasons.clone(),
            add_options: AddSeriesOptions {
                search_for_missing_episodes: true,
            },
        };
        let added = target_sonarr.add_series(&req).await?;
        info!(
            source = %source_instance.name,
            target = %target.name,
            tvdb_id = series_ref.tvdb_id,
            added_id = added.id,
            "cross-instance add_series succeeded"
        );
    }
    Ok(())
}

// =====================================================================
// Delete propagation
// =====================================================================

/// Propagate a delete from a primary Radarr instance to every other
/// Radarr instance with `propagate_delete = true`.
pub async fn propagate_delete_movie<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    tmdb_id: u32,
    delete_files: bool,
) -> Result<(), HandlerError> {
    if !source_instance.propagate_delete {
        info!(
            source = %source_instance.name,
            "propagate_delete=false on source instance — skipping cross-instance delete"
        );
        return Ok(());
    }

    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| i.kind == InstanceKind::Radarr && i.name != source_instance.name)
        .collect();

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Radarr(target_radarr) = target_client else {
            continue;
        };
        let Some(existing) = target_radarr.get_movie_by_tmdb_id(tmdb_id).await? else {
            info!(
                target = %target.name,
                tmdb_id,
                "movie not present in target — nothing to delete"
            );
            continue;
        };
        target_radarr
            .delete_movie(existing.id, delete_files)
            .await?;
        info!(
            source = %source_instance.name,
            target = %target.name,
            tmdb_id,
            target_id = existing.id,
            "cross-instance delete_movie succeeded"
        );
    }
    Ok(())
}

/// Propagate a delete from a primary Sonarr instance to every other
/// Sonarr instance with `propagate_delete = true`.
pub async fn propagate_delete_series<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    tvdb_id: u32,
    delete_files: bool,
) -> Result<(), HandlerError> {
    if !source_instance.propagate_delete {
        info!(
            source = %source_instance.name,
            "propagate_delete=false on source instance — skipping cross-instance delete"
        );
        return Ok(());
    }

    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| i.kind == InstanceKind::Sonarr && i.name != source_instance.name)
        .collect();

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Sonarr(target_sonarr) = target_client else {
            continue;
        };
        let Some(existing) = target_sonarr.get_series_by_tvdb_id(tvdb_id).await? else {
            info!(
                target = %target.name,
                tvdb_id,
                "series not present in target — nothing to delete"
            );
            continue;
        };
        target_sonarr
            .delete_series(existing.id, delete_files)
            .await?;
        info!(
            source = %source_instance.name,
            target = %target.name,
            tvdb_id,
            target_id = existing.id,
            "cross-instance delete_series succeeded"
        );
    }
    Ok(())
}
