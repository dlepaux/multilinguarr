//! Admin scenarios — regeneration endpoint.

use serde_json::Value;

use crate::common::harness::{harness, Harness};
use crate::common::{assertions, post_webhook, wait_for_job};

/// Seed a movie into Radarr-EN, place fixture in EN storage, fire a
/// Download webhook so multilinguarr creates the initial links, then
/// return the movie's library folder name for later assertions.
async fn seed_and_import(
    h: &Harness,
    search_term: &str,
    fixture: &std::path::Path,
    filename: &str,
) -> (u32, String) {
    let lookup = h
        .radarr_en_client
        .movie_lookup(search_term)
        .await
        .expect("lookup");
    let tmdb_id = crate::common::as_u32(
        lookup
            .get("tmdbId")
            .and_then(Value::as_u64)
            .expect("tmdbId"),
        "tmdbId",
    );

    // Cleanup stale.
    if let Ok(Some(existing)) = h.radarr_en_client.find_movie_by_tmdb(tmdb_id).await {
        if let Some(id) = existing.get("id").and_then(Value::as_u64) {
            let _ = h
                .radarr_en_client
                .delete_movie(crate::common::as_u32(id, "id"), true)
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    let movie = h
        .radarr_en_client
        .add_movie_from_lookup(&lookup, &h.sandbox.storage.radarr_en)
        .await
        .expect("add movie");
    let movie_id =
        crate::common::as_u32(movie.get("id").and_then(Value::as_u64).expect("id"), "id");
    let movie_folder = movie
        .get("path")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .expect("path");
    let folder_name = movie_folder
        .file_name()
        .and_then(|n| n.to_str())
        .expect("folder")
        .to_owned();

    tokio::fs::create_dir_all(&movie_folder)
        .await
        .expect("mkdir");
    let dst = movie_folder.join(filename);
    tokio::fs::copy(fixture, &dst).await.expect("copy");

    let payload = serde_json::json!({
        "eventType": "Download",
        "isUpgrade": false,
        "movie": {
            "id": movie_id,
            "title": search_term,
            "year": 2000,
            "tmdbId": tmdb_id,
            "folderPath": movie_folder.display().to_string()
        },
        "movieFile": {
            "id": 1,
            "relativePath": filename,
            "path": dst.display().to_string()
        }
    });

    let resp = post_webhook(h, "radarr-en", payload)
        .await
        .expect("webhook");
    let job_id = resp.get("job_id").and_then(Value::as_i64).expect("job_id");
    wait_for_job(h, job_id).await.expect("job completed");

    (tmdb_id, folder_name)
}

// =====================================================================
// 01 — admin regenerate rebuilds wiped symlinks
// =====================================================================

pub async fn admin_01_regenerate_rebuilds_wiped_symlinks() {
    let h = harness().await;

    // Seed and import two movies via Radarr-EN:
    // 1. Multi-audio → both libraries
    // 2. English-only → EN library only
    let (tmdb_multi, folder_multi) =
        seed_and_import(&h, "Big Buck Bunny", &h.fixtures.multi_audio, "multi.mkv").await;
    let (tmdb_en, folder_en) =
        seed_and_import(&h, "Sintel", &h.fixtures.english_only, "en-only.mkv").await;

    // Verify initial state.
    assertions::assert_present(&h.sandbox.library.movies_en.join(&folder_multi))
        .await
        .expect("initial multi en present");
    assertions::assert_present(&h.sandbox.library.movies_en.join(&folder_en))
        .await
        .expect("initial en-only en present");

    // Wipe all movie library symlinks.
    let _ = tokio::fs::remove_dir_all(&h.sandbox.library.movies_en).await;
    let _ = tokio::fs::remove_dir_all(&h.sandbox.library.movies_fr).await;
    tokio::fs::create_dir_all(&h.sandbox.library.movies_en)
        .await
        .unwrap();
    tokio::fs::create_dir_all(&h.sandbox.library.movies_fr)
        .await
        .unwrap();

    // Assert wipe.
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&folder_multi))
        .await
        .expect("wiped multi en");
    assertions::assert_absent(&h.sandbox.library.movies_en.join(&folder_en))
        .await
        .expect("wiped en-only en");

    // POST /api/v1/admin/regenerate
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{}/api/v1/admin/regenerate", h.server.base_url))
        .header("X-Api-Key", "e2e-root")
        .send()
        .await
        .expect("regenerate request");
    assert_eq!(resp.status(), 200, "regenerate should return 200");
    let body: Value = resp.json().await.expect("regenerate json");
    assert!(!body["dry_run"].as_bool().unwrap_or(true));

    // Multi-audio movie: EN library rebuilt.
    assertions::assert_present(&h.sandbox.library.movies_en.join(&folder_multi))
        .await
        .expect("regenerated multi en present");

    // English-only movie: EN library rebuilt.
    assertions::assert_present(&h.sandbox.library.movies_en.join(&folder_en))
        .await
        .expect("regenerated en-only en present");

    // Cleanup.
    if let Ok(Some(m)) = h.radarr_en_client.find_movie_by_tmdb(tmdb_multi).await {
        if let Some(id) = m.get("id").and_then(Value::as_u64) {
            let _ = h
                .radarr_en_client
                .delete_movie(crate::common::as_u32(id, "id"), true)
                .await;
        }
    }
    if let Ok(Some(m)) = h.radarr_en_client.find_movie_by_tmdb(tmdb_en).await {
        if let Some(id) = m.get("id").and_then(Value::as_u64) {
            let _ = h
                .radarr_en_client
                .delete_movie(crate::common::as_u32(id, "id"), true)
                .await;
        }
    }
}
