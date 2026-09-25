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
    AddMovieOptions, AddMovieRequest, AddOutcome, AddSeriesOptions, AddSeriesRequest, ArrClient,
    Language, SeasonInfo,
};
use crate::config::{InstanceConfig, InstanceKind};
use crate::detection::FfprobeProber;
use crate::observability::names::CROSS_INSTANCE_ADD;
use crate::webhook::{RadarrMovieRef, SonarrSeriesRef};

// Closed set of `outcome` label values — keeps Prometheus cardinality
// bounded.
const OUTCOME_CREATED: &str = "created";
const OUTCOME_ALREADY_EXISTED: &str = "already_existed";
const OUTCOME_SKIPPED_ORIGINAL: &str = "skipped_original_language";
const OUTCOME_ERROR: &str = "error";

fn record_add_outcome(source: &str, target: &str, outcome: &'static str) {
    metrics::counter!(
        CROSS_INSTANCE_ADD,
        "instance" => source.to_owned(),
        "target" => target.to_owned(),
        "outcome" => outcome,
    )
    .increment(1);
}

/// True when the title's original language is `instance`'s own. An unknown
/// original language (a record without one) keeps the cross-add.
fn is_own_language_original<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    instance: &InstanceConfig,
    original: Option<&Language>,
) -> bool {
    let Some(original) = original else {
        return false;
    };
    let Some(own) = registry
        .config
        .languages
        .definitions
        .get(&instance.language)
    else {
        return false;
    };
    let own_id = match instance.kind {
        InstanceKind::Radarr => own.radarr_id,
        InstanceKind::Sonarr => own.sonarr_id,
    };
    original.id == own_id
}

/// Decision of record (MED-17, 2026-09-25): a title whose original language is
/// the source's own lives in the source's library only. Its siblings have no
/// dub of it to fetch, so a cross-add would sit "missing" forever.
fn skip_own_language_original(source: &InstanceConfig, targets: &[&InstanceConfig], title: &str) {
    info!(
        source = %source.name,
        title,
        "original language is the source's own — skipping cross-instance add"
    );
    for target in targets {
        record_add_outcome(&source.name, &target.name, OUTCOME_SKIPPED_ORIGINAL);
    }
}

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
    detected_languages: &std::collections::HashSet<String>,
) -> Result<(), HandlerError> {
    // Only ask a sibling to fetch a language THIS file cannot already serve.
    // A VOSTFR grab (English audio) imported by the French primary already
    // satisfies English — propagating it would download a redundant copy of
    // the same movie into the English instance.
    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| {
            i.kind == InstanceKind::Radarr
                && i.name != source_instance.name
                && i.language != source_instance.language
                && !detected_languages.contains(&i.language)
        })
        .collect();

    if targets.is_empty() {
        return Ok(());
    }
    let ArrClient::Radarr(source_radarr) = registry.client(&source_instance.name)? else {
        return Ok(());
    };
    let original = source_radarr
        .get_movie_by_tmdb_id(movie_ref.tmdb_id)
        .await?
        .and_then(|movie| movie.original_language);
    if is_own_language_original(registry, source_instance, original.as_ref()) {
        skip_own_language_original(source_instance, &targets, &movie_ref.title);
        return Ok(());
    }

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Radarr(target_radarr) = target_client else {
            continue;
        };

        // Optimisation: skip the POST→409 round-trip when we already
        // know the movie exists. Correctness no longer depends on it.
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
            record_add_outcome(&source_instance.name, &target.name, OUTCOME_ALREADY_EXISTED);
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
        match target_radarr.add_movie(&req).await {
            Ok(AddOutcome::Created(added)) => {
                info!(
                    source = %source_instance.name,
                    target = %target.name,
                    tmdb_id = movie_ref.tmdb_id,
                    added_id = added.id,
                    "cross-instance add_movie succeeded"
                );
                record_add_outcome(&source_instance.name, &target.name, OUTCOME_CREATED);
            }
            Ok(AddOutcome::AlreadyExisted(existing)) => {
                info!(
                    source = %source_instance.name,
                    target = %target.name,
                    tmdb_id = movie_ref.tmdb_id,
                    existing_id = existing.id,
                    "cross-instance add_movie absorbed 409 race — movie already exists in target"
                );
                record_add_outcome(&source_instance.name, &target.name, OUTCOME_ALREADY_EXISTED);
            }
            Err(err) => {
                record_add_outcome(&source_instance.name, &target.name, OUTCOME_ERROR);
                return Err(HandlerError::Arr(err));
            }
        }
    }
    Ok(())
}

