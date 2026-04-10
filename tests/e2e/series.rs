//! Series scenarios — ports of the TypeScript E2E suite's
//! `tests/e2e/scenarios/series/` coverage. Same primary-vs-alternate
//! × multi-vs-single matrix as movies.rs, but at episode-file
//! granularity.
//!
//! All scenarios share a single real TVDB series ("Breaking Bad",
//! TVDB 81189) discovered via `series_lookup`. Tests clean up at
//! start AND end so the shared session stays re-entrant.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::common::arr::ArrHarnessClient;
use crate::common::harness::{harness, Harness};
use crate::common::{as_u32, assertions, post_webhook, wait_for_job};

const SEARCH_TERM: &str = "Breaking Bad";
const RELATIVE_EPISODE: &str = "Season 01/Episode.S01E01.mkv";

// ---- shared setup helpers ----

async fn seed_series_episode(
    client: &ArrHarnessClient,
    storage_root: &Path,
    fixture: &Path,
) -> SeriesHandle {
    let lookup = client
        .series_lookup(SEARCH_TERM)
        .await
        .expect("series lookup");
    let tvdb_id = as_u32(
        lookup
            .get("tvdbId")
            .and_then(Value::as_u64)
            .expect("lookup tvdbId"),
        "tvdbId",
    );
    let title = lookup
        .get("title")
        .and_then(Value::as_str)
        .expect("lookup title")
        .to_owned();

    if let Ok(Some(existing)) = client.find_series_by_tvdb(tvdb_id).await {
        if let Some(id) = existing.get("id").and_then(Value::as_u64) {
            let _ = client.delete_series(as_u32(id, "series.id"), true).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    let series = client
        .add_series_from_lookup(&lookup, storage_root)
        .await
        .expect("add series");
    let series_id = as_u32(
        series.get("id").and_then(Value::as_u64).expect("series id"),
        "series.id",
    );
    let series_dir = series
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .expect("series.path in add response");
    let folder_name = series_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("series folder")
        .to_owned();

    // Place file directly in storage. The handler reads the file
    // path from the webhook payload and runs ffprobe — no arr
    // rescan needed.
    let full_path = series_dir.join(RELATIVE_EPISODE);
    tokio::fs::create_dir_all(full_path.parent().unwrap())
        .await
        .expect("mkdir season");
    tokio::fs::copy(fixture, &full_path)
        .await
        .expect("copy fixture");

    // Synthetic id — handler uses file path from webhook, not this id.
    let episode_file_id = 1;

    SeriesHandle {
        series_id,
        tvdb_id,
        title,
        folder_name,
        series_dir,
        episode_file_id,
        relative_episode: RELATIVE_EPISODE.to_owned(),
        full_path,
    }
}

#[derive(Debug, Clone)]
struct SeriesHandle {
    series_id: u32,
    tvdb_id: u32,
    title: String,
    folder_name: String,
    series_dir: PathBuf,
    episode_file_id: u32,
    /// Path relative to `series_dir` — `Season 01/<file>.mkv`.
    relative_episode: String,
    /// Absolute path on disk — `series_dir / relative_episode`.
    full_path: PathBuf,
}

fn episode_download_payload(h: &SeriesHandle, is_upgrade: bool) -> Value {
    json!({
        "eventType": "Download",
        "isUpgrade": is_upgrade,
        "series": {
            "id": h.series_id,
            "title": h.title,
            "tvdbId": h.tvdb_id,
            "path": h.series_dir.display().to_string()
        },
        "episodes": [{
            "id": 1,
            "episodeNumber": 1,
            "seasonNumber": 1
        }],
        "episodeFile": {
            "id": h.episode_file_id,
            "relativePath": h.relative_episode,
            "path": h.full_path.display().to_string()
        }
    })
}

fn series_delete_payload(h: &SeriesHandle, deleted_files: bool) -> Value {
    json!({
        "eventType": "SeriesDelete",
        "deletedFiles": deleted_files,
        "series": {
            "id": h.series_id,
            "title": h.title,
            "tvdbId": h.tvdb_id,
            "path": h.series_dir.display().to_string()
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

async fn cleanup_sonarr(client: &ArrHarnessClient, tvdb_id: u32) {
    if let Ok(Some(existing)) = client.find_series_by_tvdb(tvdb_id).await {
        if let Some(id) = existing.get("id").and_then(Value::as_u64) {
            let _ = client.delete_series(as_u32(id, "series.id"), true).await;
        }
    }
}

async fn cleanup_all(h: &Harness, tvdb_id: u32) {
    cleanup_sonarr(&h.sonarr_fr_client, tvdb_id).await;
    cleanup_sonarr(&h.sonarr_en_client, tvdb_id).await;
}

async fn cleanup_libraries(h: &Harness, folder_name: &str) {
    let _ = tokio::fs::remove_dir_all(h.sandbox.library.tv_fr.join(folder_name)).await;
    let _ = tokio::fs::remove_dir_all(h.sandbox.library.tv_en.join(folder_name)).await;
}

async fn lookup_tvdb_id(h: &Harness) -> u32 {
    let lookup = h
        .sonarr_fr_client
        .series_lookup(SEARCH_TERM)
        .await
        .expect("series lookup");
    as_u32(
        lookup
            .get("tvdbId")
            .and_then(Value::as_u64)
            .expect("lookup tvdbId"),
        "tvdbId",
    )
}

// =====================================================================
// 01 — multi-audio primary episode import
// =====================================================================

pub async fn series_01_multi_audio_primary_links_both_libraries() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_multi,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    let rel_in_lib: PathBuf = [&handle.folder_name, RELATIVE_EPISODE].iter().collect();
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&rel_in_lib))
        .await
        .expect("tv fr present");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&rel_in_lib))
        .await
        .expect("tv en present");

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 02 — single-EN episode on alternate
// =====================================================================

pub async fn series_02_single_en_alternate_links_alt_only() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_en_client,
        &h.sandbox.storage.sonarr_en,
        &h.fixtures.episode_en,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "sonarr-en", episode_download_payload(&handle, false)).await;

    let rel_in_lib: PathBuf = [&handle.folder_name, RELATIVE_EPISODE].iter().collect();
    assertions::assert_present(&h.sandbox.library.tv_en.join(&rel_in_lib))
        .await
        .expect("tv en present");
    assertions::assert_absent(&h.sandbox.library.tv_fr.join(&handle.folder_name))
        .await
        .expect("tv fr absent");

    // Alternate imports must not propagate to primary.
    assert!(
        h.sonarr_fr_client
            .find_series_by_tvdb(handle.tvdb_id)
            .await
            .expect("primary lookup")
            .is_none(),
        "alternate must not propagate to primary"
    );

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 03 — single-EN episode on primary FR (cross-instance add)
// =====================================================================

pub async fn series_03_single_en_on_primary_fr_propagates_add() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_en,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    // Cross-instance propagation: Sonarr-EN now has the series.
    assert!(
        h.sonarr_en_client
            .find_series_by_tvdb(handle.tvdb_id)
            .await
            .expect("alt lookup")
            .is_some(),
        "Sonarr-EN should receive the cross-instance add"
    );

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 04 — upgrade episode (single→multi)
// =====================================================================

pub async fn series_04_upgrade_to_multi_audio_relinks_both_libraries() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_en,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    // Step 1 — single-EN primary import.
    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    // Step 2 — upgrade to multi-audio: replace file on disk. Handler
    // reads file path from webhook and runs ffprobe — no arr rescan.
    let _ = tokio::fs::remove_file(&handle.full_path).await;
    let upgraded_relative = "Season 01/Breaking.Bad.S01E01.MULTi.TRUEFRENCH.ENGLISH.1080p.mkv";
    let upgraded_full = handle.series_dir.join(upgraded_relative);
    tokio::fs::create_dir_all(upgraded_full.parent().unwrap())
        .await
        .expect("mkdir season");
    tokio::fs::copy(&h.fixtures.episode_multi, &upgraded_full)
        .await
        .expect("copy multi-audio episode");
    let mut upgraded = handle.clone();
    upgraded_relative.clone_into(&mut upgraded.relative_episode);
    upgraded.full_path = upgraded_full;
    ship_webhook(&h, "sonarr-fr", episode_download_payload(&upgraded, true)).await;
    let handle = upgraded;

    let rel_in_lib: PathBuf = [&handle.folder_name, handle.relative_episode.as_str()]
        .iter()
        .collect();
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&rel_in_lib))
        .await
        .expect("post-upgrade tv fr present");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&rel_in_lib))
        .await
        .expect("post-upgrade tv en present");

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 05 — delete from primary clears both libraries
// =====================================================================

