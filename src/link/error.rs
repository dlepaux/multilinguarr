//! Link manager errors.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LinkError {
    #[error("path `{0:?}` not found")]
    NotFound(PathBuf),

    #[error("permission denied on `{0:?}`")]
    PermissionDenied(PathBuf),

    #[error(
        "target `{target:?}` already exists and does not match source `{from:?}` — \
         refusing to overwrite"
    )]
    AlreadyExists { from: PathBuf, target: PathBuf },

    #[error("cross-filesystem hardlink: `{from:?}` and `{target:?}` live on different devices")]
    CrossFilesystem { from: PathBuf, target: PathBuf },

    #[error("expected `{path:?}` to be a directory for movie-level linking — found a file")]
    ExpectedDirectory { path: PathBuf },

    #[error("expected `{path:?}` to be a file for episode-level linking — found a directory")]
    ExpectedFile { path: PathBuf },

    #[error("relative path `{0:?}` must not be absolute or contain `..` components")]
    InvalidRelativePath(PathBuf),

    #[error("io error on `{path:?}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl LinkError {
    pub(super) fn from_io(path: PathBuf, source: std::io::Error) -> Self {
        match source.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(path),
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(path),
            _ => Self::Io { path, source },
        }
    }
}
