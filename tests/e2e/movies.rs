//! Movie scenarios — ports of the TypeScript E2E suite's
//! `tests/e2e/scenarios/movies/` coverage.
//!
//! All scenarios share a single real TMDB movie ("Big Buck Bunny",
//! TMDB 10378) discovered via `movie_lookup`. Tests clean up at
//! start AND end so the shared session stays re-entrant.
//!
//! The Radarr-assigned folder (read from `movie.path` on the add
//! response) is used both for fixture placement and webhook payload
//! construction — Radarr generates its folder name from TMDB
//! metadata, not the harness.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::common::arr::ArrHarnessClient;
use crate::common::harness::{harness, Harness};
use crate::common::{as_u32, assertions, post_webhook, wait_for_job};

const SEARCH_TERM: &str = "Big Buck Bunny";

// ---- shared setup helpers ----

async fn seed_movie(
    client: &ArrHarnessClient,
    storage_root: &Path,
    fixture: &Path,
    filename: &str,
) -> MovieHandle {
    // Discover a real TMDB entry via lookup (so Radarr's validator
    // accepts the add payload).
    let lookup = client
        .movie_lookup(SEARCH_TERM)
        .await
        .expect("movie lookup");
    let tmdb_id = as_u32(
        lookup
            .get("tmdbId")
            .and_then(Value::as_u64)
            .expect("lookup tmdbId"),
        "tmdbId",
    );
    let title = lookup
        .get("title")
        .and_then(Value::as_str)
        .expect("lookup title")
        .to_owned();
    let year = as_u32(
        lookup.get("year").and_then(Value::as_u64).unwrap_or(0),
        "year",
    );

    // Aggressive pre-cleanup: delete the movie if a prior test left
    // it behind. We wait briefly for the delete to settle.
    if let Ok(Some(existing)) = client.find_movie_by_tmdb(tmdb_id).await {
        if let Some(id) = existing.get("id").and_then(Value::as_u64) {
            let _ = client.delete_movie(as_u32(id, "movie.id"), true).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    let movie = client
        .add_movie_from_lookup(&lookup, storage_root)
        .await
        .expect("add movie");
    let movie_id = as_u32(
        movie.get("id").and_then(Value::as_u64).expect("movie id"),
        "movie.id",
    );

    // Use Radarr's auto-generated path, NOT ours.
    let movie_folder = movie
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .expect("movie.path in add response");
    let folder_name = movie_folder
        .file_name()
        .and_then(|n| n.to_str())
        .expect("folder name")
        .to_owned();

    // Place file directly in storage. The handler reads the file
    // path from the webhook payload and runs ffprobe — no arr
    // rescan needed.
    tokio::fs::create_dir_all(&movie_folder)
        .await
        .expect("mkdir");
    let dst = movie_folder.join(filename);
    tokio::fs::copy(fixture, &dst).await.expect("copy fixture");

    MovieHandle {
        movie_id,
        tmdb_id,
        title,
        year,
        folder_name,
        movie_folder,
        file_path: dst,
    }
}

#[derive(Debug, Clone)]
struct MovieHandle {
    movie_id: u32,
    tmdb_id: u32,
    title: String,
    year: u32,
    folder_name: String,
    movie_folder: PathBuf,
    file_path: PathBuf,
}

fn download_payload(h: &MovieHandle, filename: &str, is_upgrade: bool) -> Value {
    json!({
        "eventType": "Download",
        "isUpgrade": is_upgrade,
        "movie": {
            "id": h.movie_id,
            "title": h.title,
            "year": h.year,
            "tmdbId": h.tmdb_id,
            "folderPath": h.movie_folder.display().to_string()
        },
        "movieFile": {
            "id": 1,
            "relativePath": filename,
            "path": h.file_path.display().to_string()
        }
    })
}

fn movie_delete_payload(h: &MovieHandle, deleted_files: bool) -> Value {
    json!({
        "eventType": "MovieDelete",
        "deletedFiles": deleted_files,
        "movie": {
            "id": h.movie_id,
            "title": h.title,
            "year": h.year,
            "tmdbId": h.tmdb_id,
            "folderPath": h.movie_folder.display().to_string()
        }
    })
}

async fn ship_webhook(h: &Harness, instance: &str, payload: Value) {
    let resp = post_webhook(h, instance, payload).await.expect("webhook");
    let job_id = resp
        .get("job_id")
        .and_then(Value::as_i64)
        .expect("job_id in webhook response");
    wait_for_job(h, job_id).await.expect("job completed");
}

async fn cleanup_radarr(client: &ArrHarnessClient, tmdb_id: u32) {
    if let Ok(Some(existing)) = client.find_movie_by_tmdb(tmdb_id).await {
        if let Some(id) = existing.get("id").and_then(Value::as_u64) {
            let _ = client.delete_movie(as_u32(id, "movie.id"), true).await;
        }
    }
}

/// Clean up every arr instance that might have a copy of the
/// shared test movie. Called at the start of every scenario so
/// the session is re-entrant.
async fn cleanup_all(h: &Harness, tmdb_id: u32) {
    cleanup_radarr(&h.radarr_fr_client, tmdb_id).await;
    cleanup_radarr(&h.radarr_en_client, tmdb_id).await;
}

/// Remove any library links from a prior run.
async fn cleanup_libraries(h: &Harness, folder_name: &str) {
    let _ = tokio::fs::remove_dir_all(h.sandbox.library.movies_fr.join(folder_name)).await;
    let _ = tokio::fs::remove_dir_all(h.sandbox.library.movies_en.join(folder_name)).await;
}

/// Shared pre-cleanup helper — look up the TMDB id for the shared
/// test movie via Radarr's own lookup endpoint. Used at the top of
/// every scenario to make the session re-entrant.
async fn lookup_tmdb_id(h: &Harness) -> u32 {
    let lookup = h
        .radarr_fr_client
        .movie_lookup(SEARCH_TERM)
        .await
        .expect("movie lookup");
    as_u32(
        lookup
            .get("tmdbId")
            .and_then(Value::as_u64)
            .expect("lookup tmdbId"),
        "tmdbId",
    )
}

// =====================================================================
// 01 — multi-audio primary import
// =====================================================================

pub async fn movies_01_multi_audio_primary_links_into_both_libraries() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.multi.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.multi_audio,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "radarr-fr", download_payload(&handle, filename, false)).await;

    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("fr library present");
    assertions::assert_present(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("en library present");

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 02 — single-language FR on primary + cross-instance add
// =====================================================================

pub async fn movies_02_single_fr_primary_links_primary_only_and_propagates_add() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.fr.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.french_only,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "radarr-fr", download_payload(&handle, filename, false)).await;

    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("fr library present");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("en library absent");

    // Cross-instance propagation: Radarr-EN now has the movie.
    assert!(
        h.radarr_en_client
            .find_movie_by_tmdb(handle.tmdb_id)
            .await
            .expect("alt lookup")
            .is_some(),
        "Radarr-EN should receive the cross-instance add"
    );

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 03 — single-language EN on alternate
// =====================================================================

pub async fn movies_03_single_en_alternate_links_alt_only() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.en.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_en_client,
        &h.sandbox.storage.radarr_en,
        &h.fixtures.english_only,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "radarr-en", download_payload(&handle, filename, false)).await;

    assertions::assert_present(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("en library present");
    assertions::assert_absent(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("fr library absent");

    // Alternate imports do not propagate to primary.
    assert!(
        h.radarr_fr_client
            .find_movie_by_tmdb(handle.tmdb_id)
            .await
            .expect("primary lookup")
            .is_none(),
        "alternate import must not propagate to primary"
    );

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 04 — upgrade single-language → multi-audio
// =====================================================================

pub async fn movies_04_upgrade_to_multi_audio_relinks_both_libraries() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.upgrade.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.french_only,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "radarr-fr", download_payload(&handle, filename, false)).await;
    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("initial fr library present");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("initial en library absent");

    // Upgrade to multi-audio: replace file on disk. Handler reads
    // file path from webhook and runs ffprobe — no arr rescan needed.
    let _ = tokio::fs::remove_file(&handle.file_path).await;
    let upgraded_filename = "Big.Buck.Bunny.2008.MULTi.TRUEFRENCH.ENGLISH.1080p.mkv";
    let upgraded_path = handle.movie_folder.join(upgraded_filename);
    tokio::fs::copy(&h.fixtures.multi_audio, &upgraded_path)
        .await
        .expect("copy multi-audio fixture");

    let mut upgraded_handle = handle.clone();
    upgraded_handle.file_path = upgraded_path;
    ship_webhook(
        &h,
        "radarr-fr",
        download_payload(&upgraded_handle, upgraded_filename, true),
    )
    .await;
    let handle = upgraded_handle;

    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("post-upgrade fr library present");
    assertions::assert_present(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("post-upgrade en library present");

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 05 — delete from primary clears both libraries
// =====================================================================

pub async fn movies_05_delete_from_primary_clears_both_libraries() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.delete.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.multi_audio,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;
    ship_webhook(&h, "radarr-fr", download_payload(&handle, filename, false)).await;
    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("pre-delete fr library present");
    assertions::assert_present(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("pre-delete en library present");

    ship_webhook(&h, "radarr-fr", movie_delete_payload(&handle, true)).await;

    assertions::assert_absent(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("post-delete fr library absent");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("post-delete en library absent");

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 06 — full journey: import → upgrade → delete
// =====================================================================

pub async fn movies_06_full_journey_import_upgrade_delete() {
    let h = harness().await;
    let filename = "Big.Buck.Bunny.journey.mkv";

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    let handle = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.french_only,
        filename,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    // Step 1: single-FR import, verify fr lib + alt propagation.
    ship_webhook(&h, "radarr-fr", download_payload(&handle, filename, false)).await;
    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("journey step1 fr present");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("journey step1 en absent");
    assert!(
        h.radarr_en_client
            .find_movie_by_tmdb(handle.tmdb_id)
            .await
            .expect("alt lookup")
            .is_some(),
        "journey step1 alt propagated"
    );

    // Step 2: upgrade to multi-audio → en library now populated.
    let _ = tokio::fs::remove_file(&handle.file_path).await;
    let upgraded_filename = "Big.Buck.Bunny.2008.MULTi.TRUEFRENCH.ENGLISH.1080p.mkv";
    let upgraded_path = handle.movie_folder.join(upgraded_filename);
    tokio::fs::copy(&h.fixtures.multi_audio, &upgraded_path)
        .await
        .expect("copy multi-audio fixture");
    let mut handle = handle;
    handle.file_path = upgraded_path;
    ship_webhook(
        &h,
        "radarr-fr",
        download_payload(&handle, upgraded_filename, true),
    )
    .await;
    assertions::assert_present(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("journey step2 en present");

    // Step 3: delete → both libraries cleared.
    ship_webhook(&h, "radarr-fr", movie_delete_payload(&handle, true)).await;
    assertions::assert_absent(&h.sandbox.library.movies_fr.join(&handle.folder_name))
        .await
        .expect("journey step3 fr absent");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle.folder_name))
        .await
        .expect("journey step3 en absent");

    cleanup_all(&h, handle.tmdb_id).await;
}

// =====================================================================
// 07 — full single-language journey: FR downloads FR, EN downloads EN
// =====================================================================

pub async fn movies_07_full_single_language_journey() {
    let h = harness().await;

    cleanup_all(&h, lookup_tmdb_id(&h).await).await;

    // Phase 1: Radarr-FR downloads a French-only file.
    let filename_fr = "Big.Buck.Bunny.fr-only.mkv";
    let handle_fr = seed_movie(
        &h.radarr_fr_client,
        &h.sandbox.storage.radarr_fr,
        &h.fixtures.french_only,
        filename_fr,
    )
    .await;
    cleanup_libraries(&h, &handle_fr.folder_name).await;

    ship_webhook(
        &h,
        "radarr-fr",
        download_payload(&handle_fr, filename_fr, false),
    )
    .await;

    // FR library present, EN library absent.
    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle_fr.folder_name))
        .await
        .expect("phase1 fr library present");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&handle_fr.folder_name))
        .await
        .expect("phase1 en library absent");

    // Cross-instance: Radarr-EN now has the movie.
    assert!(
        h.radarr_en_client
            .find_movie_by_tmdb(handle_fr.tmdb_id)
            .await
            .expect("alt lookup")
            .is_some(),
        "Radarr-EN should receive cross-instance add"
    );

    // Phase 2: Radarr-EN downloads its own English-only file.
    let filename_en = "Big.Buck.Bunny.en-only.mkv";
    let en_movie = h
        .radarr_en_client
        .find_movie_by_tmdb(handle_fr.tmdb_id)
        .await
        .expect("find en")
        .expect("en movie exists");
    let en_movie_id = crate::common::as_u32(
        en_movie
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .expect("id"),
        "id",
    );
    let en_movie_folder = en_movie
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .expect("path");

    tokio::fs::create_dir_all(&en_movie_folder)
        .await
        .expect("mkdir en");
    let en_file_path = en_movie_folder.join(filename_en);
    tokio::fs::copy(&h.fixtures.english_only, &en_file_path)
        .await
        .expect("copy en fixture");

    let en_folder_name = en_movie_folder
        .file_name()
        .and_then(|n| n.to_str())
        .expect("en folder name")
        .to_owned();

    let en_payload = serde_json::json!({
        "eventType": "Download",
        "isUpgrade": false,
        "movie": {
            "id": en_movie_id,
            "title": handle_fr.title,
            "year": handle_fr.year,
            "tmdbId": handle_fr.tmdb_id,
            "folderPath": en_movie_folder.display().to_string()
        },
        "movieFile": {
            "id": 2,
            "relativePath": filename_en,
            "path": en_file_path.display().to_string()
        }
    });
    ship_webhook(&h, "radarr-en", en_payload).await;

    // EN library now present.
    assertions::assert_present(&h.sandbox.library.movies_en.join(&en_folder_name))
        .await
        .expect("phase2 en library present");
    // FR library still present.
    assertions::assert_present(&h.sandbox.library.movies_fr.join(&handle_fr.folder_name))
        .await
        .expect("phase2 fr library still present");

    // Verify symlink targets point to correct storage.
    let fr_link = h.sandbox.library.movies_fr.join(&handle_fr.folder_name);
    let fr_target = tokio::fs::read_link(&fr_link).await.expect("read fr link");
    assert!(
        fr_target.starts_with(&h.sandbox.storage.radarr_fr),
        "FR symlink should point to FR storage"
    );

    let en_link = h.sandbox.library.movies_en.join(&en_folder_name);
    let en_target = tokio::fs::read_link(&en_link).await.expect("read en link");
    assert!(
        en_target.starts_with(&h.sandbox.storage.radarr_en),
        "EN symlink should point to EN storage"
    );

    cleanup_all(&h, handle_fr.tmdb_id).await;
}
