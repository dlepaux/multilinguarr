//! End-to-end integration tests — story 10.
//!
//! Gated behind `--features e2e` so `cargo test` default skips them
//! (no Docker daemon required in CI unit runs). Run with:
//!
//! ```bash
//! cargo test --features e2e --test e2e
//! ```
//!
//! The harness spins up 4 linuxserver arr containers, seeds them
//! with a shared sandbox directory, and boots an in-process
//! multilinguarr server on an ephemeral port. All scenarios share
//! that single session via a `OnceCell<Arc<Harness>>`.

#![cfg(feature = "e2e")]

// Integration-test crate roots resolve submodules relative to the
// `tests/` directory, not a stem-named subdir. Use `#[path]` to
// keep harness code grouped under `tests/e2e/`.
#[path = "e2e/common/mod.rs"]
mod common;

#[path = "e2e/admin.rs"]
mod admin;

#[path = "e2e/movies.rs"]
mod movies;

#[path = "e2e/series.rs"]
mod series;

/// Single `#[tokio::test]` entrypoint — every scenario runs on the
/// same runtime so the shared harness (containers + in-process
/// multilinguarr server + background tasks) stays alive across tests.
/// Using per-scenario `#[tokio::test]` tears down the runtime between
/// tests, killing the harness's spawned tasks while `OnceCell` still
/// holds stale references.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_all_scenarios() {
    // Movies
    movies::movies_01_multi_audio_primary_links_into_both_libraries().await;
    movies::movies_02_single_fr_primary_links_primary_only_and_propagates_add().await;
    movies::movies_03_single_en_alternate_links_alt_only().await;
    movies::movies_04_upgrade_to_multi_audio_relinks_both_libraries().await;
    movies::movies_05_delete_from_primary_clears_both_libraries().await;
    movies::movies_06_full_journey_import_upgrade_delete().await;
    movies::movies_07_full_single_language_journey().await;

    // Series
    series::series_01_multi_audio_primary_links_both_libraries().await;
    series::series_02_single_en_alternate_links_alt_only().await;
    series::series_03_single_en_on_primary_fr_propagates_add().await;
    series::series_04_upgrade_to_multi_audio_relinks_both_libraries().await;
    series::series_05_delete_from_primary_clears_both_libraries().await;
    series::series_06_full_journey_import_then_delete().await;
    series::series_07_mixed_audio_per_episode().await;

    // Admin
    admin::admin_01_regenerate_rebuilds_wiped_symlinks().await;
}
