//! Filesystem reconciliation — walk storage, ffprobe, recreate links.
//!
//! Two modes:
//! - `dry_run = false`: create/update symlinks/hardlinks
//! - `dry_run = true`: log what would be done, return manifest

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::fs;
use tracing::info;
use utoipa::ToSchema;

use crate::config::{InstanceConfig, InstanceKind};
use crate::detection::{DetectionResult, FfprobeProber, LanguageDetector};
use crate::link::LinkManager;

#[derive(Debug, Serialize, ToSchema)]
pub struct RegenerateResult {
    pub dry_run: bool,
    pub scanned: usize,
    pub linked: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub actions: Vec<RegenerateAction>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegenerateAction {
    pub instance: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    pub languages: Vec<String>,
    pub is_multi_audio: bool,
}

/// Shared context threaded through the regeneration walk to keep
/// function signatures under clippy's argument limit.
struct WalkCtx<'a, P: FfprobeProber> {
    all_instances: &'a [InstanceConfig],
    detector: &'a LanguageDetector<P>,
    link_managers: &'a [(String, LinkManager)],
    primary_language: &'a str,
    dry_run: bool,
}

/// Walk all storage paths, ffprobe each file, recreate links.
///
/// Takes explicit params rather than `HandlerRegistry` so the API
/// layer can build what it needs from DB state.
pub async fn regenerate_all<P: FfprobeProber>(
    instances: &[InstanceConfig],
    detector: &LanguageDetector<P>,
    link_managers: &[(String, LinkManager)],
    primary_language: &str,
    dry_run: bool,
) -> RegenerateResult {
    let mut result = RegenerateResult {
        dry_run,
        scanned: 0,
        linked: 0,
        skipped: 0,
        errors: vec![],
        actions: vec![],
    };

    let ctx = WalkCtx {
        all_instances: instances,
        detector,
        link_managers,
        primary_language,
        dry_run,
    };

    for instance in instances {
        match instance.kind {
            InstanceKind::Radarr => {
                regenerate_movies(instance, &ctx, &mut result).await;
            }
            InstanceKind::Sonarr => {
                regenerate_episodes(instance, &ctx, &mut result).await;
            }
        }
    }

    info!(
        dry_run,
        scanned = result.scanned,
        linked = result.linked,
        skipped = result.skipped,
        errors = result.errors.len(),
        "regeneration complete"
    );

    result
}

