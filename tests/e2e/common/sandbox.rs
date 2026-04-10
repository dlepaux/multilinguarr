//! Per-session sandbox directory tree that every arr container AND
//! the in-process multilinguarr server both see under the same
//! absolute path.
//!
//! **Why same-path bind mounts**: the arr containers send webhook
//! payloads carrying absolute paths (e.g. the movie's `folder_path`),
//! and multilinguarr must resolve those same paths on the host to
//! create symlinks. If the container path and the host path differ,
//! every webhook-derived path needs rewriting, and the arr APIs that
//! return `path` fields for existing records would require the same
//! treatment. Mounting the host tempdir at its own absolute path
//! inside each container keeps things frictionless.
//!
//! Structure:
//! ```text
//! <root>/media/
//!   storage/{radarr-en,radarr-fr,sonarr-en,sonarr-fr}/
//!   library/movies/{en,fr}/
//!   library/tv/{en,fr}/
//! ```

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::fs;

use super::Result;

#[derive(Debug)]
pub struct Sandbox {
    _tmp: TempDir,
    pub root: PathBuf,
    pub media: PathBuf,
    pub storage: InstancePaths,
    pub library: LibraryPaths,
}

#[derive(Debug, Clone)]
pub struct InstancePaths {
    pub radarr_en: PathBuf,
    pub radarr_fr: PathBuf,
    pub sonarr_en: PathBuf,
    pub sonarr_fr: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LibraryPaths {
    pub movies_en: PathBuf,
    pub movies_fr: PathBuf,
    pub tv_en: PathBuf,
    pub tv_fr: PathBuf,
}

impl Sandbox {
    pub async fn new() -> Result<Self> {
        let tmp = TempDir::with_prefix("multilinguarr-e2e-")?;
        let root = tmp.path().to_path_buf();
        let media = root.join("media");

        let storage = InstancePaths {
            radarr_en: media.join("storage/radarr-en"),
            radarr_fr: media.join("storage/radarr-fr"),
            sonarr_en: media.join("storage/sonarr-en"),
            sonarr_fr: media.join("storage/sonarr-fr"),
        };
        let library = LibraryPaths {
            movies_en: media.join("library/movies/en"),
            movies_fr: media.join("library/movies/fr"),
            tv_en: media.join("library/tv/en"),
            tv_fr: media.join("library/tv/fr"),
        };

        for dir in [
            &storage.radarr_en,
            &storage.radarr_fr,
            &storage.sonarr_en,
            &storage.sonarr_fr,
            &library.movies_en,
            &library.movies_fr,
            &library.tv_en,
            &library.tv_fr,
        ] {
            fs::create_dir_all(dir).await?;
        }

        // Docker Desktop bind mounts on macOS can mask out the host
        // user's permissions; make everything world-writable so the
        // arr containers (running as PUID=1000 by default in the
        // linuxserver images) can write to the same tree that the
        // host-side multilinguarr reads.
        chmod_recursive_0777(&media)?;

        Ok(Self {
            _tmp: tmp,
            root,
            media,
            storage,
            library,
        })
    }
}

#[cfg(unix)]
fn chmod_recursive_0777(root: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let perms = std::fs::Permissions::from_mode(0o777);
        std::fs::set_permissions(&current, perms)?;
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn chmod_recursive_0777(_root: &Path) -> std::io::Result<()> {
    Ok(())
}
