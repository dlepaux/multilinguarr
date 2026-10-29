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
/// `tags.language` (ISO 639-2, three letters) drives language matching.
/// `channels` and `is_commentary` back the audio-quality gate: a release
/// passes only if a language-appropriate, non-commentary stream has a
/// `<= 6`-channel (5.1-or-lower) layout.
#[derive(Debug, Clone)]
pub struct AudioStream {
    pub language: Option<String>,
    pub channels: Option<u8>,
    /// True when disposition marks this a commentary / visual-impaired
    /// track, or its title names it as one. Such tracks are not the
    /// feature's base audio.
    pub is_commentary: bool,
}

/// An HEVC `hvcC` record shorter than this carries no parameter sets:
/// the fixed header alone is 23 bytes, and one VPS + SPS + PPS adds
/// well over 50. Such a file plays in mpv, which reads the parameter
/// sets in-band, but not in players that need them up front (Jellyfin
/// direct play on Apple TV).
const HEVC_MIN_HEADER_BYTES: u32 = 50;

/// What one ffprobe run tells the detector about a file.
#[derive(Debug, Clone, Default)]
pub struct ProbeReport {
    pub audio_streams: Vec<AudioStream>,
    /// `extradata_size` of the first HEVC video stream, `None` when the
    /// file has no HEVC video or ffprobe did not report the size.
    pub hevc_header_bytes: Option<u32>,
}

impl ProbeReport {
    /// True when the file's HEVC video lacks its parameter sets — see
    /// [`HEVC_MIN_HEADER_BYTES`].
    #[must_use]
    pub fn has_empty_hevc_header(&self) -> bool {
        self.hevc_header_bytes
            .is_some_and(|b| b < HEVC_MIN_HEADER_BYTES)
    }
}

impl From<Vec<AudioStream>> for ProbeReport {
    fn from(audio_streams: Vec<AudioStream>) -> Self {
        Self {
            audio_streams,
            hevc_header_bytes: None,
        }
    }
}

/// Abstraction over "run ffprobe and report on the file". The real
/// implementation shells out; tests use a stub.
pub trait FfprobeProber: std::fmt::Debug + Send + Sync + 'static {
    fn probe(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<ProbeReport, DetectionError>> + Send;
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
    async fn probe(&self, path: &Path, timeout: Duration) -> Result<ProbeReport, DetectionError> {
        let mut cmd = Command::new(&self.path);
        cmd.arg("-v")
            .arg("error")
            .arg("-print_format")
            .arg("json")
            .arg("-show_streams")
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
/// `ffprobe -print_format json -show_streams` emits every stream:
/// ```json
/// { "streams": [ { "codec_type": "audio", "tags": { "language": "eng" } } ] }
/// ```
/// Audio streams drive language detection; the video stream is read
/// only for its HEVC header size.
///
/// # Errors
///
/// Returns `serde_json::Error` if the input is not valid ffprobe JSON.
pub fn parse_streams_json(json: &str) -> Result<ProbeReport, serde_json::Error> {
    let envelope: FfprobeEnvelope = serde_json::from_str(json)?;
    let hevc_header_bytes = envelope
        .streams
        .iter()
        .find(|s| {
            s.codec_type.as_deref() == Some("video") && s.codec_name.as_deref() == Some("hevc")
        })
        .and_then(|s| s.extradata_size);
    let audio_streams = envelope
        .streams
        .into_iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .map(audio_stream)
        .collect();
    Ok(ProbeReport {
        audio_streams,
        hevc_header_bytes,
    })
}

fn audio_stream(s: FfprobeStream) -> AudioStream {
    let is_commentary = s
        .disposition
        .as_ref()
        .is_some_and(|d| d.comment == Some(1) || d.visual_impaired == Some(1))
        || s.tags
            .as_ref()
            .and_then(|t| t.title.as_deref())
            .is_some_and(title_is_commentary);
    AudioStream {
        language: s.tags.and_then(|t| t.language),
        channels: s.channels,
        is_commentary,
    }
}

/// Heuristic: does a stream's title name it a commentary / descriptive
/// track? A small substring check on purpose — not worth a `regex` dep.
fn title_is_commentary(title: &str) -> bool {
    let t = title.to_lowercase();
    t.contains("comment")
        || t.contains("narrat")
        || t.contains("descri")
        || t.contains("visual impair")
        || t.contains("visually impair")
}

#[derive(Debug, Deserialize)]
struct FfprobeEnvelope {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    extradata_size: Option<u32>,
    #[serde(default)]
    channels: Option<u8>,
    #[serde(default)]
    disposition: Option<FfprobeDisposition>,
    #[serde(default)]
    tags: Option<FfprobeTags>,
}

/// Subset of ffprobe's `disposition` object — the flags that mark a track
/// as non-feature audio. Unknown disposition fields are ignored by serde.
#[derive(Debug, Deserialize)]
struct FfprobeDisposition {
    #[serde(default)]
    comment: Option<u8>,
    #[serde(default)]
    visual_impaired: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct FfprobeTags {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    title: Option<String>,
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
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