/// Walk a Radarr instance's storage — each subdirectory is a movie folder.
async fn regenerate_movies<P: FfprobeProber>(
    instance: &InstanceConfig,
    ctx: &WalkCtx<'_, P>,
    result: &mut RegenerateResult,
) {
    let mut entries = match fs::read_dir(&instance.storage_path).await {
        Ok(e) => e,
        Err(e) => {
            result
                .errors
                .push(format!("{}: cannot read storage: {e}", instance.name));
            return;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(folder_name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };

        let Some(file_path) = find_media_file(&path).await else {
            result.skipped += 1;
            continue;
        };

        result.scanned += 1;

        let detection = match ctx.detector.detect(&file_path).await {
            Ok(d) => d,
            Err(e) => {
                result.errors.push(format!(
                    "{}/{folder_name}: ffprobe failed: {e}",
                    instance.name
                ));
                continue;
            }
        };

        if detection.languages.is_empty() {
            result.skipped += 1;
            continue;
        }

        // Audio-truth gate (deliberately observe-only — see
        // plan/decisions/audio-gate-stays-observe-only.md). Inventory on-disk
        // files with no language-appropriate <=5.1 base track; linking is
        // intentionally unchanged (tags unreliable, last-resort language allowed
        // by design).
        if !ctx
            .detector
            .has_base_audio_track(&detection.audio_streams, &instance.language)
        {
            metrics::counter!(
                crate::observability::names::AUDIO_SKIPPED,
                "instance" => instance.name.clone(),
                "source" => "radarr",
            )
            .increment(1);
            tracing::warn!(
                file = %file_path.display(),
                instance = %instance.name,
                "audio gate: no language-appropriate <=5.1 base track"
            );
        }

        let source_path = instance.storage_path.join(&folder_name);
        let targets = resolve_targets(
            instance,
            ctx.all_instances,
            &detection,
            ctx.primary_language,
        );

        let spec = LinkSpec {
            detection: &detection,
            source_path: &source_path,
            display_name: &folder_name,
            relative_episode: None,
            source_instance: instance,
        };
        apply_links(&targets, ctx, &spec, result).await;
    }
}

/// Walk a Sonarr instance's storage — series/Season XX/*.mkv
async fn regenerate_episodes<P: FfprobeProber>(
    instance: &InstanceConfig,
    ctx: &WalkCtx<'_, P>,
    result: &mut RegenerateResult,
) {
    let mut series_entries = match fs::read_dir(&instance.storage_path).await {
        Ok(e) => e,
        Err(e) => {
            result
                .errors
                .push(format!("{}: cannot read storage: {e}", instance.name));
            return;
        }
    };

    while let Ok(Some(series_entry)) = series_entries.next_entry().await {
        let series_path = series_entry.path();
        if !series_path.is_dir() {
            continue;
        }

        let Some(series_name) = series_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
        else {
            continue;
        };

        let Ok(mut season_entries) = fs::read_dir(&series_path).await else {
            continue;
        };

        while let Ok(Some(season_entry)) = season_entries.next_entry().await {
            let season_path = season_entry.path();
            if !season_path.is_dir() {
                continue;
            }

            let Some(season_name) = season_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
            else {
                continue;
            };

            walk_season_files(
                instance,
                ctx,
                result,
                &season_path,
                &series_name,
                &season_name,
            )
            .await;
        }
    }
}

/// Process all media files in a single season directory.
async fn walk_season_files<P: FfprobeProber>(
    instance: &InstanceConfig,
    ctx: &WalkCtx<'_, P>,
    result: &mut RegenerateResult,
    season_path: &Path,
    series_name: &str,
    season_name: &str,
) {
    let Ok(mut file_entries) = fs::read_dir(season_path).await else {
        return;
    };

    while let Ok(Some(file_entry)) = file_entries.next_entry().await {
        let file_path = file_entry.path();
        if !is_media_file(&file_path) {
            continue;
        }

        result.scanned += 1;

        let detection = match ctx.detector.detect(&file_path).await {
            Ok(d) => d,
            Err(e) => {
                let fname = file_path.file_name().unwrap_or_default().to_string_lossy();
                result.errors.push(format!(
                    "{}/{series_name}/{season_name}/{fname}: ffprobe failed: {e}",
                    instance.name,
                ));
                continue;
            }
        };

        if detection.languages.is_empty() {
            result.skipped += 1;
            continue;
        }

        // Audio-truth gate (deliberately observe-only — see
        // plan/decisions/audio-gate-stays-observe-only.md). Inventory on-disk
        // files with no language-appropriate <=5.1 base track; linking is
        // intentionally unchanged (tags unreliable, last-resort language allowed
        // by design).
        if !ctx
            .detector
            .has_base_audio_track(&detection.audio_streams, &instance.language)
        {
            metrics::counter!(
                crate::observability::names::AUDIO_SKIPPED,
                "instance" => instance.name.clone(),
                "source" => "sonarr",
            )
            .increment(1);
            tracing::warn!(
                file = %file_path.display(),
                instance = %instance.name,
                "audio gate: no language-appropriate <=5.1 base track"
            );
        }

        let file_name = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let relative = Path::new(series_name).join(season_name).join(&file_name);
        let source_path = instance.storage_path.join(&relative);
        let targets = resolve_targets(
            instance,
            ctx.all_instances,
            &detection,
            ctx.primary_language,
        );

        let lossy = relative.to_string_lossy();
        let spec = LinkSpec {
            detection: &detection,
            source_path: &source_path,
            display_name: &lossy,
            relative_episode: Some(&relative),
            source_instance: instance,
        };
        apply_links(&targets, ctx, &spec, result).await;
    }
}

/// Determine which instances should receive a link for detected media.
///
/// Mirrors the import handler exactly (see `handler::import::primary_link_targets`
/// and `link_sonarr_alternate`). The two used to diverge: reconcile fanned out
/// *any* multi-audio file, so an alternate's `MULTi` release was linked into the
/// primary's library on every regenerate, while the live import path never did
/// that. Keeping one rule in two places is how a sweep silently undoes itself.
fn resolve_targets<'a>(
    source_instance: &'a InstanceConfig,
    all_instances: &'a [InstanceConfig],
    detection: &DetectionResult,
    primary_language: &str,
) -> Vec<&'a InstanceConfig> {
    if source_instance.language == primary_language {
        let mut targets: Vec<&InstanceConfig> = all_instances
            .iter()
            .filter(|i| i.kind == source_instance.kind && detection.languages.contains(&i.language))
            .collect();
        if !targets.iter().any(|i| i.name == source_instance.name) {
            targets.push(source_instance);
        }
        return targets;
    }

    // Alternate instances only ever serve their own library, and only when the
    // file actually carries their language.
    if !detection.languages.contains(&source_instance.language) {
        return vec![];
    }
    all_instances
        .iter()
        .filter(|i| i.name == source_instance.name)
        .collect()
}

