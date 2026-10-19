//! Undo only publication entries written by one installation.
//!
//! Callers capture before/after values in the same explicit catalog critical
//! section as the actual atomic write, then register that completed mutation.
//! Rollback compares each current value under that same lock before restoring
//! it. No global catalog snapshot, marker state or runtime payload lives here.

use std::fmt;
use std::fs::{self, Permissions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use eyre::Result;

pub(crate) const MAX_ENTRY_BYTES: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PublicationValue {
    Missing,
    Regular {
        bytes: Vec<u8>,
        permissions: Permissions,
    },
    Symlink {
        target: PathBuf,
    },
}

impl fmt::Debug for PublicationValue {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => output.write_str("Missing"),
            Self::Regular { bytes, .. } => output
                .debug_struct("Regular")
                .field("bytes", &bytes.len())
                .finish_non_exhaustive(),
            Self::Symlink { .. } => output.debug_struct("Symlink").finish_non_exhaustive(),
        }
    }
}

impl PublicationValue {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Missing),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)?;
            eyre::ensure!(
                target.as_os_str().as_encoded_bytes().len() <= MAX_ENTRY_BYTES,
                "publication link exceeds the metadata size limit"
            );
            return Ok(Self::Symlink { target });
        }
        eyre::ensure!(
            metadata.is_file(),
            "publication entry is not a regular file or link"
        );
        eyre::ensure!(
            metadata.len() <= MAX_ENTRY_BYTES as u64,
            "publication entry exceeds the metadata size limit"
        );
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(nix::libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        let opened = file.metadata()?;
        eyre::ensure!(opened.is_file(), "publication entry is not a regular file");
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        eyre::ensure!(
            bytes.len() <= MAX_ENTRY_BYTES,
            "publication entry exceeds the metadata size limit"
        );
        Ok(Self::Regular {
            bytes,
            permissions: opened.permissions(),
        })
    }

    fn restore(&self, path: &Path) -> Result<()> {
        match self {
            Self::Missing => match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
            Self::Regular { bytes, permissions } => {
                let parent = path
                    .parent()
                    .ok_or_else(|| eyre::eyre!("publication entry parent is missing"))?;
                fs::create_dir_all(parent)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(bytes)?;
                temporary.as_file().set_permissions(permissions.clone())?;
                temporary.flush()?;
                temporary.as_file().sync_all()?;
                temporary.persist(path).map_err(|error| error.error)?;
                Ok(())
            }
            Self::Symlink { target } => restore_symlink(target, path),
        }
    }
}

