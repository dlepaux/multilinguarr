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
use crate::webhook::{RadarrDownload, SonarrDownload, SonarrSeriesRef};

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

        // Audio-truth gate (deliberately observe-only — see
        // plan/decisions/audio-gate-stays-observe-only.md). The counter is the
        // alertable signal for human review; linking is intentionally NOT gated
        // on it, because in-file language tags are unreliable (mis-tagged multi
        // releases) and the primary profile may allow a last-resort fallback.
        if !registry
            .detector
            .has_base_audio_track(&detection.audio_streams, &instance.language)
        {
            metrics::counter!(
                crate::observability::names::AUDIO_SKIPPED,
                "instance" => instance.name.clone(),
                "source" => source_label(instance.kind),
            )
            .increment(1);
            warn!(
                file = %file.display(),
                instance = %instance.name,
                "audio gate: no language-appropriate <=5.1 base track"
            );
        }

        let source_path = instance.storage_path.join(folder_name);

        if event.is_upgrade {
            unlink_radarr_targets(registry, instance, &detection, folder_name).await?;
        }

        if registry.is_primary(instance) {
            link_radarr_primary(registry, instance, &detection, &source_path, folder_name).await?;
            propagate_add_movie(registry, instance, movie_ref, &detection.languages).await?;
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
    for target in primary_link_targets(registry, primary, detection, InstanceKind::Radarr) {
        let mgr = registry.link_manager(&target.name)?;
        link_movie_with_log(mgr, source_path, folder_name, &target.name).await?;
    }
    Ok(())
}

/// Libraries that should receive a link for a file the *primary* imported.
///
/// Language-driven, not role-driven: every instance whose language the file
/// actually carries gets a link to the one storage copy. The primary's own
/// library is always included — it is where the operator requested the media,
/// even when the file carries none of the primary's language (a VOSTFR grab:
/// English audio, French subtitles).
fn primary_link_targets<'a, P: FfprobeProber>(
    registry: &'a HandlerRegistry<P>,
    primary: &'a InstanceConfig,
    detection: &DetectionResult,
    kind: InstanceKind,
) -> Vec<&'a InstanceConfig> {
    let mut targets = registry.instances_for_languages(kind, &detection.languages);
    if !targets.iter().any(|t| t.name == primary.name) {
        targets.push(primary);
    }
    info!(
        target_count = targets.len(),
        is_multi_audio = detection.is_multi_audio,
        "primary import → linking into every library this file can serve"
    );
    targets
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

        // Audio-truth gate (deliberately observe-only — see
        // plan/decisions/audio-gate-stays-observe-only.md). The counter is the
        // alertable signal for human review; linking is intentionally NOT gated
        // on it, because in-file language tags are unreliable (mis-tagged multi
        // releases) and the primary profile may allow a last-resort fallback.
        if !registry
            .detector
            .has_base_audio_track(&detection.audio_streams, &instance.language)
        {
            metrics::counter!(
                crate::observability::names::AUDIO_SKIPPED,
                "instance" => instance.name.clone(),
                "source" => source_label(instance.kind),
            )
            .increment(1);
            warn!(
                file = %file.display(),
                instance = %instance.name,
                "audio gate: no language-appropriate <=5.1 base track"
            );
        }

        let source_path = instance.storage_path.join(&relative_path);

        if event.is_upgrade {
            unlink_sonarr_targets(registry, instance, &detection, &relative_path).await?;
        }

        let import = SonarrImport {
            series_ref,
            detection: &detection,
            source_path: &source_path,
            relative_path: &relative_path,
            // Episode identity comes from the webhook, not the filename: a
            // bare `info[EZTVx.to].mkv` carries no SxxEyy tokens of its own.
            episode: event
                .episodes
                .first()
                .map(|e| (e.season_number, e.episode_number)),
        };
        dispatch_sonarr_link(registry, instance, &import).await?;

        registry.jellyfin.refresh().await;
        Ok(())
    }
    .instrument(span)
    .await
}

/// Everything one imported episode needs in order to be linked. Bundled to
/// keep the dispatch signature under clippy's argument limit.
struct SonarrImport<'a> {
    series_ref: &'a SonarrSeriesRef,
    detection: &'a DetectionResult,
    source_path: &'a Path,
    relative_path: &'a Path,
    episode: Option<(u32, u32)>,
}

