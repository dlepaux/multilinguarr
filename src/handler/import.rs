//! Radarr + Sonarr `Download` handlers (import + isUpgrade).
//!
//! Both handlers follow the same shape:
//!
//! 1. Extract file path and folder name from the webhook payload.
//! 2. Run ffprobe on the file to detect languages.
//! 3. Apply the primary-vs-alternate × multi-vs-single matrix to
//!    decide which libraries get links.
//! 4. Trigger Jellyfin refresh.
//!
//! `isUpgrade = true` is handled by unlinking first, then proceeding
//! through the normal link path. The link manager's idempotency does
//! the rest.

use std::path::{Path, PathBuf};

use tracing::Instrument;
use tracing::{info, info_span, warn};

use super::cross_instance::{propagate_add_movie, propagate_add_series};
use super::error::HandlerError;
use super::registry::HandlerRegistry;
use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};
use crate::detection::{DetectionResult, FfprobeProber};
use crate::link::LinkManager;
use crate::webhook::{RadarrDownload, SonarrDownload};

fn strategy_label(strategy: LinkStrategy) -> &'static str {
    match strategy {
        LinkStrategy::Symlink => "symlink",
        LinkStrategy::Hardlink => "hardlink",
    }
}

fn source_label(kind: InstanceKind) -> &'static str {
    match kind {
        InstanceKind::Radarr => "radarr",
        InstanceKind::Sonarr => "sonarr",
    }
}

// =====================================================================
// Radarr
// =====================================================================

