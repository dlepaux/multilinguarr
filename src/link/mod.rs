//! Filesystem link manager.
//!
//! Maps content between an instance's storage tree and its library
//! tree using either symlinks or hardlinks. Movies are linked at the
//! directory level (symlink) or as a mirrored tree of hardlinked files
//! (hardlink). TV episodes are always linked at the file level — the
//! season directory is created on demand.
//!
//! The cross-filesystem safety check for hardlink instances runs at
//! startup in [`crate::config::validation`]; by the time a
//! `LinkManager` is constructed, that invariant is guaranteed.

mod error;

#[cfg(test)]
mod tests;

use std::path::{Component, Path, PathBuf};

use tokio::fs;

pub use error::LinkError;

use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};

/// Outcome of a link operation. `Created` means the library now has a
/// link it did not have before; `AlreadyPresent` means an equivalent
/// link already existed and the call was a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAction {
    Created,
    AlreadyPresent,
}

/// Per-instance link manager.
#[derive(Debug, Clone)]
pub struct LinkManager {
    instance_name: String,
    instance_kind: InstanceKind,
    storage_root: PathBuf,
    library_root: PathBuf,
    strategy: LinkStrategy,
}

impl LinkManager {
    /// Build a `LinkManager` for a configured instance.
    #[must_use]
    pub fn from_instance(instance: &InstanceConfig) -> Self {
        Self {
            instance_name: instance.name.clone(),
            instance_kind: instance.kind,
            storage_root: instance.storage_path.clone(),
            library_root: instance.library_path.clone(),
            strategy: instance.link_strategy,
        }
    }

    #[must_use]
    pub fn instance_name(&self) -> &str {
        &self.instance_name
    }

    #[must_use]
    pub fn strategy(&self) -> LinkStrategy {
        self.strategy
    }

    #[must_use]
    pub fn kind(&self) -> InstanceKind {
        self.instance_kind
    }

    // ---------------------------------------------------------------
    // Movie-level (Radarr): directory symlink OR mirrored hardlinks.
    // ---------------------------------------------------------------

    /// Link a movie folder from `storage_root/folder_name` into
    /// `library_root/folder_name`.
    ///
    /// * `symlink` strategy: single directory symlink.
    /// * `hardlink` strategy: mirror the directory tree and hardlink
    ///   every file individually.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError`] on I/O failure, missing source directory,
    /// path traversal, or if the target already exists with a different source.
    pub async fn link_movie(&self, folder_name: &str) -> Result<LinkAction, LinkError> {
        let source = self.storage_root.join(folder_name);
        self.link_movie_from(&source, folder_name).await
    }

    /// Link a movie folder from an explicit `source` (which may live
    /// under a different instance's storage tree) into this instance's
    /// `library_root/folder_name`.
    ///
    /// Used by the cross-instance multi-audio import path: when
    /// radarr-fr downloads a file with both fr and en audio, the en
    /// alternate's library needs a link whose source is fr's storage,
    /// not en's.
    ///
    /// **Cross-filesystem caveat for hardlinks**: the startup
    /// validation in `config::validation` only checks each instance's
    /// own `storage_path → library_path` pairing. A hardlink instance
    /// linked from a foreign storage on a different device will fail
    /// at runtime with `LinkError::CrossFilesystem`. Story 08b is
    /// scoped to extend the startup check to cover cross-instance
    /// pairs implied by the language layout.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `folder_name` is absolute or contains `..`.
    /// - [`LinkError::NotFound`] / [`LinkError::ExpectedDirectory`] if source is missing or not a directory.
    /// - [`LinkError::AlreadyExists`] if the target exists but does not match the source.
    /// - [`LinkError::CrossFilesystem`] on hardlink across devices.
    /// - [`LinkError::Io`] on other filesystem errors.
    pub async fn link_movie_from(
        &self,
        source: &Path,
        folder_name: &str,
    ) -> Result<LinkAction, LinkError> {
        reject_unsafe_relative(Path::new(folder_name))?;
        ensure_source_is_dir(source).await?;

        let target = self.library_root.join(folder_name);
        match self.strategy {
            LinkStrategy::Symlink => create_symlink_idempotent(source, &target).await,
            LinkStrategy::Hardlink => mirror_dir_with_hardlinks(source, &target).await,
        }
    }

    /// Remove a previously linked movie folder from the library.
    /// Delegates to [`Self::unlink_folder`] — kept for call-site
    /// clarity at movie handlers.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError`] on path traversal or I/O failure during removal.
    pub async fn unlink_movie(&self, folder_name: &str) -> Result<(), LinkError> {
        self.unlink_folder(folder_name).await
    }