/// Describes a single detected media file ready for linking.
struct LinkSpec<'a> {
    detection: &'a DetectionResult,
    source_path: &'a Path,
    display_name: &'a str,
    /// `Some` for episodes (file-level link), `None` for movies
    /// (directory-level link via `display_name`).
    relative_episode: Option<&'a Path>,
    source_instance: &'a InstanceConfig,
}

/// Apply link operations (or record dry-run actions) for each target instance.
async fn apply_links<P: FfprobeProber>(
    targets: &[&InstanceConfig],
    ctx: &WalkCtx<'_, P>,
    spec: &LinkSpec<'_>,
    result: &mut RegenerateResult,
) {
    let kind_label = if spec.relative_episode.is_some() {
        "episode"
    } else {
        "movie"
    };
    let languages: Vec<String> = sort_languages(&spec.detection.languages);

    for target in targets {
        let Some((_, mgr)) = ctx.link_managers.iter().find(|(n, _)| n == &target.name) else {
            continue;
        };

        let action = RegenerateAction {
            instance: target.name.clone(),
            source: spec.source_path.display().to_string(),
            target: if let Some(rel) = spec.relative_episode {
                target.library_path.join(rel).display().to_string()
            } else {
                target
                    .library_path
                    .join(spec.display_name)
                    .display()
                    .to_string()
            },
            kind: kind_label.to_owned(),
            languages: languages.clone(),
            is_multi_audio: spec.detection.is_multi_audio,
        };

        // One file per SxxEyy per library, using the same keep-policy as the
        // import handler. Without this a regenerate would resurrect every
        // duplicate the handler resolved — and would silently undo a sweep.
        let evict = match episode_conflict(mgr, ctx.all_instances, spec, target).await {
            Conflict::Proceed(evict) => evict,
            Conflict::Skip => {
                metrics::counter!(
                    crate::observability::names::DUPLICATE_LINK_SKIPPED,
                    "instance" => target.name.clone(),
                    "outcome" => "skipped",
                )
                .increment(1);
                result.skipped += 1;
                continue;
            }
            Conflict::Failed(e) => {
                result
                    .errors
                    .push(format!("{}: dedup scan failed: {e}", target.name));
                continue;
            }
        };

        if ctx.dry_run {
            result.actions.push(action);
            result.linked += 1;
            continue;
        }

        if let Some(existing) = evict {
            if let Err(e) = mgr.unlink_absolute(&existing).await {
                result.errors.push(format!(
                    "{}: evict {} failed: {e}",
                    target.name,
                    existing.display()
                ));
                continue;
            }
            metrics::counter!(
                crate::observability::names::DUPLICATE_LINK_SKIPPED,
                "instance" => target.name.clone(),
                "outcome" => "replaced",
            )
            .increment(1);
        }

        let link_result = if let Some(rel) = spec.relative_episode {
            mgr.link_episode_from(spec.source_path, rel).await
        } else {
            mgr.link_movie_from(spec.source_path, spec.display_name)
                .await
        };

        match link_result {
            Ok(_) => {
                result.actions.push(action);
                result.linked += 1;
            }
            Err(e) => {
                result.errors.push(format!(
                    "{}/{} -> {}: {e}",
                    spec.source_instance.name, spec.display_name, target.name
                ));
            }
        }
    }
}

