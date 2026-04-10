//! Per-session harness: a single `OnceCell<Arc<Harness>>` so the
//! expensive container startup happens once per test-binary run.
//!
//! Each scenario calls `harness().await` and gets a shared
//! reference. Per-test isolation is handled by using unique
//! `tmdb_id` / `tvdb_id` / folder names in each scenario — there is
//! no reset between scenarios.

use std::sync::{Arc, LazyLock};

use tokio::sync::OnceCell;

use super::arr::ArrHarnessClient;
use super::containers::{spawn_arr_containers, ArrContainers};
use super::fixtures::Fixtures;
use super::sandbox::Sandbox;
use super::server::{spawn_server, ServerHandle};
use super::Result;

#[derive(Debug)]
pub struct Harness {
    pub sandbox: Sandbox,
    /// RAII guard — dropping this stops every arr container. Not
    /// read directly by scenarios (they go through the per-instance
    /// clients below), but must outlive them.
    #[allow(dead_code)]
    pub arr: ArrContainers,
    pub server: ServerHandle,
    pub fixtures: Fixtures,
    pub radarr_en_client: ArrHarnessClient,
    pub radarr_fr_client: ArrHarnessClient,
    pub sonarr_en_client: ArrHarnessClient,
    pub sonarr_fr_client: ArrHarnessClient,
}

static HARNESS: LazyLock<OnceCell<Arc<Harness>>> = LazyLock::new(OnceCell::new);

/// Return the shared harness, building it on the first call.
///
/// On failure the error bubbles up as a panic with a descriptive
/// message — scenarios call `.expect(...)` which gives us the stack
/// trace for the actual setup failure.
pub async fn harness() -> Arc<Harness> {
    HARNESS.get_or_init(build_harness).await.clone()
}

async fn build_harness() -> Arc<Harness> {
    match try_build_harness().await {
        Ok(h) => Arc::new(h),
        Err(e) => panic!("e2e harness setup failed: {e}"),
    }
}

async fn try_build_harness() -> Result<Harness> {
    let fixtures = Fixtures::locate().await?;
    let sandbox = Sandbox::new().await?;
    let arr = spawn_arr_containers(&sandbox).await?;

    // Seed all four arr instances: add the root folder, relax size
    // limits, relax free-space check.
    let radarr_en_client = ArrHarnessClient::new(&arr.radarr_en)?;
    let radarr_fr_client = ArrHarnessClient::new(&arr.radarr_fr)?;
    let sonarr_en_client = ArrHarnessClient::new(&arr.sonarr_en)?;
    let sonarr_fr_client = ArrHarnessClient::new(&arr.sonarr_fr)?;

    radarr_en_client
        .add_root_folder(&sandbox.storage.radarr_en)
        .await?;
    radarr_fr_client
        .add_root_folder(&sandbox.storage.radarr_fr)
        .await?;
    sonarr_en_client
        .add_root_folder(&sandbox.storage.sonarr_en)
        .await?;
    sonarr_fr_client
        .add_root_folder(&sandbox.storage.sonarr_fr)
        .await?;

    // Size limits must be relaxed on both radarr and sonarr so the
    // 3-4 MB test fixtures do not get rejected as sample files.
    for client in [
        &radarr_en_client,
        &radarr_fr_client,
        &sonarr_en_client,
        &sonarr_fr_client,
    ] {
        client.relax_quality_definitions().await?;
        client.relax_media_management().await?;
    }

    let server = spawn_server(&sandbox, &arr).await?;

    Ok(Harness {
        sandbox,
        arr,
        server,
        fixtures,
        radarr_en_client,
        radarr_fr_client,
        sonarr_en_client,
        sonarr_fr_client,
    })
}