/// Route an imported episode to the primary or alternate linking path, then
/// backfill any language this file cannot serve via a cross-instance add.
async fn dispatch_sonarr_link<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    instance: &InstanceConfig,
    import: &SonarrImport<'_>,
) -> Result<(), HandlerError> {
    let SonarrImport {
        series_ref,
        detection,
        source_path,
        relative_path,
        episode,
    } = *import;

    if registry.is_primary(instance) {
        link_sonarr_primary(
            registry,
            instance,
            detection,
            source_path,
            relative_path,
            episode,
        )
        .await?;
        propagate_add_series(registry, instance, series_ref, &detection.languages).await?;
    } else {
        link_sonarr_alternate(
            registry,
            instance,
            detection,
            source_path,
            relative_path,
            episode,
        )
        .await?;
    }
    Ok(())
}

async fn link_sonarr_primary<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    primary: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    relative_path: &Path,
    episode: Option<(u32, u32)>,
) -> Result<(), HandlerError> {
    for target in primary_link_targets(registry, primary, detection, InstanceKind::Sonarr) {
        link_episode_deduped(
            registry,
            primary,
            target,
            source_path,
            relative_path,
            episode,
        )
        .await?;
    }
    Ok(())
}

/// Link one episode into `target`, enforcing **one file per `SxxEyy` per
/// library**.
///
/// Two releases of the same episode (a French `MULTi` and a native English
/// grab) both legitimately carry English audio, so both are eligible for the
/// English library. Jellyfin has no way to stack them — release filenames
/// never match its version convention — so it renders two episodes.
///
/// The keep-policy, in order:
/// 1. An instance always replaces its **own** link (upgrade / re-import).
/// 2. Otherwise the release whose *source instance* speaks the library's
///    language wins — that is the native-audio copy.
/// 3. Ties and unidentifiable owners keep the incumbent, so the surviving
///    link does not depend on import order.
async fn link_episode_deduped<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    target: &InstanceConfig,
    source_path: &Path,
    relative_path: &Path,
    episode: Option<(u32, u32)>,
) -> Result<(), HandlerError> {
    let mgr = registry.link_manager(&target.name)?;

    if let Some((season, number)) = episode {
        if let Some(existing) = mgr
            .find_conflicting_episode_link(relative_path, season, number)
            .await?
        {
            let incumbent = owner_instance(registry, &existing).await;
            match crate::link::dedup_verdict(incumbent, source_instance, target) {
                crate::link::DedupVerdict::Skip => {
                    metrics::counter!(
                        crate::observability::names::DUPLICATE_LINK_SKIPPED,
                        "instance" => target.name.clone(),
                        "outcome" => "skipped",
                    )
                    .increment(1);
                    info!(
                        target = %target.name,
                        existing = %existing.display(),
                        "episode already linked from another release — skipping duplicate"
                    );
                    return Ok(());
                }
                crate::link::DedupVerdict::Replace => {
                    mgr.unlink_absolute(&existing).await?;
                    metrics::counter!(
                        crate::observability::names::DUPLICATE_LINK_SKIPPED,
                        "instance" => target.name.clone(),
                        "outcome" => "replaced",
                    )
                    .increment(1);
                    info!(
                        target = %target.name,
                        evicted = %existing.display(),
                        "evicted losing link in favour of this release"
                    );
                }
                crate::link::DedupVerdict::Link => {}
            }
        }
    }

    link_episode_with_log(mgr, source_path, relative_path, &target.name).await
}

/// The instance whose storage backs `link`, or `None` when the link cannot be
/// resolved (hardlink strategy, or a target outside every configured storage).
async fn owner_instance<'a, P: FfprobeProber>(
    registry: &'a HandlerRegistry<P>,
    link: &Path,
) -> Option<&'a InstanceConfig> {
    let target = tokio::fs::read_link(link).await.ok()?;
    registry
        .config_instances()
        .iter()
        .find(|i| target.starts_with(&i.storage_path))
}

async fn link_sonarr_alternate<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    alternate: &InstanceConfig,
    detection: &DetectionResult,
    source_path: &Path,
    relative_path: &Path,
    episode: Option<(u32, u32)>,
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
    link_episode_deduped(
        registry,
        alternate,
        alternate,
        source_path,
        relative_path,
        episode,
    )
    .await
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
