//! `ffprobe` integration — subprocess wrapper and JSON parser.
//!
//! Split into three concerns so the pure parser can be unit-tested in
//! isolation, while the subprocess layer is exercised via a small
//! injectable trait and a stub in tests.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use super::error::DetectionError;

/// One audio stream as returned by `ffprobe`.
///
/// Only the `tags.language` field is load-bearing — everything else is
/// ignored. ffprobe emits ISO 639-2 codes (three letters) for audio
/// tracks, so that is what callers will usually match against.
#[derive(Debug, Clone)]
pub struct AudioStream {
    pub language: Option<String>,
}

/// Abstraction over "run ffprobe and give me the audio streams". The
/// real implementation shells out; tests use a stub.
pub trait FfprobeProber: std::fmt::Debug + Send + Sync + 'static {
    fn probe(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<Vec<AudioStream>, DetectionError>> + Send;
}

/// System `ffprobe` prober. Constructed via [`Self::locate`] which
/// searches `$PATH`. Returns `None` (not an error) if `ffprobe` is not
/// installed — the detector falls back to methods 1 + 2 and emits a
/// warning at startup. Swap in with a non-standard path via
/// [`Self::with_path`] when the binary lives outside `$PATH`.
#[derive(Debug, Clone)]
pub struct SystemFfprobe {
    path: PathBuf,
}

impl SystemFfprobe {
    /// Probe `$PATH` for an ffprobe binary. Returns `None` if not found.
    #[must_use]
    pub fn locate() -> Option<Self> {
        which("ffprobe").map(|path| Self { path })
    }

    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.path
    }
}

impl FfprobeProber for SystemFfprobe {
    async fn probe(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> Result<Vec<AudioStream>, DetectionError> {
        let mut cmd = Command::new(&self.path);
        cmd.arg("-v")
            .arg("error")
            .arg("-print_format")
            .arg("json")
            .arg("-show_streams")
            .arg("-select_streams")
            .arg("a")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(|source| DetectionError::FfprobeSpawn {
            path: path.to_path_buf(),
            source,
        })?;

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|source| DetectionError::FfprobeSpawn {
                path: path.to_path_buf(),
                source,
            })?,
            Err(_) => {
                return Err(DetectionError::FfprobeTimeout {
                    path: path.to_path_buf(),
                    timeout_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
                });
            }
        };

        if !output.status.success() {
            return Err(DetectionError::FfprobeExit {
                path: path.to_path_buf(),
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let json = std::str::from_utf8(&output.stdout).unwrap_or_default();
        parse_streams_json(json).map_err(|source| DetectionError::FfprobeParse {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Pure parser — this is what the unit tests pound on.
///
/// `ffprobe -print_format json -show_streams` emits:
/// ```json
/// { "streams": [ { "tags": { "language": "eng", ... }, ... } ] }
/// ```
///
/// # Errors
///
/// Returns `serde_json::Error` if the input is not valid ffprobe JSON.
pub fn parse_streams_json(json: &str) -> Result<Vec<AudioStream>, serde_json::Error> {
    let envelope: FfprobeEnvelope = serde_json::from_str(json)?;
    Ok(envelope
        .streams
        .into_iter()
        .map(|s| AudioStream {
            language: s.tags.and_then(|t| t.language),
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct FfprobeEnvelope {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    tags: Option<FfprobeTags>,
}

#[derive(Debug, Deserialize)]
struct FfprobeTags {
    #[serde(default)]
    language: Option<String>,
}

/// Minimal `which` replacement — a 15-line PATH scan, no `which` crate.
///
/// We deliberately do not pull in the `which` crate for this: the logic
/// is trivial and the dep would be paying kilobytes for `X_OK` checks
/// we already get from `std::fs::metadata`.
fn which(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