pub async fn series_05_delete_from_primary_clears_both_libraries() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_multi,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;
    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    let rel_in_lib: PathBuf = [&handle.folder_name, RELATIVE_EPISODE].iter().collect();
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&rel_in_lib))
        .await
        .expect("pre-delete tv fr present");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&rel_in_lib))
        .await
        .expect("pre-delete tv en present");

    ship_webhook(&h, "sonarr-fr", series_delete_payload(&handle, true)).await;

    assertions::assert_absent(&h.sandbox.library.tv_fr.join(&handle.folder_name))
        .await
        .expect("post-delete tv fr absent");
    assertions::assert_absent(&h.sandbox.library.tv_en.join(&handle.folder_name))
        .await
        .expect("post-delete tv en absent");

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 06 — full journey: import → delete
// =====================================================================

pub async fn series_06_full_journey_import_then_delete() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_multi,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    let rel_in_lib: PathBuf = [&handle.folder_name, RELATIVE_EPISODE].iter().collect();
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&rel_in_lib))
        .await
        .expect("journey import tv fr present");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&rel_in_lib))
        .await
        .expect("journey import tv en present");

    ship_webhook(&h, "sonarr-fr", series_delete_payload(&handle, true)).await;

    assertions::assert_absent(&h.sandbox.library.tv_fr.join(&handle.folder_name))
        .await
        .expect("journey delete tv fr absent");
    assertions::assert_absent(&h.sandbox.library.tv_en.join(&handle.folder_name))
        .await
        .expect("journey delete tv en absent");

    cleanup_all(&h, handle.tvdb_id).await;
}