    /// Recursively remove a top-level library folder and prune empty
    /// parents up to (but not past) the library root. Works for both
    /// movie folders (Radarr) and series folders (Sonarr series
    /// delete) because the on-disk shape is the same: either a
    /// directory symlink or a mirrored subtree.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `folder_name` is absolute or contains `..`.
    /// - [`LinkError::Io`] / [`LinkError::PermissionDenied`] on filesystem failure.
    pub async fn unlink_folder(&self, folder_name: &str) -> Result<(), LinkError> {
        reject_unsafe_relative(Path::new(folder_name))?;

        let target = self.library_root.join(folder_name);
        match fs::symlink_metadata(&target).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                fs::remove_file(&target)
                    .await
                    .map_err(|e| LinkError::from_io(target.clone(), e))?;
            }
            Ok(meta) if meta.is_dir() => {
                fs::remove_dir_all(&target)
                    .await
                    .map_err(|e| LinkError::from_io(target.clone(), e))?;
            }
            Ok(_) => {
                fs::remove_file(&target)
                    .await
                    .map_err(|e| LinkError::from_io(target.clone(), e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(LinkError::from_io(target, e)),
        }

        prune_empty_parents(&target, &self.library_root).await
    }

    /// **Storage-aware delete probe.** For a hardlink instance, return
    /// `true` when the file at `library_path/relative` is the *last*
    /// hardlink to its inode (`nlink == 1`) — meaning removing it
    /// would lose the underlying data. For a symlink instance, this
    /// check is meaningless (a symlink never owns data) and always
    /// returns `Ok(false)`. Also returns `Ok(false)` if the path does
    /// not exist (nothing to lose).
    ///
    /// Used by the 08b delete handlers to refuse to remove a library
    /// link that would orphan the source file.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `relative` is absolute or contains `..`.
    /// - [`LinkError::Io`] on metadata read failure (other than not-found).
    pub async fn is_last_hardlink(&self, relative: &Path) -> Result<bool, LinkError> {
        reject_unsafe_relative(relative)?;
        if !matches!(self.strategy, LinkStrategy::Hardlink) {
            return Ok(false);
        }
        let target = self.library_root.join(relative);
        match fs::metadata(&target).await {
            Ok(meta) => Ok(nlink_of(&meta) <= 1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(LinkError::from_io(target, e)),
        }
    }

    /// **Storage-aware delete probe.** For a symlink instance, return
    /// `true` when the entry at `library_path/relative`:
    ///
    /// - is a symlink whose target lives under `foreign_storage_root`, **or**
    /// - is a directory containing at least one symlink (recursively)
    ///   whose target lives under `foreign_storage_root`.
    ///
    /// Used to detect "this library entry is actually served by
    /// another instance's storage — do not remove it just because the
    /// local arr forgot about the file." The directory case covers
    /// per-episode symlink trees created by Sonarr handlers.
    ///
    /// For hardlink instances the concept does not apply (a hardlink
    /// has no "target"), and the method returns `Ok(false)`.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `relative` is absolute or contains `..`.
    /// - [`LinkError::Io`] on symlink metadata or readlink failure.
    pub async fn resolves_into(
        &self,
        relative: &Path,
        foreign_storage_root: &Path,
    ) -> Result<bool, LinkError> {
        reject_unsafe_relative(relative)?;
        if !matches!(self.strategy, LinkStrategy::Symlink) {
            return Ok(false);
        }
        let target = self.library_root.join(relative);
        match fs::symlink_metadata(&target).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link_target = fs::read_link(&target)
                    .await
                    .map_err(|e| LinkError::from_io(target.clone(), e))?;
                Ok(link_target.starts_with(foreign_storage_root))
            }
            Ok(meta) if meta.is_dir() => {
                first_symlink_resolves_into(&target, foreign_storage_root).await
            }
            Ok(_) => Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(LinkError::from_io(target, e)),
        }
    }

    // ---------------------------------------------------------------
    // Episode-level (Sonarr): per-file link.
    // ---------------------------------------------------------------

    /// Link a single episode file `storage_root/relative` into
    /// `library_root/relative`, creating season directories as needed.
    /// Works identically for symlink and hardlink strategies.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError`] on I/O failure, missing source file,
    /// path traversal, or if the target already exists with a different source.
    pub async fn link_episode(&self, relative: &Path) -> Result<LinkAction, LinkError> {
        let source = self.storage_root.join(relative);
        self.link_episode_from(&source, relative).await
    }

