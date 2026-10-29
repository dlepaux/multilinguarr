//! Delete handlers — Radarr `MovieDelete` / `MovieFileDelete`,
//! Sonarr `SeriesDelete` / `EpisodeFileDelete`.
//!
//! All handlers share the same shape:
//!
//! 1. Honour the `deletedFiles` guard — if the user only deleted
//!    metadata, leave the filesystem alone.
//! 2. Resolve the folder/file name from the webhook payload (it
//!    carries enough; we do not need to round-trip the arr API).
//! 3. **Storage-aware unlink** in every affected library:
//!    - Symlink instances: only remove links whose target lives
//!      under the *source* instance's storage tree. A link that
//!      points into another instance's storage is owned by that
//!      instance and must not be touched.
//!    - Hardlink instances: refuse to remove the last hardlink
//!      (`is_last_hardlink == true`), which would orphan the file.
//!    - Alternate deletes follow the same ownership rule in their own
//!      library: a symlink into another instance's storage is kept. A
//!      hardlink carries no target to check, so a hardlink instance's
//!      alternate delete still removes every entry; the file survives
//!      in the storage that holds it.
//! 4. For primary instances, propagate the delete to other instances
//!    via the arr API (08b `cross_instance` helpers), skipping any
//!    sibling record that holds files of its own.
//! 5. Trigger a Jellyfin refresh.

use std::path::{Path, PathBuf};

use tracing::{info, info_span, warn, Instrument};

use super::cross_instance::{propagate_delete_movie, propagate_delete_series};
use super::error::HandlerError;
use super::registry::HandlerRegistry;
use crate::config::{InstanceConfig, InstanceKind};
use crate::detection::FfprobeProber;
use crate::link::LinkManager;
use crate::webhook::{
    RadarrMovieDelete, RadarrMovieFileDelete, SonarrEpisodeFileDelete, SonarrSeriesDelete,
};

// =====================================================================
// Radarr — MovieDelete
// =====================================================================

