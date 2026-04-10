//! Filesystem assertions for scenarios.
//!
//! Scenarios call `assert_present` / `assert_absent` on specific
//! paths. Anything heavier (recursive snapshots, golden-file diffs)
//! is out of scope for the current scenario set and can be added
//! when the first test needs it.

use std::path::Path;

use tokio::fs;

use super::Result;

/// Assert a file or symlink exists at the given absolute path and
/// resolves to (or IS) readable data.
pub async fn assert_present(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).await.map_err(|e| {
        format!(
            "expected path {} to exist, but stat failed: {e}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        // Resolve through to ensure the link is not dangling.
        let target = fs::read_link(path).await?;
        if fs::try_exists(&target).await.unwrap_or(false) {
            Ok(())
        } else {
            Err(format!(
                "symlink {} → {} is dangling",
                path.display(),
                target.display()
            )
            .into())
        }
    } else {
        Ok(())
    }
}

pub async fn assert_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => Err(format!(
            "expected path {} to be absent, but it exists",
            path.display()
        )
        .into()),
    }
}
