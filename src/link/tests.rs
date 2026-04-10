//! Filesystem tests for [`LinkManager`].
//!
//! Every test runs against its own tempdir so they are safe to run in
//! parallel. Both the symlink and hardlink strategies are exercised.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio::fs;

use super::{LinkAction, LinkError, LinkManager};
use crate::config::{InstanceConfig, InstanceKind, LinkStrategy};

// ---------- fixtures ----------

struct Sandbox {
    _tmp: TempDir,
    storage: PathBuf,
    library: PathBuf,
}

impl Sandbox {
    async fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let storage = tmp.path().join("storage");
        let library = tmp.path().join("library");
        fs::create_dir_all(&storage).await.unwrap();
        fs::create_dir_all(&library).await.unwrap();
        Self {
            _tmp: tmp,
            storage,
            library,
        }
    }

    fn manager(&self, kind: InstanceKind, strategy: LinkStrategy) -> LinkManager {
        LinkManager::from_instance(&InstanceConfig {
            name: "test".to_owned(),
            kind,
            language: "fr".to_owned(),
            url: "http://localhost".to_owned(),
            api_key: "k".to_owned(),
            storage_path: self.storage.clone(),
            library_path: self.library.clone(),
            link_strategy: strategy,
            propagate_delete: true,
        })
    }
}

async fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.unwrap();
    }
    fs::write(path, contents).await.unwrap();
}

async fn read_file(path: &Path) -> String {
    fs::read_to_string(path).await.unwrap()
}

// ---------- movie symlink ----------

#[tokio::test]
async fn symlink_movie_creates_directory_symlink() {
    let sandbox = Sandbox::new().await;
    write_file(
        &sandbox.storage.join("The Matrix (1999)/The Matrix.mkv"),
        "bytes",
    )
    .await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    let action = manager.link_movie("The Matrix (1999)").await.unwrap();
    assert_eq!(action, LinkAction::Created);

    let target = sandbox.library.join("The Matrix (1999)");
    let meta = fs::symlink_metadata(&target).await.unwrap();
    assert!(meta.file_type().is_symlink());
    let resolved = fs::read_link(&target).await.unwrap();
    assert_eq!(resolved, sandbox.storage.join("The Matrix (1999)"));
    // Content reachable through the symlink.
    assert_eq!(read_file(&target.join("The Matrix.mkv")).await, "bytes");
}

#[tokio::test]
async fn symlink_movie_is_idempotent_on_repeat() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Inception/Inception.mkv"), "x").await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    assert_eq!(
        manager.link_movie("Inception").await.unwrap(),
        LinkAction::Created
    );
    assert_eq!(
        manager.link_movie("Inception").await.unwrap(),
        LinkAction::AlreadyPresent
    );
}

#[tokio::test]
async fn symlink_movie_unlink_removes_the_link_only() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Arrival/Arrival.mkv"), "x").await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    manager.link_movie("Arrival").await.unwrap();
    manager.unlink_movie("Arrival").await.unwrap();

    // Library target gone.
    assert!(!fs::try_exists(sandbox.library.join("Arrival"))
        .await
        .unwrap());
    // Storage untouched.
    assert!(fs::try_exists(sandbox.storage.join("Arrival/Arrival.mkv"))
        .await
        .unwrap());
}

#[tokio::test]
async fn unlink_movie_is_idempotent_when_missing() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    // No link exists; unlink must succeed silently.
    manager.unlink_movie("Ghost").await.unwrap();
}

// ---------- movie hardlink (mirrored tree) ----------

#[tokio::test]
async fn hardlink_movie_mirrors_tree_and_links_files() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Dune (2021)/Dune.mkv"), "video").await;
    write_file(
        &sandbox.storage.join("Dune (2021)/featurettes/behind.mkv"),
        "extra",
    )
    .await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Hardlink);
    let action = manager.link_movie("Dune (2021)").await.unwrap();
    assert_eq!(action, LinkAction::Created);

    // Target is a real directory, not a symlink.
    let meta = fs::symlink_metadata(sandbox.library.join("Dune (2021)"))
        .await
        .unwrap();
    assert!(meta.is_dir());
    assert!(!meta.file_type().is_symlink());

    // Files exist and hold the same content as the source.
    let linked = sandbox.library.join("Dune (2021)/Dune.mkv");
    assert_eq!(read_file(&linked).await, "video");
    let linked_extra = sandbox.library.join("Dune (2021)/featurettes/behind.mkv");
    assert_eq!(read_file(&linked_extra).await, "extra");

    // Same inode as source — true hardlink, not a copy.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let src_meta = std::fs::metadata(sandbox.storage.join("Dune (2021)/Dune.mkv")).unwrap();
        let tgt_meta = std::fs::metadata(&linked).unwrap();
        assert_eq!(src_meta.ino(), tgt_meta.ino());
    }
}