    /// Link an episode file from an explicit `source` (potentially in
    /// another instance's storage) into this instance's
    /// `library_root/relative`. See [`Self::link_movie_from`] for the
    /// cross-instance rationale and the cross-filesystem caveat.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `relative` is absolute or contains `..`.
    /// - [`LinkError::NotFound`] / [`LinkError::ExpectedFile`] if source is missing or not a file.
    /// - [`LinkError::AlreadyExists`] if the target exists but does not match the source.
    /// - [`LinkError::CrossFilesystem`] on hardlink across devices.
    /// - [`LinkError::Io`] on other filesystem errors.
    pub async fn link_episode_from(
        &self,
        source: &Path,
        relative: &Path,
    ) -> Result<LinkAction, LinkError> {
        reject_unsafe_relative(relative)?;
        ensure_source_is_file(source).await?;

        let target = self.library_root.join(relative);
        ensure_parent_dir(&target).await?;

        match self.strategy {
            LinkStrategy::Symlink => create_symlink_idempotent(source, &target).await,
            LinkStrategy::Hardlink => create_hardlink_idempotent(source, &target).await,
        }
    }

    /// Remove a linked episode file. Prunes empty parent directories
    /// up to (but not past) the library root.
    ///
    /// # Errors
    ///
    /// - [`LinkError::InvalidRelativePath`] if `relative` is absolute or contains `..`.
    /// - [`LinkError::Io`] / [`LinkError::PermissionDenied`] on filesystem failure.
    pub async fn unlink_episode(&self, relative: &Path) -> Result<(), LinkError> {
        reject_unsafe_relative(relative)?;

        let target = self.library_root.join(relative);
        match fs::symlink_metadata(&target).await {
            Ok(_) => {
                fs::remove_file(&target)
                    .await
                    .map_err(|e| LinkError::from_io(target.clone(), e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(LinkError::from_io(target, e)),
        }

        prune_empty_parents(&target, &self.library_root).await
    }
}

// ---------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------

/// Reject absolute paths and `..` traversal — the link manager only
/// accepts relative, well-behaved descendants.
fn reject_unsafe_relative(path: &Path) -> Result<(), LinkError> {
    if path.is_absolute() {
        return Err(LinkError::InvalidRelativePath(path.to_path_buf()));
    }
    for comp in path.components() {
        if matches!(comp, Component::ParentDir | Component::RootDir) {
            return Err(LinkError::InvalidRelativePath(path.to_path_buf()));
        }
    }
    Ok(())
}

async fn ensure_source_is_dir(path: &Path) -> Result<(), LinkError> {
    let meta = fs::metadata(path)
        .await
        .map_err(|e| LinkError::from_io(path.to_path_buf(), e))?;
    if !meta.is_dir() {
        return Err(LinkError::ExpectedDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

async fn ensure_source_is_file(path: &Path) -> Result<(), LinkError> {
    let meta = fs::metadata(path)
        .await
        .map_err(|e| LinkError::from_io(path.to_path_buf(), e))?;
    if !meta.is_file() {
        return Err(LinkError::ExpectedFile {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

async fn ensure_parent_dir(path: &Path) -> Result<(), LinkError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| LinkError::from_io(parent.to_path_buf(), e))?;
    }
    Ok(())
}

/// Create `target` as a symlink pointing at `source`, idempotently.
///
/// * Missing target → create. `Created`.
/// * Existing symlink resolving to `source` → `AlreadyPresent`.
/// * Existing symlink pointing elsewhere OR existing non-symlink → error.
async fn create_symlink_idempotent(source: &Path, target: &Path) -> Result<LinkAction, LinkError> {
    ensure_parent_dir(target).await?;

    match fs::symlink_metadata(target).await {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                let existing = fs::read_link(target)
                    .await
                    .map_err(|e| LinkError::from_io(target.to_path_buf(), e))?;
                if existing == source {
                    return Ok(LinkAction::AlreadyPresent);
                }
            }
            return Err(LinkError::AlreadyExists {
                from: source.to_path_buf(),
                target: target.to_path_buf(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(LinkError::from_io(target.to_path_buf(), e)),
    }

    #[cfg(unix)]
    {
        tokio::fs::symlink(source, target)
            .await
            .map_err(|e| LinkError::from_io(target.to_path_buf(), e))?;
    }
    #[cfg(not(unix))]
    {
        return Err(LinkError::from_io(
            target.to_path_buf(),
            std::io::Error::other("symlinks unsupported on non-unix target"),
        ));
    }
    Ok(LinkAction::Created)
}

/// Create `target` as a hardlink to `source`, idempotently.
async fn create_hardlink_idempotent(source: &Path, target: &Path) -> Result<LinkAction, LinkError> {
    ensure_parent_dir(target).await?;

    if fs::try_exists(target)
        .await
        .map_err(|e| LinkError::from_io(target.to_path_buf(), e))?
    {
        if same_inode(source, target).await? {
            return Ok(LinkAction::AlreadyPresent);
        }
        return Err(LinkError::AlreadyExists {
            from: source.to_path_buf(),
            target: target.to_path_buf(),
        });
    }

    fs::hard_link(source, target).await.map_err(|e| {
        if e.raw_os_error() == Some(libc_exdev()) {
            LinkError::CrossFilesystem {
                from: source.to_path_buf(),
                target: target.to_path_buf(),
            }
        } else {
            LinkError::from_io(target.to_path_buf(), e)
        }
    })?;
    Ok(LinkAction::Created)
}

#[cfg(unix)]
fn libc_exdev() -> i32 {
    // EXDEV = 18 on Linux, 18 on macOS. Rather than linking libc, just
    // inline the constant — it's been 18 since AT&T System V.
    18
}

#[cfg(not(unix))]
fn libc_exdev() -> i32 {
    0
}

#[cfg(unix)]
async fn same_inode(a: &Path, b: &Path) -> Result<bool, LinkError> {
    use std::os::unix::fs::MetadataExt;
    let a_meta = fs::metadata(a)
        .await
        .map_err(|e| LinkError::from_io(a.to_path_buf(), e))?;
    let b_meta = fs::metadata(b)
        .await
        .map_err(|e| LinkError::from_io(b.to_path_buf(), e))?;
    Ok(a_meta.dev() == b_meta.dev() && a_meta.ino() == b_meta.ino())
}

#[cfg(not(unix))]
async fn same_inode(_a: &Path, _b: &Path) -> Result<bool, LinkError> {
    Ok(false)
}

/// Walk `dir` recursively and return `true` if the first symlink
/// encountered resolves to a path under `foreign_root`. Used by
/// [`LinkManager::resolves_into`] for the directory case.
async fn first_symlink_resolves_into(dir: &Path, foreign_root: &Path) -> Result<bool, LinkError> {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = fs::read_dir(&current)
            .await
            .map_err(|e| LinkError::from_io(current.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| LinkError::from_io(current.clone(), e))?
        {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)
                .await
                .map_err(|e| LinkError::from_io(path.clone(), e))?;
            if meta.file_type().is_symlink() {
                let link_target = fs::read_link(&path)
                    .await
                    .map_err(|e| LinkError::from_io(path.clone(), e))?;
                return Ok(link_target.starts_with(foreign_root));
            } else if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn nlink_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(not(unix))]
fn nlink_of(_meta: &std::fs::Metadata) -> u64 {
    // Non-unix targets are not supported deployment targets; treating
    // every file as "not a hardlink" disables the storage-aware probe.
    2
}

/// Mirror a source directory tree into `target`, creating directories
/// and hardlinking every regular file. Idempotent at the file level:
/// if a file already exists as a hardlink to the source, skip it.
async fn mirror_dir_with_hardlinks(source: &Path, target: &Path) -> Result<LinkAction, LinkError> {
    fs::create_dir_all(target)
        .await
        .map_err(|e| LinkError::from_io(target.to_path_buf(), e))?;

    let mut any_created = false;
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];

    while let Some(relative) = stack.pop() {
        let src_dir = source.join(&relative);
        let dst_dir = target.join(&relative);
        fs::create_dir_all(&dst_dir)
            .await
            .map_err(|e| LinkError::from_io(dst_dir.clone(), e))?;

        let mut read = fs::read_dir(&src_dir)
            .await
            .map_err(|e| LinkError::from_io(src_dir.clone(), e))?;
        while let Some(entry) = read
            .next_entry()
            .await
            .map_err(|e| LinkError::from_io(src_dir.clone(), e))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| LinkError::from_io(entry.path(), e))?;
            let child_rel = relative.join(entry.file_name());
            if file_type.is_dir() {
                stack.push(child_rel);
            } else if file_type.is_file() {
                let src_file = source.join(&child_rel);
                let dst_file = target.join(&child_rel);
                match create_hardlink_idempotent(&src_file, &dst_file).await? {
                    LinkAction::Created => any_created = true,
                    LinkAction::AlreadyPresent => {}
                }
            }
            // Skip symlinks inside storage trees — arr clients should
            // never be writing those under storage/.
        }
    }

    Ok(if any_created {
        LinkAction::Created
    } else {
        LinkAction::AlreadyPresent
    })
}

/// Walk upward from `start.parent()` removing empty directories, but
/// stop as soon as we reach `boundary` — the library root must never
/// be deleted, even if it is empty.
async fn prune_empty_parents(start: &Path, boundary: &Path) -> Result<(), LinkError> {
    let Some(mut cursor) = start.parent().map(Path::to_path_buf) else {
        return Ok(());
    };

    while cursor.starts_with(boundary) && cursor != *boundary {
        match fs::remove_dir(&cursor).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // `remove_dir` returns an io::Error with kind=Other when
            // the directory is non-empty on most platforms — stop
            // walking up and return success.
            Err(_) => return Ok(()),
        }
        let Some(parent) = cursor.parent().map(Path::to_path_buf) else {
            break;
        };
        cursor = parent;
    }
    Ok(())
}