/// Outcome of the per-library episode de-duplication check.
enum Conflict {
    /// Link may be created; `Some(path)` is an incumbent link to evict first.
    Proceed(Option<PathBuf>),
    /// An incumbent release wins this library — leave it alone.
    Skip,
    /// The scan itself failed; the caller records it and moves on.
    Failed(String),
}

/// Apply `link::dedup_verdict` to a candidate episode link during the walk.
/// Movies never conflict: their library entries are directory symlinks named
/// `Title (Year)`, so an en/fr twin collides on name and cannot coexist.
async fn episode_conflict(
    mgr: &LinkManager,
    all_instances: &[InstanceConfig],
    spec: &LinkSpec<'_>,
    target: &InstanceConfig,
) -> Conflict {
    let Some(rel) = spec.relative_episode else {
        return Conflict::Proceed(None);
    };
    let Some((season, number)) = rel
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(crate::link::parse_season_episode)
    else {
        // No SxxEyy in the filename — nothing to key de-duplication on.
        return Conflict::Proceed(None);
    };

    match mgr.find_conflicting_episode_link(rel, season, number).await {
        Ok(None) => Conflict::Proceed(None),
        Ok(Some(existing)) => {
            let incumbent = owner_instance(all_instances, &existing).await;
            match crate::link::dedup_verdict(incumbent, spec.source_instance, target) {
                crate::link::DedupVerdict::Skip => Conflict::Skip,
                crate::link::DedupVerdict::Replace => Conflict::Proceed(Some(existing)),
                crate::link::DedupVerdict::Link => Conflict::Proceed(None),
            }
        }
        Err(e) => Conflict::Failed(e.to_string()),
    }
}

/// The instance whose storage backs `link`, or `None` when the link cannot be
/// resolved (hardlink strategy, or a target outside every configured storage).
/// Mirrors `handler::import::owner_instance` — both feed `link::dedup_verdict`.
async fn owner_instance<'a>(
    all_instances: &'a [InstanceConfig],
    link: &Path,
) -> Option<&'a InstanceConfig> {
    let target = fs::read_link(link).await.ok()?;
    all_instances
        .iter()
        .find(|i| target.starts_with(&i.storage_path))
}

/// Sort language keys for deterministic output.
fn sort_languages(languages: &HashSet<String>) -> Vec<String> {
    let mut sorted: Vec<String> = languages.iter().cloned().collect();
    sorted.sort();
    sorted
}

async fn find_media_file(dir: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if is_media_file(&path) {
            return Some(path);
        }
    }
    None
}