#[tokio::test]
async fn hardlink_movie_is_idempotent_on_repeat() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Tenet/Tenet.mkv"), "x").await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Hardlink);
    assert_eq!(
        manager.link_movie("Tenet").await.unwrap(),
        LinkAction::Created
    );
    assert_eq!(
        manager.link_movie("Tenet").await.unwrap(),
        LinkAction::AlreadyPresent
    );
}

#[tokio::test]
async fn hardlink_movie_unlink_removes_mirror_and_empty_parents() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Sicario/Sicario.mkv"), "x").await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Hardlink);
    manager.link_movie("Sicario").await.unwrap();
    manager.unlink_movie("Sicario").await.unwrap();

    assert!(!fs::try_exists(sandbox.library.join("Sicario"))
        .await
        .unwrap());
    // Source untouched.
    assert!(fs::try_exists(sandbox.storage.join("Sicario/Sicario.mkv"))
        .await
        .unwrap());
}

// ---------- episode symlink + hardlink ----------

#[tokio::test]
async fn symlink_episode_creates_file_symlink_and_parents() {
    let sandbox = Sandbox::new().await;
    write_file(
        &sandbox.storage.join("Breaking Bad/Season 01/S01E01.mkv"),
        "e1",
    )
    .await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    let relative = Path::new("Breaking Bad/Season 01/S01E01.mkv");
    let action = manager.link_episode(relative).await.unwrap();
    assert_eq!(action, LinkAction::Created);

    let target = sandbox.library.join(relative);
    assert!(fs::symlink_metadata(&target)
        .await
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(read_file(&target).await, "e1");
}

#[tokio::test]
async fn hardlink_episode_creates_hardlink_with_same_inode() {
    let sandbox = Sandbox::new().await;
    let rel = Path::new("The Wire/Season 02/S02E01.mkv");
    write_file(&sandbox.storage.join(rel), "pilot").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Hardlink);
    let action = manager.link_episode(rel).await.unwrap();
    assert_eq!(action, LinkAction::Created);

    let src = sandbox.storage.join(rel);
    let tgt = sandbox.library.join(rel);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let s = std::fs::metadata(&src).unwrap();
        let t = std::fs::metadata(&tgt).unwrap();
        assert_eq!(s.ino(), t.ino());
    }
    assert_eq!(read_file(&tgt).await, "pilot");
}

#[tokio::test]
async fn episode_unlink_prunes_empty_season_but_not_library_root() {
    let sandbox = Sandbox::new().await;
    let rel = Path::new("Show/Season 01/S01E01.mkv");
    write_file(&sandbox.storage.join(rel), "x").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    manager.link_episode(rel).await.unwrap();
    manager.unlink_episode(rel).await.unwrap();

    // Season and series dirs cleaned up.
    assert!(!fs::try_exists(sandbox.library.join("Show")).await.unwrap());
    // Library root still present.
    assert!(fs::try_exists(&sandbox.library).await.unwrap());
}

#[tokio::test]
async fn episode_unlink_preserves_non_empty_sibling_directories() {
    let sandbox = Sandbox::new().await;
    let a = Path::new("Show/Season 01/S01E01.mkv");
    let b = Path::new("Show/Season 01/S01E02.mkv");
    write_file(&sandbox.storage.join(a), "a").await;
    write_file(&sandbox.storage.join(b), "b").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    manager.link_episode(a).await.unwrap();
    manager.link_episode(b).await.unwrap();
    manager.unlink_episode(a).await.unwrap();

    assert!(!fs::try_exists(sandbox.library.join(a)).await.unwrap());
    assert!(fs::try_exists(sandbox.library.join(b)).await.unwrap());
    assert!(fs::try_exists(sandbox.library.join("Show/Season 01"))
        .await
        .unwrap());
}

// ---------- cross-instance link (link_*_from) ----------

