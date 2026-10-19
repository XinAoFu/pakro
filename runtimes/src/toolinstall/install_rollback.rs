//! Ordinary-error recovery for an explicitly selected install path.
//!
//! The caller holds the install identity, state-marker and explicit-destination
//! locks before beginning. This guard does not acquire settings, resolve tools,
//! execute runtimes, or promise crash recovery or continuous availability.
//!
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, bail, ensure};

use crate::install_layout::publication::{PublicationUndo, PublicationValue};

pub(crate) struct InstallRollback {
    install_path: PathBuf,
    marker_path: PathBuf,
    marker_before: PublicationValue,
    backup: Option<PathBuf>,
    had_previous: bool,
    payload_restored: bool,
    marker_restored: bool,
    active: bool,
    publication: PublicationUndo,
}

impl std::fmt::Debug for InstallRollback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallRollback")
            .field("install_path", &self.install_path)
            .field("backup", &self.backup)
            .field("payload_restored", &self.payload_restored)
            .field("marker_restored", &self.marker_restored)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl InstallRollback {
    pub(crate) fn begin(install_path: &Path, marker_path: &Path) -> Result<Self> {
        ensure!(
            install_path.is_absolute() && marker_path.is_absolute(),
            "install rollback requires explicit absolute install and marker paths"
        );
        ensure!(
            install_path.file_name().is_some() && marker_path.file_name().is_some(),
            "install rollback paths must name individual entries"
        );
        // Validate every bounded marker input before moving the previous tree.
        // PublicationValue captures links as raw entries, without following them.
        let marker_before =
            PublicationValue::capture(marker_path).context("capturing install rollback marker")?;
        let previous = match fs::symlink_metadata(install_path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir() || metadata.is_file() || metadata.is_symlink(),
                    "install rollback cannot preserve this install entry type"
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("inspecting previous install for rollback"),
        };
        let backup = if previous {
            let parent = install_path
                .parent()
                .context("install rollback path has no parent")?;
            let container = tempfile::Builder::new()
                .prefix(".pakro-runtime-rollback-")
                .tempdir_in(parent)
                .context("creating same-filesystem install rollback backup")?
                .keep();
            // Retain the container explicitly before the old entry moves into
            // it: a TempDir destructor must never own the only previous tree.
            if let Err(error) = fs::rename(install_path, container.join("previous")) {
                if let Err(cleanup) = fs::remove_dir(&container) {
                    warn!(
                        "could not remove unused install rollback container {}: {cleanup}",
                        container.display()
                    );
                }
                return Err(error).context("moving previous install into rollback backup");
            }
            Some(container)
        } else {
            None
        };
        Ok(Self {
            install_path: install_path.to_path_buf(),
            marker_path: marker_path.to_path_buf(),
            marker_before,
            had_previous: previous,
            backup,
            payload_restored: false,
            marker_restored: false,
            active: true,
            publication: PublicationUndo::new(),
        })
    }

    pub(crate) fn has_previous_entry(&self) -> bool {
        self.had_previous
    }

    pub(crate) fn publication(&mut self) -> &mut PublicationUndo {
        &mut self.publication
    }

    pub(crate) fn rollback(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut failures = Vec::new();
        if !self.payload_restored {
            match self.restore_payload() {
                Ok(()) => {
                    // Advance before publication rollback can fail. Retrying
                    // metadata or Drop must never delete this restored tree.
                    self.payload_restored = true;
                }
                Err(error) => failures.push(format!("restoring previous payload: {error:#}")),
            }
        }
        // Until the old tree is back, reverting aliases or clearing the active
        // marker could advertise the remaining failed prefix as installed.
        // Keep both pending so an explicit retry can finish after IO recovery.
        if self.payload_restored {
            if let Err(error) = self.publication.rollback() {
                failures.push(format!("restoring install publication: {error:#}"));
            }
            if !self.marker_restored {
                match restore_marker(&self.marker_path, &self.marker_before) {
                    Ok(()) => self.marker_restored = true,
                    Err(error) => failures.push(format!("restoring install marker: {error:#}")),
                }
            }
        } else if let Err(error) = ensure_incomplete_marker(&self.marker_path) {
            failures.push(format!("retaining failed install marker: {error:#}"));
        }
        if self.payload_restored
            && let Some(backup) = &self.backup
        {
            // The previous entry has moved back. Only the now-empty private
            // container may be removed; never recursively clean its parent.
            match fs::remove_dir(backup) {
                Ok(()) => self.backup = None,
                Err(error) => failures.push(format!(
                    "removing empty rollback container {}: {error}",
                    backup.display()
                )),
            }
        }
        if failures.is_empty() {
            self.active = false;
            Ok(())
        } else if let Some(backup) = &self.backup {
            bail!(
                "install rollback failed: {}; previous backup retained at {}",
                failures.join("; "),
                backup.display()
            )
        } else {
            bail!("install rollback failed: {}", failures.join("; "))
        }
    }

    pub(crate) fn commit(&mut self) {
        if !self.active {
            return;
        }
        // The successful installation is committed before fallible garbage
        // cleanup. A partially removed backup can never become rollback input.
        self.active = false;
        self.publication.commit();
        if let Some(backup) = self.backup.take()
            && let Err(error) = remove_entry(&backup)
        {
            warn!(
                "installed runtime committed; could not clean previous backup {}: {error:#}",
                backup.display()
            );
        }
    }

    fn restore_payload(&self) -> Result<()> {
        if let Some(backup) = &self.backup {
            fs::symlink_metadata(backup.join("previous"))
                .context("locating previous payload in rollback backup")?;
        }
        remove_entry(&self.install_path).context("removing failed install entry")?;
        if let Some(backup) = &self.backup {
            fs::rename(backup.join("previous"), &self.install_path)
                .context("restoring original install entry from rollback backup")?;
        }
        Ok(())
    }
}