pub async fn handle_radarr_download<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &RadarrDownload,
    registry: &HandlerRegistry<P>,
) -> Result<(), HandlerError> {
    let movie_ref = event
        .movie
        .as_ref()
        .ok_or(HandlerError::MissingField("movie"))?;
    let movie_file_ref = event
        .movie_file
        .as_ref()
        .ok_or(HandlerError::MissingField("movie_file"))?;
    let span = info_span!(
        "radarr_download",
        instance = %instance.name,
        tmdb_id = movie_ref.tmdb_id,
        is_upgrade = event.is_upgrade,
    );
    async move {
        // Extract paths from webhook payload.
        let file_path = movie_file_ref
            .path
            .as_deref()
            .or(movie_file_ref.relative_path.as_deref())
            .ok_or(HandlerError::MissingField("movie_file.path"))?;

        let folder_path = movie_ref
            .folder_path
            .as_deref()
            .ok_or(HandlerError::MissingField("movie.folder_path"))?;
        let folder_name = Path::new(folder_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| HandlerError::MalformedPath(PathBuf::from(folder_path)))?;

        // Reject paths outside the configured media root to prevent
        // a compromised arr instance from probing arbitrary files.
        let file = Path::new(file_path);
        if !file.starts_with(&registry.config.media_base_path) {
            return Err(HandlerError::MalformedPath(file.to_path_buf()));
        }

        // ffprobe is the single source of truth for language detection.
        let mut detection: DetectionResult = registry.detector.detect(file).await?;

        // Undetermined language: assume the file is in the downloading
        // instance's language (common for old rips, AVI, untagged MKV).
        // Treated as single-language so alternates get a propagate-add.
        if detection.languages.is_empty() {
            info!(
                instance = %instance.name,
                language = %instance.language,
                "no language tags — assuming instance language"
            );
            metrics::counter!(
                crate::observability::names::LANGUAGE_TAG_FALLBACK,
                "instance" => instance.name.clone(),
                "source" => source_label(instance.kind),
                "fallback_language" => instance.language.clone(),
            )
            .increment(1);
            detection.languages = [instance.language.clone()].into();
            detection.is_multi_audio = false;
        }

        info!(
            languages = ?detection.languages,
            is_multi_audio = detection.is_multi_audio,
            "language detection complete",
        );

        let source_path = instance.storage_path.join(folder_name);

        if event.is_upgrade {
            unlink_radarr_targets(registry, instance, &detection, folder_name).await?;
        }

        if registry.is_primary(instance) {
            link_radarr_primary(registry, instance, &detection, &source_path, folder_name).await?;
            if !detection.is_multi_audio {
                propagate_add_movie(registry, instance, movie_ref).await?;
            }
        } else {
            link_radarr_alternate(registry, instance, &detection, &source_path, folder_name)
                .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

async fn link_radarr_primary<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    primary: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    folder_name: &str,
) -> Result<(), HandlerError> {
    if detection.is_multi_audio {
        let targets = registry.instances_for_languages(InstanceKind::Radarr, &detection.languages);
        info!(
            target_count = targets.len(),
            "primary multi-audio import → linking to every matching language library"
        );
        for target in targets {
            let mgr = registry.link_manager(&target.name)?;
            link_movie_with_log(mgr, source_path, folder_name, &target.name).await?;
        }
    } else {
        let mgr = registry.link_manager(&primary.name)?;
        link_movie_with_log(mgr, source_path, folder_name, &primary.name).await?;
    }
    Ok(())
}

async fn link_radarr_alternate<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    alternate: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    folder_name: &str,
) -> Result<(), HandlerError> {
    if !detection.languages.contains(&alternate.language) {
        warn!(
            instance = %alternate.name,
            language = %alternate.language,
            detected = ?detection.languages,
            "alternate instance imported a file that does not contain its own language — skipping"
        );
        let mut detected_sorted: Vec<&String> = detection.languages.iter().collect();
        detected_sorted.sort();
        let detected_label = detected_sorted
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        metrics::counter!(
            crate::observability::names::WRONG_LANGUAGE_SKIP,
            "instance" => alternate.name.clone(),
            "source" => source_label(alternate.kind),
            "expected_language" => alternate.language.clone(),
            "detected_language" => detected_label,
        )
        .increment(1);
        return Ok(());
    }
    let mgr = registry.link_manager(&alternate.name)?;
    link_movie_with_log(mgr, source_path, folder_name, &alternate.name).await
}

async fn link_movie_with_log(
    mgr: &LinkManager,
    source: &Path,
    folder_name: &str,
    target_name: &str,
) -> Result<(), HandlerError> {
    let action = mgr.link_movie_from(source, folder_name).await?;
    if action == crate::link::LinkAction::Created {
        metrics::counter!(crate::observability::names::LINKS_CREATED,
            "instance" => target_name.to_owned(),
            "strategy" => strategy_label(mgr.strategy()),
        )
        .increment(1);
    }
    info!(target = %target_name, ?action, "movie linked");
    Ok(())
}

async fn unlink_radarr_targets<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    instance: &InstanceConfig,
    detection: &DetectionResult,
    folder_name: &str,
) -> Result<(), HandlerError> {
    let targets: Vec<&InstanceConfig> = if registry.is_primary(instance) && detection.is_multi_audio
    {
        registry.instances_for_languages(InstanceKind::Radarr, &detection.languages)
    } else {
        vec![instance]
    };
    for target in targets {
        let mgr = registry.link_manager(&target.name)?;
        mgr.unlink_movie(folder_name).await?;
        info!(target = %target.name, "upgraded — old link removed");
    }
    Ok(())
}

// =====================================================================
// Sonarr
// =====================================================================