#[tokio::test]
async fn symlink_movie_from_foreign_storage_links_into_this_library() {
    // Two sandboxes simulate two instances with different storage but
    // sharing the same parent directory (so the test stays portable).
    let tmp = TempDir::new().unwrap();
    let primary_storage = tmp.path().join("primary-storage");
    let alt_storage = tmp.path().join("alt-storage");
    let alt_library = tmp.path().join("alt-library");
    fs::create_dir_all(&primary_storage).await.unwrap();
    fs::create_dir_all(&alt_storage).await.unwrap();
    fs::create_dir_all(&alt_library).await.unwrap();

    write_file(&primary_storage.join("Multi (2024)/Multi.mkv"), "video").await;

    let alt = LinkManager::from_instance(&InstanceConfig {
        name: "alt".to_owned(),
        kind: InstanceKind::Radarr,
        language: "en".to_owned(),
        url: "http://alt".to_owned(),
        api_key: "k".to_owned(),
        storage_path: alt_storage.clone(),
        library_path: alt_library.clone(),
        link_strategy: LinkStrategy::Symlink,
        propagate_delete: true,
    });

    let foreign_source = primary_storage.join("Multi (2024)");
    let action = alt
        .link_movie_from(&foreign_source, "Multi (2024)")
        .await
        .unwrap();
    assert_eq!(action, LinkAction::Created);

    let target = alt_library.join("Multi (2024)");
    let resolved = fs::read_link(&target).await.unwrap();
    assert_eq!(resolved, foreign_source);
    // Content reachable through the alt-library symlink, but the file
    // physically lives under primary storage.
    assert_eq!(read_file(&target.join("Multi.mkv")).await, "video");
}

#[tokio::test]
async fn hardlink_episode_from_foreign_storage_inodes_match() {
    let tmp = TempDir::new().unwrap();
    let primary_storage = tmp.path().join("primary-storage");
    let alt_storage = tmp.path().join("alt-storage");
    let alt_library = tmp.path().join("alt-library");
    fs::create_dir_all(&primary_storage).await.unwrap();
    fs::create_dir_all(&alt_storage).await.unwrap();
    fs::create_dir_all(&alt_library).await.unwrap();

    let foreign_rel = Path::new("Show/Season 01/S01E01.mkv");
    write_file(&primary_storage.join(foreign_rel), "ep").await;

    let alt = LinkManager::from_instance(&InstanceConfig {
        name: "alt".to_owned(),
        kind: InstanceKind::Sonarr,
        language: "en".to_owned(),
        url: "http://alt".to_owned(),
        api_key: "k".to_owned(),
        storage_path: alt_storage.clone(),
        library_path: alt_library.clone(),
        link_strategy: LinkStrategy::Hardlink,
        propagate_delete: true,
    });

    let foreign_source = primary_storage.join(foreign_rel);
    alt.link_episode_from(&foreign_source, foreign_rel)
        .await
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let s = std::fs::metadata(&foreign_source).unwrap();
        let t = std::fs::metadata(alt_library.join(foreign_rel)).unwrap();
        assert_eq!(s.ino(), t.ino());
    }
}

#[tokio::test]
async fn link_movie_from_rejects_unsafe_folder_name() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    // Even with an explicit source path, the folder_name (used as the
    // relative target) must remain safe.
    let err = manager
        .link_movie_from(Path::new("/tmp/whatever"), "../escape")
        .await
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidRelativePath(_)));
}

// ---------- storage-aware probes (08b) ----------

#[tokio::test]
async fn is_last_hardlink_returns_false_for_symlink_strategy() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Show/S01E01.mkv"), "x").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    manager
        .link_episode(Path::new("Show/S01E01.mkv"))
        .await
        .unwrap();

    // Symlink instances always answer false: the concept does not apply.
    assert!(!manager
        .is_last_hardlink(Path::new("Show/S01E01.mkv"))
        .await
        .unwrap());
}

#[tokio::test]
async fn is_last_hardlink_returns_false_when_source_still_exists() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Show/S01E01.mkv"), "x").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Hardlink);
    manager
        .link_episode(Path::new("Show/S01E01.mkv"))
        .await
        .unwrap();

    // Two hardlinks to the same inode (storage + library) → nlink == 2.
    assert!(!manager
        .is_last_hardlink(Path::new("Show/S01E01.mkv"))
        .await
        .unwrap());
}

#[tokio::test]
async fn is_last_hardlink_returns_true_after_source_removed() {
    let sandbox = Sandbox::new().await;
    let source_rel = Path::new("Show/S01E01.mkv");
    write_file(&sandbox.storage.join(source_rel), "x").await;

    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Hardlink);
    manager.link_episode(source_rel).await.unwrap();
    fs::remove_file(sandbox.storage.join(source_rel))
        .await
        .unwrap();

    // Source gone — only the library hardlink owns the inode now.
    assert!(manager.is_last_hardlink(source_rel).await.unwrap());
}

