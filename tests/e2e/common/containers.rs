//! Spin up 4 arr containers (radarr-en, radarr-fr, sonarr-en,
//! sonarr-fr), wait for each to expose its HTTP port, extract the
//! generated API key, and return a usable `ArrInstance` for each.
//!
//! Design choices:
//!
//! - **linuxserver images** are used to match the production setup.
//! - **Same-path bind mount**: every container gets
//!   `<sandbox.root>:<sandbox.root>` so absolute paths in API
//!   responses line up with host-side resolution (see `sandbox.rs`).
//! - **API key extraction** via `docker exec cat /config/config.xml`
//!   because the linuxserver entrypoint generates the key on first
//!   boot and exposes it nowhere else.
//! - **Readiness probe** hits `GET /api/v3/system/status` — an
//!   unauthenticated request returns 401 once the server is up, any
//!   other response means we keep waiting.

use std::time::{Duration, Instant};

use testcontainers::core::{ExecCommand, IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use super::sandbox::Sandbox;
use super::Result;

const RADARR_IMAGE: &str = "linuxserver/radarr";
const SONARR_IMAGE: &str = "linuxserver/sonarr";
const IMAGE_TAG: &str = "latest";

const RADARR_PORT: u16 = 7878;
const SONARR_PORT: u16 = 8989;

const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct ArrInstance {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

/// Everything required to tear down AND use the four arr containers.
/// The `ContainerAsync` handles are kept alive so they are stopped
/// when `ArrContainers` is dropped.
pub struct ArrContainers {
    pub radarr_en: ArrInstance,
    pub radarr_fr: ArrInstance,
    pub sonarr_en: ArrInstance,
    pub sonarr_fr: ArrInstance,
    _handles: Vec<ContainerAsync<GenericImage>>,
}

impl std::fmt::Debug for ArrContainers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `_handles` is deliberately omitted — testcontainers handles
        // do not implement `Debug` and carry no useful info here.
        f.debug_struct("ArrContainers")
            .field("radarr_en", &self.radarr_en)
            .field("radarr_fr", &self.radarr_fr)
            .field("sonarr_en", &self.sonarr_en)
            .field("sonarr_fr", &self.sonarr_fr)
            .finish_non_exhaustive()
    }
}

pub async fn spawn_arr_containers(sandbox: &Sandbox) -> Result<ArrContainers> {
    // Start all four in parallel — each startup takes ~30s for
    // linuxserver images on first boot.
    let (r_en, r_fr, s_en, s_fr) = tokio::try_join!(
        start_arr(
            "multilinguarr-e2e-radarr-en",
            RADARR_IMAGE,
            RADARR_PORT,
            sandbox
        ),
        start_arr(
            "multilinguarr-e2e-radarr-fr",
            RADARR_IMAGE,
            RADARR_PORT,
            sandbox
        ),
        start_arr(
            "multilinguarr-e2e-sonarr-en",
            SONARR_IMAGE,
            SONARR_PORT,
            sandbox
        ),
        start_arr(
            "multilinguarr-e2e-sonarr-fr",
            SONARR_IMAGE,
            SONARR_PORT,
            sandbox
        ),
    )?;

    Ok(ArrContainers {
        radarr_en: r_en.0,
        radarr_fr: r_fr.0,
        sonarr_en: s_en.0,
        sonarr_fr: s_fr.0,
        _handles: vec![r_en.1, r_fr.1, s_en.1, s_fr.1],
    })
}

async fn start_arr(
    label: &str,
    image: &str,
    internal_port: u16,
    sandbox: &Sandbox,
) -> Result<(ArrInstance, ContainerAsync<GenericImage>)> {
    // Same-path bind mount: host's `sandbox.root` appears at the
    // exact same absolute path inside the container.
    let sandbox_root = sandbox.root.display().to_string();
    let mount = Mount::bind_mount(sandbox_root.clone(), sandbox_root.clone());

    let image = GenericImage::new(image, IMAGE_TAG)
        .with_exposed_port(internal_port.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_mount(mount)
        .with_env_var("PUID", puid())
        .with_env_var("PGID", pgid())
        .with_env_var("TZ", "Etc/UTC");

    let container = image.start().await?;
    let host_port = container.get_host_port_ipv4(internal_port.tcp()).await?;
    let base_url = format!("http://127.0.0.1:{host_port}");

    // Wait for /api/v3/system/status to respond (401 or 200 both OK).
    wait_until_ready(&base_url).await?;

    // Extract API key from /config/config.xml inside the container.
    let api_key = extract_api_key(&container).await?;

    Ok((
        ArrInstance {
            name: label.to_owned(),
            base_url,
            api_key,
        },
        container,
    ))
}

async fn wait_until_ready(base_url: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let url = format!("{base_url}/api/v3/system/status");
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            return Err(format!("{base_url}: arr did not become ready in time").into());
        }
        match http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 401 => {
                return Ok(());
            }
            _ => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

async fn extract_api_key(container: &ContainerAsync<GenericImage>) -> Result<String> {
    // Poll for config.xml to exist + contain <ApiKey>. The
    // linuxserver entrypoint writes it asynchronously on first boot.
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            return Err("api key did not appear in config.xml in time".into());
        }
        let mut exec = container
            .exec(ExecCommand::new([
                "sh",
                "-c",
                "cat /config/config.xml 2>/dev/null || true",
            ]))
            .await?;
        let stdout = exec.stdout_to_vec().await?;
        let xml = String::from_utf8_lossy(&stdout);
        if let Some(key) = parse_api_key(&xml) {
            if !key.is_empty() {
                return Ok(key);
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn parse_api_key(xml: &str) -> Option<String> {
    let open = xml.find("<ApiKey>")?;
    let rest = &xml[open + "<ApiKey>".len()..];
    let close = rest.find("</ApiKey>")?;
    Some(rest[..close].trim().to_owned())
}

/// PUID passed to linuxserver images. We use 1000 (the default for
/// the linuxserver entrypoint) because the sandbox tree is chmod
/// 0o777 regardless — the container can always write.
fn puid() -> String {
    "1000".to_owned()
}

fn pgid() -> String {
    "1000".to_owned()
}
