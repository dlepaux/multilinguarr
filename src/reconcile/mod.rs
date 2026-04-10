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

        let source_path = instance.storage_path.join(&folder_name);
        let targets = resolve_targets(instance, ctx.all_instances, &detection);

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

        let file_name = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let relative = Path::new(series_name).join(season_name).join(&file_name);
        let source_path = instance.storage_path.join(&relative);
        let targets = resolve_targets(instance, ctx.all_instances, &detection);

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
fn resolve_targets<'a>(
    source_instance: &InstanceConfig,
    all_instances: &'a [InstanceConfig],
    detection: &DetectionResult,
) -> Vec<&'a InstanceConfig> {
    if detection.is_multi_audio {
        all_instances
            .iter()
            .filter(|i| i.kind == source_instance.kind && detection.languages.contains(&i.language))
            .collect()
    } else {
        all_instances
            .iter()
            .filter(|i| i.name == source_instance.name)
            .collect()
    }
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

        if ctx.dry_run {
            result.actions.push(action);
            result.linked += 1;
            continue;
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
            },
            AudioStream {
                language: Some("fre".to_owned()),
            },
        ]
    }

    fn fr_only_streams() -> Vec<AudioStream> {
        vec![AudioStream {
            language: Some("fre".to_owned()),
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

        let result = regenerate_all(&instances, &detector, &managers, false).await;

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
        };

        let all = vec![inst_fr.clone(), inst_en.clone(), inst_sonarr];
        let targets = resolve_targets(&inst_fr, &all, &detection);

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
        };

        let all = vec![inst_fr.clone(), inst_en];
        let targets = resolve_targets(&inst_fr, &all, &detection);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "radarr-fr");
    }
}