/// The source series' seasons (to copy their monitoring) and original language.
async fn fetch_source_series(
    source_sonarr: &crate::client::SonarrClient,
    source_name: &str,
    tvdb_id: u32,
) -> Result<(Vec<SeasonInfo>, Option<Language>), HandlerError> {
    if let Some(series) = source_sonarr.get_series_by_tvdb_id(tvdb_id).await? {
        let seasons = series
            .seasons
            .into_iter()
            .map(|s| SeasonInfo {
                season_number: s.season_number,
                monitored: s.monitored,
            })
            .collect();
        Ok((seasons, series.original_language))
    } else {
        warn!(
            source = %source_name,
            tvdb_id,
            "could not get series details from source — adding with empty seasons"
        );
        Ok((vec![], None))
    }
}

async fn add_series_to_target(
    source_instance: &InstanceConfig,
    target: &InstanceConfig,
    target_sonarr: &crate::client::SonarrClient,
    series_ref: &SonarrSeriesRef,
    seasons: &[SeasonInfo],
) -> Result<(), HandlerError> {
    // Optimisation: skip the POST→409 round-trip when we already
    // know the series exists. Correctness no longer depends on it.
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
        record_add_outcome(&source_instance.name, &target.name, OUTCOME_ALREADY_EXISTED);
        return Ok(());
    }

    let profiles = target_sonarr.quality_profiles().await?;
    let folders = target_sonarr.root_folders().await?;
    let Some(profile) = profiles.first() else {
        warn!(target = %target.name, "target has no quality profiles — cannot add series");
        return Ok(());
    };
    let Some(folder) = folders.first() else {
        warn!(target = %target.name, "target has no root folders — cannot add series");
        return Ok(());
    };

    let req = AddSeriesRequest {
        title: series_ref.title.clone(),
        year: None,
        tvdb_id: series_ref.tvdb_id,
        quality_profile_id: profile.id,
        root_folder_path: folder.path.clone(),
        season_folder: true,
        monitored: true,
        seasons: seasons.to_vec(),
        add_options: AddSeriesOptions {
            search_for_missing_episodes: true,
        },
    };
    match target_sonarr.add_series(&req).await {
        Ok(AddOutcome::Created(added)) => {
            info!(
                source = %source_instance.name,
                target = %target.name,
                tvdb_id = series_ref.tvdb_id,
                added_id = added.id,
                "cross-instance add_series succeeded"
            );
            record_add_outcome(&source_instance.name, &target.name, OUTCOME_CREATED);
            Ok(())
        }
        Ok(AddOutcome::AlreadyExisted(existing)) => {
            info!(
                source = %source_instance.name,
                target = %target.name,
                tvdb_id = series_ref.tvdb_id,
                existing_id = existing.id,
                "cross-instance add_series absorbed 409 race — series already exists in target"
            );
            record_add_outcome(&source_instance.name, &target.name, OUTCOME_ALREADY_EXISTED);
            Ok(())
        }
        Err(err) => {
            record_add_outcome(&source_instance.name, &target.name, OUTCOME_ERROR);
            Err(HandlerError::Arr(err))
        }
    }
}

/// Add a series to every Sonarr instance whose configured language is
/// different from `source_instance`. Copies season monitoring from the
/// source instance so all monitored seasons are searched on the target.
pub async fn propagate_add_series<P: FfprobeProber>(
    registry: &HandlerRegistry<P>,
    source_instance: &InstanceConfig,
    series_ref: &SonarrSeriesRef,
    detected_languages: &std::collections::HashSet<String>,
) -> Result<(), HandlerError> {
    // See `propagate_add_movie`: never ask a sibling to grab a language the
    // imported file already carries.
    let targets: Vec<&InstanceConfig> = registry
        .config_instances()
        .iter()
        .filter(|i| {
            i.kind == InstanceKind::Sonarr
                && i.name != source_instance.name
                && i.language != source_instance.language
                && !detected_languages.contains(&i.language)
        })
        .collect();

    if targets.is_empty() {
        return Ok(());
    }

    let source_client = registry.client(&source_instance.name)?;
    let ArrClient::Sonarr(source_sonarr) = source_client else {
        return Ok(());
    };
    let (seasons, original) =
        fetch_source_series(source_sonarr, &source_instance.name, series_ref.tvdb_id).await?;
    if is_own_language_original(registry, source_instance, original.as_ref()) {
        skip_own_language_original(source_instance, &targets, &series_ref.title);
        return Ok(());
    }

    for target in targets {
        let target_client = registry.client(&target.name)?;
        let ArrClient::Sonarr(target_sonarr) = target_client else {
            continue;
        };
        add_series_to_target(source_instance, target, target_sonarr, series_ref, &seasons).await?;
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