#[cfg(unix)]
fn restore_symlink(target: &Path, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("publication link parent is missing"))?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::tempdir_in(parent)?;
    let staged = temporary.path().join("link");
    std::os::unix::fs::symlink(target, &staged)?;
    fs::rename(staged, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn restore_symlink(_target: &Path, _path: &Path) -> Result<()> {
    eyre::bail!("publication link recovery requires a supported POSIX platform")
}

#[derive(Debug)]
struct EntryUndo {
    lock_path: PathBuf,
    path: PathBuf,
    before: PublicationValue,
    written: PublicationValue,
}

impl EntryUndo {
    fn rollback(&self) -> Result<()> {
        let _lock = crate::lock_file::LockFile::at(&self.lock_path).lock()?;
        let current = PublicationValue::capture(&self.path)?;
        if current == self.before {
            return Ok(());
        }
        eyre::ensure!(
            current == self.written,
            "publication entry changed after it was written"
        );
        self.before.restore(&self.path)
    }
}

#[derive(Debug, Default)]
pub(crate) struct PublicationUndo {
    entries: Vec<EntryUndo>,
}

impl PublicationUndo {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register an atomic mutation that the caller has already completed while
    /// holding `lock_path`. Never infer the written value by reading after that
    /// critical section, since another owner may have legitimately changed it.
    pub(crate) fn track_change(
        &mut self,
        lock_path: &Path,
        path: &Path,
        before: PublicationValue,
        written: PublicationValue,
    ) -> Result<()> {
        // The finite publication steps, rather than untrusted metadata rows,
        // supply entries. Registration itself cannot reject a completed write
        // and lose the only undo information for it.
        if before != written {
            self.entries.push(EntryUndo {
                lock_path: lock_path.to_path_buf(),
                path: path.to_path_buf(),
                before,
                written,
            });
        }
        Ok(())
    }

    pub(crate) fn rollback(&mut self) -> Result<()> {
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for entry in std::mem::take(&mut self.entries).into_iter().rev() {
            if let Err(error) = entry.rollback() {
                errors.push(error.to_string());
                remaining.push(entry);
            }
        }
        remaining.reverse();
        self.entries = remaining;
        eyre::ensure!(
            errors.is_empty(),
            "publication rollback failed for {} entries: {}",
            errors.len(),
            errors.join("; ")
        );
        Ok(())
    }

    pub(crate) fn commit(&mut self) {
        self.entries.clear();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn value(bytes: &[u8], mode: u32) -> PublicationValue {
        PublicationValue::Regular {
            bytes: bytes.to_vec(),
            // stat-backed Permissions include the regular-file type bits;
            // construct the same independent expected value for CAS tests.
            permissions: Permissions::from_mode(0o100000 | mode),
        }
    }

    fn write(path: &Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
    }

    fn record(
        undo: &mut PublicationUndo,
        root: &Path,
        path: &Path,
        before: PublicationValue,
        written: PublicationValue,
    ) {
        undo.track_change(&root.join("alloc.lock"), path, before, written)
            .unwrap();
    }

    #[test]
    fn empty_undo_and_missing_snapshot_preserve_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let unrelated = root.path().join("other-record");
        fs::write(&unrelated, b"other owner").unwrap();
        assert_eq!(
            PublicationValue::capture(&root.path().join("missing")).unwrap(),
            PublicationValue::Missing
        );
        let mut undo = PublicationUndo::new();
        undo.rollback().unwrap();
        undo.commit();
        assert_eq!(fs::read(unrelated).unwrap(), b"other owner");
    }

    #[test]
    fn regular_snapshot_captures_exact_bytes_permissions_and_redacts_debug() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        write(&path, b"url=https://alice:secret@private.invalid/", 0o640);
        let captured = PublicationValue::capture(&path).unwrap();
        assert_eq!(
            captured,
            value(b"url=https://alice:secret@private.invalid/", 0o640)
        );
        let debug = format!("{captured:?}");
        for secret in ["alice", "secret", "private.invalid"] {
            assert!(!debug.contains(secret), "{debug}");
        }
    }

    #[test]
    fn symlink_snapshot_keeps_the_raw_target_without_following_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("alias");
        let target = PathBuf::from("../missing-original");
        symlink(&target, &path).unwrap();
        assert_eq!(
            PublicationValue::capture(&path).unwrap(),
            PublicationValue::Symlink { target }
        );
    }

    #[test]
    fn snapshots_reject_directories_and_oversized_metadata() {
        let root = tempfile::tempdir().unwrap();
        assert!(PublicationValue::capture(root.path()).is_err());
        let path = root.path().join("oversized-record");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_ENTRY_BYTES as u64 + 1)
            .unwrap();
        assert!(PublicationValue::capture(&path).is_err());
    }

    #[test]
    fn rollback_restores_a_record_and_its_previous_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        write(&path, b"new record", 0o600);
        let mut undo = PublicationUndo::new();
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"old record", 0o640),
            value(b"new record", 0o600),
        );
        undo.rollback().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old record");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        undo.rollback().unwrap();
    }

    #[test]
    fn rollback_removes_only_a_newly_published_owned_record() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fresh-record");
        let other = root.path().join("other-record");
        write(&path, b"mine", 0o600);
        write(&other, b"other", 0o600);
        let mut undo = PublicationUndo::new();
        record(
            &mut undo,
            root.path(),
            &path,
            PublicationValue::Missing,
            value(b"mine", 0o600),
        );
        undo.rollback().unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(other).unwrap(), b"other");
    }

    #[test]
    fn rollback_restores_only_the_owned_selector() {
        let root = tempfile::tempdir().unwrap();
        let selector = root.path().join("selector-one");
        let other = root.path().join("selector-other");
        write(&selector, b"selected=new", 0o644);
        write(&other, b"selected=someone-else", 0o644);
        let mut undo = PublicationUndo::new();
        record(
            &mut undo,
            root.path(),
            &selector,
            value(b"selected=old", 0o644),
            value(b"selected=new", 0o644),
        );
        undo.rollback().unwrap();
        assert_eq!(fs::read(selector).unwrap(), b"selected=old");
        assert_eq!(fs::read(other).unwrap(), b"selected=someone-else");
    }

    #[test]
    fn rollback_restores_raw_link_targets_and_removes_fresh_links() {
        let root = tempfile::tempdir().unwrap();
        for previous in [Some(PathBuf::from("../old-hash")), None] {
            let path = root.path().join(if previous.is_some() {
                "alias"
            } else {
                "fresh-alias"
            });
            symlink("../new-hash", &path).unwrap();
            let mut undo = PublicationUndo::new();
            let before = previous
                .clone()
                .map_or(PublicationValue::Missing, |target| {
                    PublicationValue::Symlink { target }
                });
            record(
                &mut undo,
                root.path(),
                &path,
                before,
                PublicationValue::Symlink {
                    target: "../new-hash".into(),
                },
            );
            undo.rollback().unwrap();
            if let Some(target) = previous {
                assert_eq!(fs::read_link(&path).unwrap(), target);
            } else {
                assert!(fs::symlink_metadata(path).is_err());
            }
        }
    }

    #[test]
    fn repeated_writes_to_one_record_are_undone_in_reverse_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        let mut undo = PublicationUndo::new();
        write(&path, b"middle", 0o600);
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"original", 0o640),
            value(b"middle", 0o600),
        );
        write(&path, b"last", 0o644);
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"middle", 0o600),
            value(b"last", 0o644),
        );
        undo.rollback().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"original");
    }

    #[test]
    fn partial_publication_failure_still_undoes_completed_mutations() {
        let root = tempfile::tempdir().unwrap();
        let record_path = root.path().join("record");
        let selector = root.path().join("selector");
        let blocked = root.path().join("cannot-write-a-file-here");
        fs::create_dir(&blocked).unwrap();
        let mut undo = PublicationUndo::new();
        write(&record_path, b"record-new", 0o600);
        record(
            &mut undo,
            root.path(),
            &record_path,
            value(b"record-old", 0o600),
            value(b"record-new", 0o600),
        );
        write(&selector, b"selector-new", 0o644);
        record(
            &mut undo,
            root.path(),
            &selector,
            value(b"selector-old", 0o644),
            value(b"selector-new", 0o644),
        );
        assert!(fs::write(&blocked, b"unpublished").is_err());
        undo.rollback().unwrap();
        assert_eq!(fs::read(record_path).unwrap(), b"record-old");
        assert_eq!(fs::read(selector).unwrap(), b"selector-old");
        assert!(blocked.is_dir());
    }

    #[test]
    fn concurrent_pin_adoption_is_preserved_while_other_owned_changes_are_undone() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        let owned = root.path().join("owned-selector");
        let other = root.path().join("other-record");
        let mut undo = PublicationUndo::new();
        write(&owned, b"new", 0o600);
        record(
            &mut undo,
            root.path(),
            &owned,
            value(b"old", 0o600),
            value(b"new", 0o600),
        );
        write(&path, b"pins=ours", 0o600);
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"pins=old", 0o600),
            value(b"pins=ours", 0o600),
        );
        // Reverse rollback sees the conflict first. It must still undo the
        // earlier owned selector rather than return on the first conflict.
        write(&path, b"pins=ours,concurrent", 0o600);
        write(&other, b"unrelated concurrent owner", 0o600);
        let error = undo.rollback().unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(fs::read(path).unwrap(), b"pins=ours,concurrent");
        assert_eq!(fs::read(owned).unwrap(), b"old");
        assert_eq!(fs::read(other).unwrap(), b"unrelated concurrent owner");
    }

    #[test]
    fn concurrent_selection_and_link_changes_are_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selector");
        let link = root.path().join("alias");
        let mut undo = PublicationUndo::new();
        write(&path, b"new", 0o600);
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"old", 0o600),
            value(b"new", 0o600),
        );
        symlink("new", &link).unwrap();
        record(
            &mut undo,
            root.path(),
            &link,
            PublicationValue::Symlink {
                target: "old".into(),
            },
            PublicationValue::Symlink {
                target: "new".into(),
            },
        );
        write(&path, b"chosen-by-other", 0o600);
        fs::remove_file(&link).unwrap();
        symlink("chosen-by-other", &link).unwrap();
        assert!(undo.rollback().is_err());
        assert_eq!(fs::read(path).unwrap(), b"chosen-by-other");
        assert_eq!(fs::read_link(link).unwrap(), Path::new("chosen-by-other"));
    }

    #[test]
    fn commit_discards_undo_without_reverting_the_written_value() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        write(&path, b"new", 0o600);
        let mut undo = PublicationUndo::new();
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"old", 0o600),
            value(b"new", 0o600),
        );
        undo.commit();
        undo.rollback().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"new");
    }

    #[test]
    fn rollback_takes_the_explicit_shared_catalog_lock_before_comparing_values() {
        use std::sync::mpsc;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        let lock_path = root.path().join("alloc.lock");
        write(&path, b"new", 0o600);
        let mut undo = PublicationUndo::new();
        record(
            &mut undo,
            root.path(),
            &path,
            value(b"old", 0o600),
            value(b"new", 0o600),
        );
        let held = crate::lock_file::LockFile::at(&lock_path).lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = undo.rollback();
            done_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"old");
    }
}