// =====================================================================
// 07 — mixed-audio series: per-episode language variation
// =====================================================================

#[allow(clippy::too_many_lines)]
pub async fn series_07_mixed_audio_per_episode() {
    let h = harness().await;
    cleanup_all(&h, lookup_tvdb_id(&h).await).await;

    // Seed the series in Sonarr-FR.
    let handle = seed_series_episode(
        &h.sonarr_fr_client,
        &h.sandbox.storage.sonarr_fr,
        &h.fixtures.episode_multi,
    )
    .await;
    cleanup_libraries(&h, &handle.folder_name).await;

    // --- Episode 1: multi-audio (EN+FR) from Sonarr-FR ---
    ship_webhook(&h, "sonarr-fr", episode_download_payload(&handle, false)).await;

    let ep1_lib = PathBuf::from(&handle.folder_name).join(RELATIVE_EPISODE);
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&ep1_lib))
        .await
        .expect("ep1 tv fr present");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&ep1_lib))
        .await
        .expect("ep1 tv en present");

    // --- Episode 2: French-only from Sonarr-FR ---
    let ep2_relative = "Season 01/Episode.S01E02.mkv";
    let ep2_full = handle.series_dir.join(ep2_relative);
    tokio::fs::create_dir_all(ep2_full.parent().unwrap())
        .await
        .expect("mkdir ep2");
    tokio::fs::copy(&h.fixtures.episode_fr, &ep2_full)
        .await
        .expect("copy ep2 fixture");

    let ep2_payload = serde_json::json!({
        "eventType": "Download",
        "isUpgrade": false,
        "series": {
            "id": handle.series_id,
            "title": handle.title,
            "tvdbId": handle.tvdb_id,
            "path": handle.series_dir.display().to_string()
        },
        "episodes": [{
            "id": 2,
            "episodeNumber": 2,
            "seasonNumber": 1
        }],
        "episodeFile": {
            "id": 2,
            "relativePath": ep2_relative,
            "path": ep2_full.display().to_string()
        }
    });
    ship_webhook(&h, "sonarr-fr", ep2_payload).await;

    let ep2_lib = PathBuf::from(&handle.folder_name).join(ep2_relative);
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&ep2_lib))
        .await
        .expect("ep2 tv fr present");
    assertions::assert_absent(&h.sandbox.library.tv_en.join(&ep2_lib))
        .await
        .expect("ep2 tv en absent");

    // Cross-instance: Sonarr-EN now has the series.
    assert!(
        h.sonarr_en_client
            .find_series_by_tvdb(handle.tvdb_id)
            .await
            .expect("en lookup")
            .is_some(),
        "Sonarr-EN should receive cross-instance add from ep2"
    );

    // --- Episode 3: English-only from Sonarr-EN ---
    let ep3_relative = "Season 01/Episode.S01E03.mkv";
    let en_series = h
        .sonarr_en_client
        .find_series_by_tvdb(handle.tvdb_id)
        .await
        .expect("find en series")
        .expect("en series exists");
    let en_series_dir = en_series
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .expect("en series path");
    let en_series_id = crate::common::as_u32(
        en_series
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .expect("id"),
        "id",
    );
    let en_folder_name = en_series_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("en folder")
        .to_owned();

    let ep3_full = en_series_dir.join(ep3_relative);
    tokio::fs::create_dir_all(ep3_full.parent().unwrap())
        .await
        .expect("mkdir ep3");
    tokio::fs::copy(&h.fixtures.english_only, &ep3_full)
        .await
        .expect("copy ep3 fixture");

    let ep3_payload = serde_json::json!({
        "eventType": "Download",
        "isUpgrade": false,
        "series": {
            "id": en_series_id,
            "title": handle.title,
            "tvdbId": handle.tvdb_id,
            "path": en_series_dir.display().to_string()
        },
        "episodes": [{
            "id": 3,
            "episodeNumber": 3,
            "seasonNumber": 1
        }],
        "episodeFile": {
            "id": 3,
            "relativePath": ep3_relative,
            "path": ep3_full.display().to_string()
        }
    });
    ship_webhook(&h, "sonarr-en", ep3_payload).await;

    let ep3_lib = PathBuf::from(&en_folder_name).join(ep3_relative);
    assertions::assert_present(&h.sandbox.library.tv_en.join(&ep3_lib))
        .await
        .expect("ep3 tv en present");
    assertions::assert_absent(&h.sandbox.library.tv_fr.join(&ep3_lib))
        .await
        .expect("ep3 tv fr absent");

    // Final re-check: all three episodes in expected state.
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&ep1_lib))
        .await
        .expect("final ep1 fr");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&ep1_lib))
        .await
        .expect("final ep1 en");
    assertions::assert_present(&h.sandbox.library.tv_fr.join(&ep2_lib))
        .await
        .expect("final ep2 fr");
    assertions::assert_absent(&h.sandbox.library.tv_en.join(&ep2_lib))
        .await
        .expect("final ep2 en absent");
    assertions::assert_present(&h.sandbox.library.tv_en.join(&ep3_lib))
        .await
        .expect("final ep3 en");
    assertions::assert_absent(&h.sandbox.library.tv_fr.join(&ep3_lib))
        .await
        .expect("final ep3 fr absent");

    cleanup_all(&h, handle.tvdb_id).await;
}
