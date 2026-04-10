//! Detection errors.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetectionError {
    #[error("ffprobe binary not available on this host")]
    FfprobeUnavailable,

    #[error("ffprobe spawn failed for `{path}`: {source}")]
    FfprobeSpawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("ffprobe timed out after {timeout_ms}ms probing `{path}`")]
    FfprobeTimeout { path: PathBuf, timeout_ms: u64 },

    #[error("ffprobe exited with status {status} probing `{path}`: {stderr}")]
    FfprobeExit {
        path: PathBuf,
        status: i32,
        stderr: String,
    },

    #[error("failed to parse ffprobe JSON output for `{path}`: {source}")]
    FfprobeParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}