#[tokio::test]
async fn is_last_hardlink_returns_false_when_path_missing() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Hardlink);
    assert!(!manager
        .is_last_hardlink(Path::new("missing.mkv"))
        .await
        .unwrap());
}

#[tokio::test]
async fn resolves_into_returns_true_when_symlink_points_under_foreign_root() {
    let tmp = TempDir::new().unwrap();
    let foreign_storage = tmp.path().join("foreign-storage");
    let alt_storage = tmp.path().join("alt-storage");
    let alt_library = tmp.path().join("alt-library");
    for d in [&foreign_storage, &alt_storage, &alt_library] {
        fs::create_dir_all(d).await.unwrap();
    }
    write_file(&foreign_storage.join("Movie/movie.mkv"), "x").await;

    let alt = LinkManager::from_instance(&InstanceConfig {
        name: "alt".to_owned(),
        kind: InstanceKind::Radarr,
        language: "en".to_owned(),
        url: "http://alt".to_owned(),
        api_key: "k".to_owned(),
        storage_path: alt_storage,
        library_path: alt_library,
        link_strategy: LinkStrategy::Symlink,
        propagate_delete: true,
    });

    alt.link_movie_from(&foreign_storage.join("Movie"), "Movie")
        .await
        .unwrap();

    assert!(alt
        .resolves_into(Path::new("Movie"), &foreign_storage)
        .await
        .unwrap());
    // Negative: the symlink does not resolve under an unrelated root.
    let unrelated = tmp.path().join("unrelated");
    assert!(!alt
        .resolves_into(Path::new("Movie"), &unrelated)
        .await
        .unwrap());
}

#[tokio::test]
async fn resolves_into_returns_false_for_hardlink_strategy() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Movie/movie.mkv"), "x").await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Hardlink);
    manager.link_movie("Movie").await.unwrap();
    // Hardlink strategy: not a symlink, can't resolve, returns false.
    assert!(!manager
        .resolves_into(Path::new("Movie"), Path::new("/tmp"))
        .await
        .unwrap());
}

#[tokio::test]
async fn unlink_folder_works_for_a_series_subtree() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Show/Season 01/S01E01.mkv"), "a").await;
    write_file(&sandbox.storage.join("Show/Season 01/S01E02.mkv"), "b").await;
    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    manager
        .link_episode(Path::new("Show/Season 01/S01E01.mkv"))
        .await
        .unwrap();
    manager
        .link_episode(Path::new("Show/Season 01/S01E02.mkv"))
        .await
        .unwrap();

    // unlink_folder("Show") should remove the entire library/Show tree.
    manager.unlink_folder("Show").await.unwrap();
    assert!(!fs::try_exists(sandbox.library.join("Show")).await.unwrap());
}

// ---------- error paths ----------

#[tokio::test]
async fn link_movie_rejects_absolute_path() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    let err = manager.link_movie("/etc/passwd").await.unwrap_err();
    assert!(matches!(err, LinkError::InvalidRelativePath(_)));
}

#[tokio::test]
async fn link_episode_rejects_parent_traversal() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Sonarr, LinkStrategy::Symlink);
    let err = manager
        .link_episode(Path::new("../../etc/passwd"))
        .await
        .unwrap_err();
    assert!(matches!(err, LinkError::InvalidRelativePath(_)));
}

#[tokio::test]
async fn link_movie_errors_when_source_missing() {
    let sandbox = Sandbox::new().await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    let err = manager.link_movie("Nothing Here").await.unwrap_err();
    assert!(matches!(err, LinkError::NotFound(_)));
}

#[tokio::test]
async fn link_movie_errors_when_source_is_a_file() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Weird"), "notadir").await;
    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    let err = manager.link_movie("Weird").await.unwrap_err();
    assert!(matches!(err, LinkError::ExpectedDirectory { .. }));
}

#[tokio::test]
async fn symlink_refuses_to_overwrite_unrelated_target() {
    let sandbox = Sandbox::new().await;
    write_file(&sandbox.storage.join("Movie/Movie.mkv"), "x").await;
    // Pre-existing bystander file at the target path.
    write_file(&sandbox.library.join("Movie"), "existing").await;

    let manager = sandbox.manager(InstanceKind::Radarr, LinkStrategy::Symlink);
    let err = manager.link_movie("Movie").await.unwrap_err();
    assert!(matches!(err, LinkError::AlreadyExists { .. }));
    // Bystander untouched.
    assert_eq!(read_file(&sandbox.library.join("Movie")).await, "existing");
}