impl Drop for InstallRollback {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.rollback()
        {
            warn!("best-effort install rollback did not complete: {error:#}");
        }
    }
}

fn remove_entry(path: &Path) -> std::io::Result<()> {
    let kind = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if kind.is_dir() && !kind.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn restore_marker(path: &Path, previous: &PublicationValue) -> Result<()> {
    if PublicationValue::capture(path).is_ok_and(|current| &current == previous) {
        return Ok(());
    }
    match previous {
        PublicationValue::Missing => remove_entry(path).map_err(Into::into),
        PublicationValue::Regular { bytes, permissions } => {
            let parent = path.parent().context("install marker path has no parent")?;
            fs::create_dir_all(parent).context("creating install marker parent")?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(bytes)?;
            temporary.flush()?;
            temporary.as_file().set_permissions(permissions.clone())?;
            temporary.as_file().sync_all()?;
            // Same-directory rename replaces a raw symlink or a hardlink entry,
            // rather than truncating an external/shared target inode.
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .context("restoring previous install marker")?;
            Ok(())
        }
        PublicationValue::Symlink { target } => {
            remove_entry(path)?;
            fs::create_dir_all(path.parent().context("install marker has no parent")?)?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, path)
                    .context("restoring raw install marker symlink")?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = target;
                bail!("raw install marker symlink recovery requires Unix")
            }
        }
    }
}