pub async fn handle_sonarr_download<P: FfprobeProber>(
    instance: &InstanceConfig,
    event: &SonarrDownload,
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
        "sonarr_download",
        instance = %instance.name,
        tvdb_id = series_ref.tvdb_id,
        episode_file_id = episode_file_ref.id,
        is_upgrade = event.is_upgrade,
    );
    async move {
        // Extract paths from webhook payload.
        let file_path = episode_file_ref
            .path
            .as_deref()
            .ok_or(HandlerError::MissingField("episode_file.path"))?;

        let series_path = series_ref
            .path
            .as_deref()
            .ok_or(HandlerError::MissingField("series.path"))?;
        let series_folder_name = Path::new(series_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| HandlerError::MalformedPath(PathBuf::from(series_path)))?;

        let episode_relative = episode_file_ref
            .relative_path
            .as_deref()
            .ok_or(HandlerError::MissingField("episode_file.relative_path"))?;

        let mut relative_path = PathBuf::from(series_folder_name);
        relative_path.push(episode_relative);

        // Reject paths outside the configured media root.
        let file = Path::new(file_path);
        if !file.starts_with(&registry.config.media_base_path) {
            return Err(HandlerError::MalformedPath(file.to_path_buf()));
        }

        // ffprobe is the single source of truth for language detection.
        let mut detection = registry.detector.detect(file).await?;

        if detection.languages.is_empty() {
            info!(
                instance = %instance.name,
                language = %instance.language,
                "no language tags — assuming instance language"
            );
            metrics::counter!(
                crate::observability::names::LANGUAGE_TAG_FALLBACK,
                "instance" => instance.name.clone(),
                "source" => source_label(instance.kind),
                "fallback_language" => instance.language.clone(),
            )
            .increment(1);
            detection.languages = [instance.language.clone()].into();
            detection.is_multi_audio = false;
        }

        info!(
            languages = ?detection.languages,
            is_multi_audio = detection.is_multi_audio,
            "language detection complete",
        );

        let source_path = instance.storage_path.join(&relative_path);

        if event.is_upgrade {
            unlink_sonarr_targets(registry, instance, &detection, &relative_path).await?;
        }

        if registry.is_primary(instance) {
            link_sonarr_primary(registry, instance, &detection, &source_path, &relative_path)
                .await?;
            if !detection.is_multi_audio {
                propagate_add_series(registry, instance, series_ref).await?;
            }
        } else {
            link_sonarr_alternate(registry, instance, &detection, &source_path, &relative_path)
                .await?;
        }

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

async fn link_sonarr_primary<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    primary: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    relative_path: &Path,
) -> Result<(), HandlerError> {
    if detection.is_multi_audio {
        let targets = registry.instances_for_languages(InstanceKind::Sonarr, &detection.languages);
        info!(
            target_count = targets.len(),
            "primary multi-audio episode import → linking into matching language libraries"
        );
        for target in targets {
            let mgr = registry.link_manager(&target.name)?;
            link_episode_with_log(mgr, source_path, relative_path, &target.name).await?;
        }
    } else {
        let mgr = registry.link_manager(&primary.name)?;
        link_episode_with_log(mgr, source_path, relative_path, &primary.name).await?;
    }
    Ok(())
}

async fn link_sonarr_alternate<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    alternate: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    relative_path: &Path,
) -> Result<(), HandlerError> {
    if !detection.languages.contains(&alternate.language) {
        warn!(
            instance = %alternate.name,
            language = %alternate.language,
            detected = ?detection.languages,
            "alternate sonarr imported file that does not contain its own language — skipping"
        );
        let mut detected_sorted: Vec<&String> = detection.languages.iter().collect();
        detected_sorted.sort();
        let detected_label = detected_sorted
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        metrics::counter!(
            crate::observability::names::WRONG_LANGUAGE_SKIP,
            "instance" => alternate.name.clone(),
            "source" => source_label(alternate.kind),
            "expected_language" => alternate.language.clone(),
            "detected_language" => detected_label,
        )
        .increment(1);
        return Ok(());
    }
    let mgr = registry.link_manager(&alternate.name)?;
    link_episode_with_log(mgr, source_path, relative_path, &alternate.name).await
}

async fn link_episode_with_log(
    mgr: &LinkManager,
    source: &Path,
    relative: &Path,
    target_name: &str,
) -> Result<(), HandlerError> {
    let action = mgr.link_episode_from(source, relative).await?;
    if action == crate::link::LinkAction::Created {
        metrics::counter!(crate::observability::names::LINKS_CREATED,
            "instance" => target_name.to_owned(),
            "strategy" => strategy_label(mgr.strategy()),
        )
        .increment(1);
    }
    info!(target = %target_name, ?action, "episode linked");
    Ok(())
}

async fn unlink_sonarr_targets<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    instance: &InstanceConfig,
    detection: &DetectionResult,
    relative_path: &Path,
) -> Result<(), HandlerError> {
    let targets: Vec<&InstanceConfig> = if registry.is_primary(instance) && detection.is_multi_audio
    {
        registry.instances_for_languages(InstanceKind::Sonarr, &detection.languages)
    } else {
        vec![instance]
    };
    for target in targets {
        let mgr = registry.link_manager(&target.name)?;
        mgr.unlink_episode(relative_path).await?;
        info!(target = %target.name, "upgraded — old episode link removed");
    }
    Ok(())
}