fn is_media_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("mkv" | "mp4" | "avi" | "ts" | "m4v" | "wmv" | "flv" | "webm")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::fs;

    use super::*;
    use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};
    use crate::detection::{AudioStream, DetectionError, FfprobeProber, LanguageDetector};
    use crate::link::LinkManager;

    #[derive(Debug, Clone)]
    struct StubFfprobe(Vec<AudioStream>);

    impl FfprobeProber for StubFfprobe {
        async fn probe(
            &self,
            _path: &Path,
            _timeout: Duration,
        ) -> Result<Vec<AudioStream>, DetectionError> {
            Ok(self.0.clone())
        }
    }

    fn en_fr_config() -> Arc<crate::config::LanguagesConfig> {
        use crate::config::{LanguageDefinition, LanguagesConfig};
        use std::collections::HashMap;

        let mut defs = HashMap::new();
        defs.insert(
            "fr".to_owned(),
            LanguageDefinition {
                iso_639_1: vec!["fr".to_owned()],
                iso_639_2: vec!["fra".to_owned(), "fre".to_owned()],
                radarr_id: 2,
                sonarr_id: 2,
            },
        );
        defs.insert(
            "en".to_owned(),
            LanguageDefinition {
                iso_639_1: vec!["en".to_owned()],
                iso_639_2: vec!["eng".to_owned()],
                radarr_id: 1,
                sonarr_id: 1,
            },
        );
        Arc::new(LanguagesConfig {
            primary: "fr".to_owned(),
            alternates: vec!["en".to_owned()],
            definitions: defs,
        })
    }

    fn multi_audio_streams() -> Vec<AudioStream> {
        vec![
            AudioStream {
                language: Some("eng".to_owned()),
                channels: None,
                is_commentary: false,
            },
            AudioStream {
                language: Some("fre".to_owned()),
                channels: None,
                is_commentary: false,
            },
        ]
    }

    fn fr_only_streams() -> Vec<AudioStream> {
        vec![AudioStream {
            language: Some("fre".to_owned()),
            channels: None,
            is_commentary: false,
        }]
    }

    fn make_instance(
        name: &str,
        kind: InstanceKind,
        lang: &str,
        storage: &Path,
        library: &Path,
    ) -> InstanceConfig {
        InstanceConfig {
            name: name.to_owned(),
            kind,
            language: lang.to_owned(),
            url: "http://unused".to_owned(),
            api_key: "k".to_owned(),
            storage_path: storage.to_path_buf(),
            library_path: library.to_path_buf(),
            link_strategy: LinkStrategy::Symlink,
            propagate_delete: true,
        }
    }

    #[tokio::test]
    async fn regenerate_movies_dry_run_reports_actions_without_linking() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("storage-fr");
        let library = tmp.path().join("library-fr");
        fs::create_dir_all(&storage).await.unwrap();
        fs::create_dir_all(&library).await.unwrap();

        // Create a movie in storage.
        let movie_dir = storage.join("Test Movie (2024)");
        fs::create_dir_all(&movie_dir).await.unwrap();
        fs::write(movie_dir.join("movie.mkv"), "content")
            .await
            .unwrap();

        let inst = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &storage, &library);
        let mgr = LinkManager::from_instance(&inst);
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(fr_only_streams()));

        let result = regenerate_all(
            &[inst.clone()],
            &detector,
            &[(inst.name.clone(), mgr)],
            "fr",
            true, // dry_run
        )
        .await;

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.scanned, 1);
        assert_eq!(result.linked, 1);
        assert!(result.dry_run);
        assert_eq!(result.actions.len(), 1);
        assert_eq!(result.actions[0].instance, "radarr-fr");

        // Dry run: no actual symlink created.
        assert!(!fs::try_exists(library.join("Test Movie (2024)"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn regenerate_movies_live_creates_symlinks() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("storage-fr");
        let library = tmp.path().join("library-fr");
        fs::create_dir_all(&storage).await.unwrap();
        fs::create_dir_all(&library).await.unwrap();

        let movie_dir = storage.join("Test Movie (2024)");
        fs::create_dir_all(&movie_dir).await.unwrap();
        fs::write(movie_dir.join("movie.mkv"), "content")
            .await
            .unwrap();

        let inst = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &storage, &library);
        let mgr = LinkManager::from_instance(&inst);
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(fr_only_streams()));

        let result = regenerate_all(
            &[inst.clone()],
            &detector,
            &[(inst.name.clone(), mgr)],
            "fr",
            false, // live
        )
        .await;

        assert_eq!(result.scanned, 1);
        assert_eq!(result.linked, 1);
        assert!(!result.dry_run);
        assert!(result.errors.is_empty());

        // Symlink created.
        let link = library.join("Test Movie (2024)");
        assert!(fs::try_exists(&link).await.unwrap());
        let target = fs::read_link(&link).await.unwrap();
        assert!(target.starts_with(&storage));
    }

    #[tokio::test]
    async fn regenerate_no_base_audio_track_increments_counter_observe_only() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("storage-fr");
        let library = tmp.path().join("library-fr");
        fs::create_dir_all(&storage).await.unwrap();
        fs::create_dir_all(&library).await.unwrap();

        let movie_dir = storage.join("Liar 7.1 (2024)");
        fs::create_dir_all(&movie_dir).await.unwrap();
        fs::write(movie_dir.join("movie.mkv"), "content")
            .await
            .unwrap();

        let inst = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &storage, &library);
        let mgr = LinkManager::from_instance(&inst);
        // fr main track but 7.1 (8ch) — no <=5.1 base in the instance language
        let streams = vec![AudioStream {
            language: Some("fre".to_owned()),
            channels: Some(8),
            is_commentary: false,
        }];
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(streams));

        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let recorder_guard = metrics::set_default_local_recorder(&recorder);

        let result = regenerate_all(
            &[inst.clone()],
            &detector,
            &[(inst.name.clone(), mgr)],
            "fr",
            false,
        )
        .await;

        drop(recorder_guard);
        let render = handle.render();
        assert!(
            render.contains(
                "multilinguarr_audio_skipped_total{instance=\"radarr-fr\",source=\"radarr\"} 1"
            ),
            "expected audio-skip counter in:\n{render}"
        );
        // observe-only: the link is still created
        assert_eq!(result.linked, 1);
        assert!(fs::try_exists(library.join("Liar 7.1 (2024)"))
            .await
            .unwrap());
    }

    /// A regenerate must CONVERGE each library to one link per `SxxEyy`, and the
    /// survivor must not depend on the order storage happens to be walked.
    /// Without this the admin regenerate endpoint resurrects every duplicate
    /// the import handler resolved — and silently undoes a manual sweep.
    /// Returns the `TempDir` alongside the paths: dropping it would delete the
    /// tree before the caller can assert on it.
    async fn run_dedup_regenerate(fr_first: bool) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let storage_fr = tmp.path().join("storage-fr");
        let library_fr = tmp.path().join("library-fr");
        let storage_en = tmp.path().join("storage-en");
        let library_en = tmp.path().join("library-en");
        for d in [&storage_fr, &library_fr, &storage_en, &library_en] {
            fs::create_dir_all(d).await.unwrap();
        }

        // Two different releases of the SAME episode, one per instance.
        let fr_season = storage_fr.join("Show").join("Season 01");
        fs::create_dir_all(&fr_season).await.unwrap();
        fs::write(fr_season.join("S01E01.MULTi-TyHD.mkv"), "multi")
            .await
            .unwrap();
        let en_season = storage_en.join("Show").join("Season 01");
        fs::create_dir_all(&en_season).await.unwrap();
        fs::write(en_season.join("S01E01.EN-EDITH.mkv"), "english")
            .await
            .unwrap();

        let inst_fr = make_instance(
            "sonarr-fr",
            InstanceKind::Sonarr,
            "fr",
            &storage_fr,
            &library_fr,
        );
        let inst_en = make_instance(
            "sonarr-en",
            InstanceKind::Sonarr,
            "en",
            &storage_en,
            &library_en,
        );
        let mgr_fr = LinkManager::from_instance(&inst_fr);
        let mgr_en = LinkManager::from_instance(&inst_en);
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(multi_audio_streams()));

        let instances = if fr_first {
            vec![inst_fr.clone(), inst_en.clone()]
        } else {
            vec![inst_en.clone(), inst_fr.clone()]
        };
        let managers = vec![
            (inst_fr.name.clone(), mgr_fr),
            (inst_en.name.clone(), mgr_en),
        ];

        let result = regenerate_all(&instances, &detector, &managers, "fr", false).await;
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        (tmp, library_en, library_fr)
    }

    #[tokio::test]
    async fn regenerate_dedupes_episode_links_regardless_of_walk_order() {
        for fr_first in [true, false] {
            let (_tmp, library_en, library_fr) = run_dedup_regenerate(fr_first).await;

            let en_native = library_en.join("Show/Season 01/S01E01.EN-EDITH.mkv");
            let en_multi = library_en.join("Show/Season 01/S01E01.MULTi-TyHD.mkv");
            assert!(
                fs::try_exists(&en_native).await.unwrap(),
                "fr_first={fr_first}: english library must keep the native release"
            );
            assert!(
                !fs::try_exists(&en_multi).await.unwrap(),
                "fr_first={fr_first}: regenerate must not resurrect the duplicate"
            );

            // The French library is served by the only fr-capable file.
            assert!(
                fs::try_exists(library_fr.join("Show/Season 01/S01E01.MULTi-TyHD.mkv"))
                    .await
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn regenerate_multi_audio_links_to_both_instances() {
        let tmp = TempDir::new().unwrap();
        let storage_fr = tmp.path().join("storage-fr");
        let library_fr = tmp.path().join("library-fr");
        let storage_en = tmp.path().join("storage-en");
        let library_en = tmp.path().join("library-en");
        for d in [&storage_fr, &library_fr, &storage_en, &library_en] {
            fs::create_dir_all(d).await.unwrap();
        }

        let movie_dir = storage_fr.join("Multi (2024)");
        fs::create_dir_all(&movie_dir).await.unwrap();
        fs::write(movie_dir.join("movie.mkv"), "content")
            .await
            .unwrap();

        let inst_fr = make_instance(
            "radarr-fr",
            InstanceKind::Radarr,
            "fr",
            &storage_fr,
            &library_fr,
        );
        let inst_en = make_instance(
            "radarr-en",
            InstanceKind::Radarr,
            "en",
            &storage_en,
            &library_en,
        );
        let mgr_fr = LinkManager::from_instance(&inst_fr);
        let mgr_en = LinkManager::from_instance(&inst_en);
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(multi_audio_streams()));

        let instances = vec![inst_fr.clone(), inst_en.clone()];
        let managers = vec![
            (inst_fr.name.clone(), mgr_fr),
            (inst_en.name.clone(), mgr_en),
        ];

        let result = regenerate_all(&instances, &detector, &managers, "fr", false).await;

        assert_eq!(result.scanned, 1);
        assert_eq!(result.linked, 2);
        assert!(result.errors.is_empty());
        assert!(fs::try_exists(library_fr.join("Multi (2024)"))
            .await
            .unwrap());
        assert!(fs::try_exists(library_en.join("Multi (2024)"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn regenerate_skips_non_media_files() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("storage");
        let library = tmp.path().join("library");
        fs::create_dir_all(&storage).await.unwrap();
        fs::create_dir_all(&library).await.unwrap();

        // Movie folder with only a .nfo file — no media.
        let movie_dir = storage.join("NoMedia (2024)");
        fs::create_dir_all(&movie_dir).await.unwrap();
        fs::write(movie_dir.join("movie.nfo"), "info")
            .await
            .unwrap();

        let inst = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &storage, &library);
        let mgr = LinkManager::from_instance(&inst);
        let detector = LanguageDetector::new(en_fr_config(), StubFfprobe(fr_only_streams()));

        let result = regenerate_all(
            &[inst.clone()],
            &detector,
            &[(inst.name.clone(), mgr)],
            "fr",
            false,
        )
        .await;

        assert_eq!(result.scanned, 0);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.linked, 0);
    }

    #[tokio::test]
    async fn resolve_targets_multi_audio_returns_matching_instances() {
        let tmp = TempDir::new().unwrap();
        let s = tmp.path().join("s");
        let l = tmp.path().join("l");

        let inst_fr = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &s, &l);
        let inst_en = make_instance("radarr-en", InstanceKind::Radarr, "en", &s, &l);
        let inst_sonarr = make_instance("sonarr-fr", InstanceKind::Sonarr, "fr", &s, &l);

        let detection = DetectionResult {
            languages: HashSet::from(["fr".to_owned(), "en".to_owned()]),
            is_multi_audio: true,
            audio_streams: vec![],
        };

        let all = vec![inst_fr.clone(), inst_en.clone(), inst_sonarr];
        let targets = resolve_targets(&inst_fr, &all, &detection, "fr");

        // Only Radarr instances with matching languages.
        assert_eq!(targets.len(), 2);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"radarr-fr"));
        assert!(names.contains(&"radarr-en"));
    }

    #[tokio::test]
    async fn resolve_targets_single_language_returns_source_only() {
        let tmp = TempDir::new().unwrap();
        let s = tmp.path().join("s");
        let l = tmp.path().join("l");

        let inst_fr = make_instance("radarr-fr", InstanceKind::Radarr, "fr", &s, &l);
        let inst_en = make_instance("radarr-en", InstanceKind::Radarr, "en", &s, &l);

        let detection = DetectionResult {
            languages: HashSet::from(["fr".to_owned()]),
            is_multi_audio: false,
            audio_streams: vec![],
        };

        let all = vec![inst_fr.clone(), inst_en];
        let targets = resolve_targets(&inst_fr, &all, &detection, "fr");

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "radarr-fr");
    }
}