fn ensure_incomplete_marker(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        // An existing regular marker already keeps the prefix incomplete.
        // Do not truncate it: it may share an inode with an external file.
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => return Ok(()),
        Ok(metadata) => ensure!(
            metadata.is_symlink(),
            "incomplete marker entry is not a regular file or symlink"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspecting incomplete install marker"),
    }
    let parent = path
        .parent()
        .context("incomplete install marker has no parent")?;
    fs::create_dir_all(parent).context("creating incomplete install marker parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    // Atomic replacement of the directory entry, never open/truncate through
    // an existing (possibly dangling) symlink to some external destination.
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("retaining incomplete install marker without following links")?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::PathBuf;

    use crate::install_layout::publication::PublicationValue;

    const OLD_PYTHON: &[u8] = b"#!/bin/sh\nprintf 'old working runtime\\n'\n";
    const OLD_RECEIPT: &[u8] = b"digest = \"old-runtime-identity\"\n";
    const OLD_MARKER: &[u8] = b"previous state marker\n";

    struct Fixture {
        _owner: tempfile::TempDir,
        root: PathBuf,
        parent: PathBuf,
        install: PathBuf,
        marker: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let owner = tempfile::tempdir().unwrap();
            let root = owner.path().canonicalize().unwrap();
            let parent = root.join("安装 store 中文");
            let marker = root.join("cache/state-key/incomplete");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            Self {
                install: parent.join("python-runtime"),
                marker,
                parent,
                root,
                _owner: owner,
            }
        }

        fn old_directory(&self) {
            fs::create_dir_all(self.install.join("bin")).unwrap();
            fs::write(self.install.join("bin/python"), OLD_PYTHON).unwrap();
            fs::set_permissions(
                self.install.join("bin/python"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            fs::write(self.install.join(".pakro-install.toml"), OLD_RECEIPT).unwrap();
            fs::set_permissions(
                self.install.join(".pakro-install.toml"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
            fs::set_permissions(&self.install, fs::Permissions::from_mode(0o750)).unwrap();
            symlink("python", self.install.join("bin/python3")).unwrap();
            fs::write(&self.marker, OLD_MARKER).unwrap();
            fs::set_permissions(&self.marker, fs::Permissions::from_mode(0o640)).unwrap();
        }

        fn failed_install(&self) {
            fs::create_dir_all(self.install.join("bin")).unwrap();
            fs::write(self.install.join("bin/python"), b"failed new runtime").unwrap();
            fs::write(
                self.install.join("partial-new-file"),
                b"new partial payload",
            )
            .unwrap();
            fs::write(&self.marker, b"new incomplete state").unwrap();
        }

        fn assert_old_directory(&self) {
            assert_eq!(
                fs::read(self.install.join("bin/python")).unwrap(),
                OLD_PYTHON
            );
            assert_eq!(
                fs::read(self.install.join(".pakro-install.toml")).unwrap(),
                OLD_RECEIPT
            );
            assert_eq!(mode(&self.install), 0o750);
            assert_eq!(mode(&self.install.join("bin/python")), 0o755);
            assert_eq!(mode(&self.install.join(".pakro-install.toml")), 0o640);
            assert_eq!(
                fs::read_link(self.install.join("bin/python3")).unwrap(),
                PathBuf::from("python")
            );
            assert!(!self.install.join("partial-new-file").exists());
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn absent(path: &Path) -> bool {
        fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    }

    fn backups(parent: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".pakro-runtime-rollback-")
            })
            .collect();
        paths.sort();
        paths
    }

    fn track_selector_change(guard: &mut InstallRollback, fixture: &Fixture) -> PathBuf {
        let selector = fixture.root.join("owned-selection.toml");
        let lock_path = fixture.root.join("alloc.lock");
        let _held = crate::lock_file::LockFile::at(&lock_path).lock().unwrap();
        fs::write(&selector, b"selected=previous-runtime\n").unwrap();
        fs::set_permissions(&selector, fs::Permissions::from_mode(0o640)).unwrap();
        let before = PublicationValue::Regular {
            bytes: fs::read(&selector).unwrap(),
            permissions: fs::metadata(&selector).unwrap().permissions(),
        };
        fs::write(&selector, b"selected=new-runtime\n").unwrap();
        fs::set_permissions(&selector, fs::Permissions::from_mode(0o600)).unwrap();
        let written = PublicationValue::Regular {
            bytes: fs::read(&selector).unwrap(),
            permissions: fs::metadata(&selector).unwrap().permissions(),
        };
        guard
            .publication()
            .track_change(&lock_path, &selector, before, written)
            .unwrap();
        selector
    }

    #[test]
    fn rollback_consumes_nonempty_publication_undo_and_restores_the_payload() {
        let fixture = Fixture::new();
        fixture.old_directory();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(absent(&fixture.install));
        let selector = track_selector_change(&mut guard, &fixture);
        fixture.failed_install();
        guard.rollback().unwrap();
        fixture.assert_old_directory();
        assert_eq!(fs::read(&selector).unwrap(), b"selected=previous-runtime\n");
        assert_eq!(mode(&selector), 0o640);
        assert_eq!(fs::read(&fixture.marker).unwrap(), OLD_MARKER);
        assert!(backups(&fixture.parent).is_empty());
        drop(guard);
        fixture.assert_old_directory();
        assert_eq!(fs::read(selector).unwrap(), b"selected=previous-runtime\n");
    }

    #[test]
    fn publication_conflict_after_payload_restore_cannot_make_drop_delete_the_old_tree() {
        let fixture = Fixture::new();
        fixture.old_directory();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(absent(&fixture.install));
        let selector = track_selector_change(&mut guard, &fixture);
        fixture.failed_install();
        {
            let _held = crate::lock_file::LockFile::at(&fixture.root.join("alloc.lock"))
                .lock()
                .unwrap();
            fs::write(&selector, b"selected=concurrent-owner\n").unwrap();
        }
        let error = guard.rollback().unwrap_err();
        assert!(error.to_string().contains("changed"), "{error:#}");
        fixture.assert_old_directory();
        assert_eq!(fs::read(&selector).unwrap(), b"selected=concurrent-owner\n");
        drop(guard);
        fixture.assert_old_directory();
        assert_eq!(fs::read(selector).unwrap(), b"selected=concurrent-owner\n");
        assert_eq!(fs::read(&fixture.marker).unwrap(), OLD_MARKER);
        assert_eq!(mode(&fixture.marker), 0o640);
    }

    #[test]
    fn directory_receipt_modes_inode_and_marker_are_restored_at_the_same_prefix() {
        let fixture = Fixture::new();
        fixture.old_directory();
        let before = fs::metadata(fixture.install.join("bin/python")).unwrap();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(guard.has_previous_entry());
        assert!(
            absent(&fixture.install),
            "begin must move the old entry aside"
        );
        let held = backups(&fixture.parent);
        assert_eq!(held.len(), 1);
        let previous = held[0].join("previous");
        assert_eq!(fs::read(previous.join("bin/python")).unwrap(), OLD_PYTHON);
        assert_eq!(fs::metadata(&previous).unwrap().dev(), before.dev());
        assert_eq!(
            fs::metadata(previous.join("bin/python")).unwrap().ino(),
            before.ino()
        );
        fixture.failed_install();
        let _publication = guard.publication();
        guard.rollback().unwrap();
        fixture.assert_old_directory();
        let restored = fs::metadata(fixture.install.join("bin/python")).unwrap();
        assert_eq!(
            (restored.dev(), restored.ino()),
            (before.dev(), before.ino())
        );
        assert_eq!(fs::read(&fixture.marker).unwrap(), OLD_MARKER);
        assert_eq!(mode(&fixture.marker), 0o640);
        assert!(backups(&fixture.parent).is_empty());
        guard.rollback().unwrap();
        drop(guard);
        fixture.assert_old_directory();
    }

    #[test]
    fn raw_install_symlinks_including_dangling_links_are_restored_without_following() {
        for target in ["../outside runtime", "../missing runtime"] {
            let fixture = Fixture::new();
            let outside = fixture.root.join("outside runtime");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep"), b"external original bytes").unwrap();
            symlink(target, &fixture.install).unwrap();
            let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
            assert!(guard.has_previous_entry());
            assert!(absent(&fixture.install));
            let held = backups(&fixture.parent);
            assert_eq!(held.len(), 1);
            assert_eq!(
                fs::read_link(held[0].join("previous")).unwrap(),
                PathBuf::from(target)
            );
            fixture.failed_install();
            guard.rollback().unwrap();
            assert!(fs::symlink_metadata(&fixture.install).unwrap().is_symlink());
            assert_eq!(
                fs::read_link(&fixture.install).unwrap(),
                PathBuf::from(target)
            );
            assert_eq!(
                fs::read(outside.join("keep")).unwrap(),
                b"external original bytes"
            );
            assert!(absent(&fixture.marker));
            assert!(backups(&fixture.parent).is_empty());
        }
    }

    #[test]
    fn fresh_target_rollback_removes_only_the_failed_entry_and_its_new_marker() {
        let fixture = Fixture::new();
        let unrelated = fixture
            .marker
            .parent()
            .unwrap()
            .join("other-version-marker");
        fs::write(&unrelated, b"another install is in progress").unwrap();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(!guard.has_previous_entry());
        fixture.failed_install();
        guard.rollback().unwrap();
        assert!(absent(&fixture.install));
        assert!(absent(&fixture.marker));
        assert_eq!(
            fs::read(unrelated).unwrap(),
            b"another install is in progress"
        );
        assert!(backups(&fixture.parent).is_empty());
    }

    #[test]
    fn commit_keeps_the_successful_tree_and_cleans_only_the_old_backup() {
        let fixture = Fixture::new();
        fixture.old_directory();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(absent(&fixture.install));
        assert_eq!(backups(&fixture.parent).len(), 1);
        fixture.failed_install();
        fs::remove_file(&fixture.marker).unwrap();
        guard.commit();
        drop(guard);
        assert_eq!(
            fs::read(fixture.install.join("bin/python")).unwrap(),
            b"failed new runtime"
        );
        assert!(fixture.install.join("partial-new-file").exists());
        assert!(absent(&fixture.marker));
        assert!(backups(&fixture.parent).is_empty());
    }

    #[test]
    fn drop_restores_the_previous_tree_and_marker_as_a_best_effort_fallback() {
        let fixture = Fixture::new();
        fixture.old_directory();
        {
            let guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
            assert!(guard.has_previous_entry());
            assert!(absent(&fixture.install));
            fixture.failed_install();
        }
        fixture.assert_old_directory();
        assert_eq!(fs::read(&fixture.marker).unwrap(), OLD_MARKER);
        assert_eq!(mode(&fixture.marker), 0o640);
        assert!(backups(&fixture.parent).is_empty());
    }

    #[test]
    fn rollback_io_error_and_drop_preserve_the_only_previous_backup() {
        let fixture = Fixture::new();
        fixture.old_directory();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(absent(&fixture.install));
        let held = backups(&fixture.parent);
        assert_eq!(held.len(), 1);
        let backup_name = held[0].file_name().unwrap().to_owned();
        let detached = fixture.root.join("detached original parent");
        fs::rename(&fixture.parent, &detached).unwrap();
        fs::write(&fixture.parent, b"the original parent path is now a file").unwrap();
        let error = guard.rollback().unwrap_err();
        assert!(error.to_string().contains("rollback"), "{error:#}");
        drop(guard);
        let previous = detached.join(backup_name).join("previous");
        assert_eq!(fs::read(previous.join("bin/python")).unwrap(), OLD_PYTHON);
        assert_eq!(
            fs::read(previous.join(".pakro-install.toml")).unwrap(),
            OLD_RECEIPT
        );
        assert_eq!(mode(&previous), 0o750);
        assert_eq!(fs::read(&fixture.marker).unwrap(), OLD_MARKER);
        assert_eq!(
            fs::read(&fixture.parent).unwrap(),
            b"the original parent path is now a file"
        );
    }

    #[test]
    fn unrestored_payload_stays_incomplete_and_published_changes_wait_for_retry() {
        let fixture = Fixture::new();
        fixture.old_directory();
        fs::remove_file(&fixture.marker).unwrap();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        let selector = track_selector_change(&mut guard, &fixture);
        fixture.failed_install();
        let held = backups(&fixture.parent);
        assert_eq!(held.len(), 1);
        let previous = held[0].join("previous");
        let unavailable = fixture
            .root
            .join("temporarily unavailable previous payload");
        fs::rename(&previous, &unavailable).unwrap();

        let error = guard.rollback().unwrap_err();
        assert!(error.to_string().contains("rollback"), "{error:#}");
        assert_eq!(
            fs::read(fixture.install.join("bin/python")).unwrap(),
            b"failed new runtime"
        );
        assert!(
            fixture.marker.is_file(),
            "a failed prefix must stay incomplete until the original payload is restored"
        );
        assert_eq!(fs::read(&fixture.marker).unwrap(), b"new incomplete state");
        assert_eq!(fs::read(&selector).unwrap(), b"selected=new-runtime\n");
        assert_eq!(
            fs::read(unavailable.join("bin/python")).unwrap(),
            OLD_PYTHON
        );
        assert_eq!(
            fs::read(unavailable.join(".pakro-install.toml")).unwrap(),
            OLD_RECEIPT
        );

        fs::rename(unavailable, previous).unwrap();
        guard.rollback().unwrap();
        fixture.assert_old_directory();
        assert!(absent(&fixture.marker));
        assert_eq!(fs::read(selector).unwrap(), b"selected=previous-runtime\n");
        assert!(backups(&fixture.parent).is_empty());
    }

    #[test]
    fn unavailable_payload_recreates_a_nofollow_incomplete_marker_after_cleanup() {
        for marker_kind in ["missing", "dangling", "symlink", "hardlink"] {
            let fixture = Fixture::new();
            fixture.old_directory();
            fs::remove_file(&fixture.marker).unwrap();
            let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
            let selector = track_selector_change(&mut guard, &fixture);
            fixture.failed_install();
            fs::remove_file(&fixture.marker).unwrap();
            let external = fixture.root.join("external marker destination");
            match marker_kind {
                "missing" => {}
                "dangling" => symlink(&external, &fixture.marker).unwrap(),
                "symlink" => {
                    fs::write(&external, b"external bytes must not be changed").unwrap();
                    symlink(&external, &fixture.marker).unwrap();
                }
                "hardlink" => {
                    fs::write(&external, b"external bytes must not be changed").unwrap();
                    fs::hard_link(&external, &fixture.marker).unwrap();
                }
                _ => unreachable!(),
            }
            let held = backups(&fixture.parent);
            assert_eq!(held.len(), 1);
            let previous = held[0].join("previous");
            let unavailable = fixture.root.join("temporarily held previous payload");
            fs::rename(&previous, &unavailable).unwrap();

            let error = guard.rollback().unwrap_err();
            assert!(error.to_string().contains("rollback"), "{error:#}");
            let marker = fs::symlink_metadata(&fixture.marker).unwrap_or_else(|error| {
                panic!("{marker_kind}: unrestored prefix lost its incomplete marker: {error}")
            });
            assert!(marker.is_file() && !marker.is_symlink(), "{marker_kind}");
            assert_eq!(fs::read(&selector).unwrap(), b"selected=new-runtime\n");
            assert_eq!(
                fs::read(unavailable.join("bin/python")).unwrap(),
                OLD_PYTHON
            );
            if matches!(marker_kind, "missing" | "dangling") {
                assert!(
                    absent(&external),
                    "marker creation followed a dangling link"
                );
            } else {
                assert_eq!(
                    fs::read(&external).unwrap(),
                    b"external bytes must not be changed",
                    "{marker_kind}"
                );
            }

            fs::rename(unavailable, previous).unwrap();
            guard.rollback().unwrap();
            fixture.assert_old_directory();
            assert!(absent(&fixture.marker));
            assert_eq!(fs::read(selector).unwrap(), b"selected=previous-runtime\n");
            assert!(backups(&fixture.parent).is_empty());
        }
    }

    #[test]
    fn invalid_or_oversized_marker_is_rejected_before_moving_the_old_tree() {
        for directory in [false, true] {
            let fixture = Fixture::new();
            fixture.old_directory();
            if directory {
                fs::remove_file(&fixture.marker).unwrap();
                fs::create_dir(&fixture.marker).unwrap();
            } else {
                fs::write(&fixture.marker, vec![b'x'; 1024 * 1024 + 1]).unwrap();
            }
            let error = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap_err();
            assert!(error.to_string().contains("marker"), "{error:#}");
            fixture.assert_old_directory();
            assert!(backups(&fixture.parent).is_empty());
        }
    }

    #[test]
    fn symlink_marker_is_restored_as_an_entry_without_writing_its_external_target() {
        let fixture = Fixture::new();
        fixture.old_directory();
        fs::remove_file(&fixture.marker).unwrap();
        let external = fixture.root.join("external marker bytes");
        fs::write(&external, b"external marker must stay untouched").unwrap();
        symlink(&external, &fixture.marker).unwrap();
        let mut guard = InstallRollback::begin(&fixture.install, &fixture.marker).unwrap();
        assert!(absent(&fixture.install));
        fs::remove_file(&fixture.marker).unwrap();
        fixture.failed_install();
        guard.rollback().unwrap();
        fixture.assert_old_directory();
        assert!(fs::symlink_metadata(&fixture.marker).unwrap().is_symlink());
        assert_eq!(fs::read_link(&fixture.marker).unwrap(), external);
        assert_eq!(
            fs::read(&external).unwrap(),
            b"external marker must stay untouched"
        );
        assert!(backups(&fixture.parent).is_empty());
    }
}