pub async fn handle_radarr_movie_delete<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &RadarrMovieDelete,
    registry: &HandlerRegistry<P>,
) -> Result<(), HandlerError> {
    let movie_ref = event
        .movie
        .as_ref()
        .ok_or(HandlerError::MissingField("movie"))?;
    let span = info_span!(
        "radarr_movie_delete",
        instance = %instance.name,
        tmdb_id = movie_ref.tmdb_id,
        deleted_files = event.deleted_files,
    );
    async move {
        if !event.deleted_files {
            info!("deletedFiles=false — leaving the filesystem alone");
            return Ok(());
        }

        let folder_name = folder_name_from_webhook(movie_ref.folder_path.as_deref())?;

        if registry.is_primary(instance) {
            unlink_movie_everywhere_owned_by(registry, instance, &folder_name).await?;
            propagate_delete_movie(registry, instance, movie_ref.tmdb_id, true).await?;
        } else {
            // Alternate delete: only this instance's library, never
            // propagate, never touch the primary library.
            let mgr = registry.link_manager(&instance.name)?;
            unlink_movie_local_only(
                mgr,
                &foreign_storage_roots(registry, instance),
                &folder_name,
            )
            .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

// =====================================================================
// Radarr — MovieFileDelete (single file removed; movie record may stay)
// =====================================================================

pub async fn handle_radarr_movie_file_delete<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &RadarrMovieFileDelete,
    registry: &HandlerRegistry<P>,
) -> Result<(), HandlerError> {
    let movie_ref = event
        .movie
        .as_ref()
        .ok_or(HandlerError::MissingField("movie"))?;
    let span = info_span!(
        "radarr_movie_file_delete",
        instance = %instance.name,
        tmdb_id = movie_ref.tmdb_id,
    );
    async move {
        // MovieFileDelete events do not carry a deletedFiles flag the
        // same way MovieDelete does — the file is gone by definition.
        let folder_name = folder_name_from_webhook(movie_ref.folder_path.as_deref())?;

        if registry.is_primary(instance) {
            unlink_movie_everywhere_owned_by(registry, instance, &folder_name).await?;
            // We do NOT propagate the cross-instance delete here —
            // the file deletion is local to this instance; the other
            // instance still owns its own copy independently.
        } else {
            let mgr = registry.link_manager(&instance.name)?;
            unlink_movie_local_only(
                mgr,
                &foreign_storage_roots(registry, instance),
                &folder_name,
            )
            .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

// =====================================================================
// Sonarr — SeriesDelete (whole series removed)
// =====================================================================

pub async fn handle_sonarr_series_delete<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &SonarrSeriesDelete,
    registry: &HandlerRegistry<P>,
) -> Result<(), HandlerError> {
    let series_ref = event
        .series
        .as_ref()
        .ok_or(HandlerError::MissingField("series"))?;
    let span = info_span!(
        "sonarr_series_delete",
        instance = %instance.name,
        tvdb_id = series_ref.tvdb_id,
        deleted_files = event.deleted_files,
    );
    async move {
        if !event.deleted_files {
            info!("deletedFiles=false — leaving the filesystem alone");
            return Ok(());
        }

        let folder_name = folder_name_from_webhook(series_ref.path.as_deref())?;

        if registry.is_primary(instance) {
            unlink_series_everywhere_owned_by(registry, instance, &folder_name).await?;
            propagate_delete_series(registry, instance, series_ref.tvdb_id, true).await?;
        } else {
            let mgr = registry.link_manager(&instance.name)?;
            unlink_series_local_only(
                mgr,
                &foreign_storage_roots(registry, instance),
                &folder_name,
            )
            .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

// =====================================================================
// Sonarr — EpisodeFileDelete (single episode removed)
// =====================================================================

pub async fn handle_sonarr_episode_file_delete<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &SonarrEpisodeFileDelete,
    registry: &HandlerRegistry<P>,
) -> Result<(), HandlerError> {
    let series_ref = event
        .series
        .as_ref()
        .ok_or(HandlerError::MissingField("series"))?;
    let episode_file_ref = event
        .episode_file
        .as_ref()
        .ok_or(HandlerError::MissingField("episode_file"))?;
    let span = info_span!(
        "sonarr_episode_file_delete",
        instance = %instance.name,
        tvdb_id = series_ref.tvdb_id,
        episode_file_id = episode_file_ref.id,
    );
    async move {
        let series_folder = folder_name_from_webhook(series_ref.path.as_deref())?;
        let inner_relative = episode_file_ref
            .relative_path
            .as_deref()
            .ok_or(HandlerError::MissingField("episode_file.relative_path"))?;
        let mut relative = PathBuf::from(&series_folder);
        relative.push(inner_relative);

        if registry.is_primary(instance) {
            unlink_episode_everywhere_owned_by(registry, instance, &relative).await?;
            // Same rationale as MovieFileDelete: do not propagate.
        } else {
            let mgr = registry.link_manager(&instance.name)?;
            unlink_episode_local_only(mgr, &foreign_storage_roots(registry, instance), &relative)
                .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

// =====================================================================
// Helpers
// =====================================================================

/// Extract the bottom folder name from an arr `path` field.
fn folder_name_from_webhook(path: Option<&str>) -> Result<String, HandlerError> {
    let path = path.ok_or(HandlerError::MissingField("movie.folder_path|series.path"))?;
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| HandlerError::MalformedPath(PathBuf::from(path)))
}

/// Unlink a movie folder from every Radarr library whose link is
/// owned by `source.storage_path`. For symlink instances, "owned"
/// means the symlink target lives under `source.storage_path`. For
/// hardlink instances, "owned" means we are the only remaining link
/// to the inode (`is_last_hardlink == false` → safe to remove).
async fn unlink_movie_everywhere_owned_by<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source: &InstanceConfig,
    folder_name: &str,
) -> Result<(), HandlerError> {
    for (target, mgr) in registry.instances_with_link_managers() {
        if target.kind != InstanceKind::Radarr {
            continue;
        }
        unlink_storage_aware(mgr, target, source, folder_name).await?;
    }
    Ok(())
}

/// Every other instance's storage root. An alternate delete keeps the
/// links that resolve into these: they serve another instance's file,
/// and `regenerate` cannot rebuild a hand-placed fallback link (MED-2).
fn foreign_storage_roots<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    instance: &InstanceConfig,
) -> Vec<PathBuf> {
    registry
        .config_instances()
        .iter()
        .filter(|other| other.name != instance.name)
        .map(|other| other.storage_path.clone())
        .collect()
}

async fn unlink_movie_local_only(
    mgr: &LinkManager,
    foreign_roots: &[PathBuf],
    folder_name: &str,
) -> Result<(), HandlerError> {
    let kept = mgr
        .unlink_folder_keeping(folder_name, foreign_roots)
        .await?;
    info!(
        kept,
        "alternate movie delete — own library entry removed, foreign links kept"
    );
    Ok(())
}

async fn unlink_series_everywhere_owned_by<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source: &InstanceConfig,
    folder_name: &str,
) -> Result<(), HandlerError> {
    for (target, mgr) in registry.instances_with_link_managers() {
        if target.kind != InstanceKind::Sonarr {
            continue;
        }
        unlink_storage_aware(mgr, target, source, folder_name).await?;
    }
    Ok(())
}

async fn unlink_series_local_only(
    mgr: &LinkManager,
    foreign_roots: &[PathBuf],
    folder_name: &str,
) -> Result<(), HandlerError> {
    let kept = mgr
        .unlink_folder_keeping(folder_name, foreign_roots)
        .await?;
    info!(
        kept,
        "alternate series delete — own links removed, foreign links kept"
    );
    Ok(())
}

async fn unlink_episode_everywhere_owned_by<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source: &InstanceConfig,
    relative: &Path,
) -> Result<(), HandlerError> {
    for (target, mgr) in registry.instances_with_link_managers() {
        if target.kind != InstanceKind::Sonarr {
            continue;
        }
        unlink_episode_storage_aware(mgr, target, source, relative).await?;
    }
    Ok(())
}

async fn unlink_episode_local_only(
    mgr: &LinkManager,
    foreign_roots: &[PathBuf],
    relative: &Path,
) -> Result<(), HandlerError> {
    for root in foreign_roots {
        if mgr.resolves_into(relative, root).await? {
            info!("alternate episode delete — link serves another instance's file, kept");
            return Ok(());
        }
    }
    mgr.unlink_episode(relative).await?;
    info!("alternate episode delete — local link removed");
    Ok(())
}

/// Unlink a movie folder from `target`'s library only when the
/// existing link is "owned" by `source`'s storage.
async fn unlink_storage_aware(
    target_mgr: &LinkManager,
    target: &InstanceConfig,
    source: &InstanceConfig,
    folder_name: &str,
) -> Result<(), HandlerError> {
    let relative = Path::new(folder_name);
    if target.name == source.name {
        target_mgr.unlink_folder(folder_name).await?;
        info!(target = %target.name, "library entry removed from source instance");
        return Ok(());
    }
    match target.link_strategy {
        crate::config::LinkStrategy::Symlink => {
            if target_mgr
                .resolves_into(relative, &source.storage_path)
                .await?
            {
                target_mgr.unlink_folder(folder_name).await?;
                info!(target = %target.name, "cross-instance link removed (resolved into source storage)");
            } else {
                info!(
                    target = %target.name,
                    "library entry not owned by source — leaving in place"
                );
            }
        }
        crate::config::LinkStrategy::Hardlink => {
            // For directory-level hardlink mirrors there is no single
            // file to inspect — refuse to walk into it. Mirrored
            // directories are removed unconditionally because the
            // primary instance owns the source.
            target_mgr.unlink_folder(folder_name).await?;
            warn!(
                target = %target.name,
                "hardlink mirror removed unconditionally — directory-level last-link \
                 detection is per-file, not implemented for delete-by-folder yet"
            );
        }
    }
    Ok(())
}

/// Unlink a single episode file from `target`'s library only when the
/// link is "owned" by `source`'s storage. For hardlink instances,
/// refuse to remove the last hardlink (would orphan the data).
async fn unlink_episode_storage_aware(
    target_mgr: &LinkManager,
    target: &InstanceConfig,
    source: &InstanceConfig,
    relative: &Path,
) -> Result<(), HandlerError> {
    if target.name == source.name {
        target_mgr.unlink_episode(relative).await?;
        info!(target = %target.name, "episode removed from source instance library");
        return Ok(());
    }
    match target.link_strategy {
        crate::config::LinkStrategy::Symlink => {
            if target_mgr
                .resolves_into(relative, &source.storage_path)
                .await?
            {
                target_mgr.unlink_episode(relative).await?;
                info!(target = %target.name, "cross-instance episode removed (resolved into source storage)");
            } else {
                info!(target = %target.name, "episode not owned by source — leaving in place");
            }
        }
        crate::config::LinkStrategy::Hardlink => {
            if target_mgr.is_last_hardlink(relative).await? {
                warn!(
                    target = %target.name,
                    "refusing to unlink last hardlink — would orphan the file"
                );
            } else {
                target_mgr.unlink_episode(relative).await?;
                info!(target = %target.name, "cross-instance hardlink removed");
            }
        }
    }
    Ok(())
}
