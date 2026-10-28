//! Independent owned file projections. Every mechanism keeps the writer's
//! ownership log and checks the actual source and output; none uses hardlinks.
use super::verify::PrefixExpectedFile;
use super::*;
use eyre::ContextCompat;
use sha2::{Digest as _, Sha256};
use std::os::unix::fs::FileExt;

#[cfg(test)]
thread_local! {
    static REFLINK_ATTEMPT: std::cell::Cell<Option<fn(&File, &File) -> io::Result<()>>> = const { std::cell::Cell::new(None) };
}

fn stamp(file: &File) -> Result<(u64, u64, u32, u32, u64, u64, i64, i64, i64, i64)> {
    let m = file.metadata()?;
    Ok((
        m.dev(),
        m.ino(),
        m.uid(),
        m.mode(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}

impl UnpublishedPrefix {
    fn verify_clone_bytes(&self, file: &File, expected: &PrefixExpectedFile) -> Result<()> {
        let before = stamp(file)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == self.expected_uid
                && metadata.nlink() == 1
                && metadata.len() == expected.size
                && metadata.mode() & 0o7777 == expected.mode,
            "clone input/output type, owner, links, size or mode differs"
        );
        let mut digest = Sha256::new();
        let mut offset = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        while offset < expected.size {
            self.check_write_ready()?;
            let count = usize::try_from((expected.size - offset).min(buffer.len() as u64))?;
            let read = file.read_at(&mut buffer[..count], offset)?;
            ensure!(read != 0, "clone input/output ended early");
            digest.update(&buffer[..read]);
            offset += read as u64;
        }
        ensure!(
            stamp(file)? == before && <[u8; 32]>::from(digest.finalize()) == expected.sha256,
            "clone input/output bytes changed from their verified inventory"
        );
        Ok(())
    }

    pub(crate) fn clone_file_exclusive(
        &mut self,
        source: &File,
        expected: &PrefixExpectedFile,
    ) -> Result<PrefixFileReceipt> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (source, expected);
            bail!(
                "native namespace projection requires supported copy-on-write cloning; ordinary copies are forbidden"
            )
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            self.check_write_ready()?;
            self.validate_relative(&expected.relative, false)?;
            ensure!(expected.mode & !0o777 == 0, "invalid clone output mode");
            ensure!(
                !self.entries.contains_key(&expected.relative),
                "clone output already exists"
            );
            self.check_entry_budget()?;
            let total = self
                .written_bytes
                .checked_add(expected.size)
                .filter(|n| *n <= self.limits.max_total_bytes)
                .ok_or_else(|| eyre::eyre!("clone output byte budget exceeded"))?;
            self.verify_clone_bytes(source, expected)?;
            let source_stamp = stamp(source)?;
            let parent = self.open_owned_parent(&expected.relative)?;
            let name = expected
                .relative
                .file_name()
                .context("missing clone output name")?;
            let name_c = std::ffi::CString::new(name.as_bytes())?;
            let operation =
                self.observe_create_intent(EntryKind::File, &expected.relative, &parent)?;
            let mut created_actual = false;
            let result = (|| {
                self.check_namespace()?;
                // CLONE_NOFOLLOW | CLONE_NOOWNERCOPY; the source is already an
                // authenticated FD and the target is one leaf under a held FD.
                let result = unsafe {
                    nix::libc::fclonefileat(
                        source.as_raw_fd(),
                        parent.as_raw_fd(),
                        name_c.as_ptr(),
                        0x0001 | 0x0002,
                    )
                };
                if result != 0 {
                    return Err(eyre::eyre!(
                        "copy-on-write clone failed (no ordinary-copy fallback): {}",
                        io::Error::last_os_error()
                    ));
                }
                created_actual = true;
                let file = File::from(openat(
                    &parent,
                    name,
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                    Mode::empty(),
                )?);
                let identity = FileIdentity::of(&file)?;
                self.entries.insert(
                    expected.relative.clone(),
                    TrackedEntry {
                        identity,
                        kind: EntryKind::File,
                    },
                );
                parent.sync_all()?;
                self.observe_created(operation, &file)?;
                fchmod(&file, Mode::from_bits_truncate(expected.mode as _))?;
                ensure!(
                    identity != FileIdentity::of(source)?,
                    "clone reused the source inode"
                );
                ensure!(
                    stamp(source)? == source_stamp,
                    "clone source changed during cloning"
                );
                self.verify_clone_bytes(&file, expected)?;
                self.check_namespace()?;
                Ok((file, identity))
            })();
            let (file, identity) = match result {
                Ok(value) => value,
                Err(error) => {
                    self.tainted |= created_actual;
                    self.retain_uncertain_creation(operation);
                    return Err(error);
                }
            };
            self.written_bytes = total;
            PrefixFile {
                prefix: self,
                relative: expected.relative.clone(),
                mode: expected.mode,
                file,
                parent,
                identity,
                size: expected.size,
                finished: false,
                ownership_operation: operation,
            }
            .finish()
        }
    }

    /// Copy exactly one held, inventoried file into a new owned regular inode.
    /// This does not change the identity of the original portable artifact.
    pub(crate) fn copy_file_exclusive(
        &mut self,
        source: &File,
        expected: &PrefixExpectedFile,
    ) -> Result<PrefixFileReceipt> {
        self.written_bytes
            .checked_add(expected.size)
            .filter(|total| *total <= self.limits.max_total_bytes)
            .context("copy output byte budget exceeded")?;
        self.verify_clone_bytes(source, expected)?;
        let source_stamp = stamp(source)?;
        let mut output = self.create_file_exclusive(&expected.relative, expected.mode)?;
        let mut offset = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        while offset < expected.size {
            output.prefix.check_write_ready()?;
            let count = usize::try_from((expected.size - offset).min(buffer.len() as u64))?;
            let read = source.read_at(&mut buffer[..count], offset)?;
            ensure!(read != 0, "projection source ended early");
            output.write_all(&buffer[..read])?;
            offset += read as u64;
        }
        verify_projected_output(&output, source, expected, &source_stamp)?;
        output.finish()
    }

    /// `None` means a recognized filesystem capability failure. An owned
    /// target may already exist: callers must discard the whole allocation
    /// before selecting a different recipe, rather than mixing mechanisms.
    pub(crate) fn try_reflink_file_exclusive(
        &mut self,
        source: &File,
        expected: &PrefixExpectedFile,
    ) -> Result<Option<PrefixFileReceipt>> {
        #[cfg(test)]
        if let Some(attempt) = REFLINK_ATTEMPT.with(|slot| slot.take()) {
            return self.try_reflink_with(source, expected, attempt);
        }
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        {
            use std::os::fd::AsRawFd;
            self.try_reflink_with(source, expected, |source, target| {
                // Linux _IOW(0x94, 9, int), identical on the supported native
                // x86_64/aarch64 ABIs. FICLONE consumes the original source FD.
                let result = unsafe {
                    nix::libc::ioctl(target.as_raw_fd(), 0x4004_9409 as _, source.as_raw_fd())
                };
                if result == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            })
        }
        #[cfg(not(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        {
            let _ = (source, expected);
            bail!("reflink projection requires a supported native Linux ABI")
        }
    }

    #[cfg(test)]
    pub(crate) fn install_reflink_test_attempt(
        attempt: Option<fn(&File, &File) -> io::Result<()>>,
    ) {
        REFLINK_ATTEMPT.with(|slot| slot.set(attempt));
    }

    #[cfg(any(
        test,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    fn try_reflink_with(
        &mut self,
        source: &File,
        expected: &PrefixExpectedFile,
        attempt: impl FnOnce(&File, &File) -> io::Result<()>,
    ) -> Result<Option<PrefixFileReceipt>> {
        self.verify_clone_bytes(source, expected)?;
        let source_stamp = stamp(source)?;
        let total = self
            .written_bytes
            .checked_add(expected.size)
            .filter(|total| *total <= self.limits.max_total_bytes)
            .context("reflink output byte budget exceeded")?;
        let mut output = self.create_file_exclusive(&expected.relative, expected.mode)?;
        if let Err(error) = attempt(source, &output.file) {
            // A source change must never be reclassified as an unsupported
            // filesystem and silently retried under another recipe.
            ensure!(
                stamp(source)? == source_stamp,
                "reflink source changed during its attempt"
            );
            output.prefix.verify_clone_bytes(source, expected)?;
            if reflink_unavailable(&error) {
                return Ok(None);
            }
            return Err(error.into());
        }
        output.size = expected.size;
        output.prefix.written_bytes = total;
        verify_projected_output(&output, source, expected, &source_stamp)?;
        Ok(Some(output.finish()?))
    }
}

#[cfg(any(
    test,
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
fn reflink_unavailable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP)
            | Some(nix::libc::ENOTTY)
            | Some(nix::libc::EXDEV)
            | Some(nix::libc::ENOSYS)
    )
}

fn verify_projected_output(
    output: &PrefixFile<'_>,
    source: &File,
    expected: &PrefixExpectedFile,
    source_stamp: &(u64, u64, u32, u32, u64, u64, i64, i64, i64, i64),
) -> Result<()> {
    fchmod(&output.file, Mode::from_bits_truncate(expected.mode as _))?;
    let name = expected
        .relative
        .file_name()
        .context("missing projection output name")?;
    let target = File::from(openat(
        &output.parent,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    ensure!(
        FileIdentity::of(&target)? == output.identity
            && output.identity != FileIdentity::of(source)?,
        "projection output aliases its source or was replaced"
    );
    output.prefix.verify_clone_bytes(&target, expected)?;
    ensure!(
        &stamp(source)? == source_stamp,
        "projection source changed during materialization"
    );
    output.prefix.check_namespace()
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> (
        tempfile::TempDir,
        File,
        PrefixExpectedFile,
        UnpublishedPrefix,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let path = root.join("source");
        std::fs::write(&path, b"fixed bytes").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let source = File::open(path).unwrap();
        let expected = PrefixExpectedFile {
            relative: "mapped".into(),
            size: 11,
            sha256: Sha256::digest(b"fixed bytes").into(),
            mode: 0o644,
        };
        let store = EnvironmentStore::open(
            &root.join("store"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let prefix = store.allocate().unwrap();
        (owner, source, expected, prefix)
    }

    #[test]
    fn p6_verified_copy_is_independent_and_preserves_the_source_offset() {
        let (_owner, mut source, expected, mut prefix) = fixture();
        source.seek(SeekFrom::Start(4)).unwrap();
        prefix.copy_file_exclusive(&source, &expected).unwrap();
        assert_eq!(source.stream_position().unwrap(), 4);
        let path = prefix.final_path().join("mapped");
        let metadata = std::fs::metadata(&path).unwrap();
        assert_ne!(
            (metadata.dev(), metadata.ino()),
            (
                source.metadata().unwrap().dev(),
                source.metadata().unwrap().ino()
            )
        );
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"fixed bytes");
        std::fs::write(&path, b"other bytes").unwrap();
        let mut original = [0; 11];
        source.read_exact_at(&mut original, 0).unwrap();
        assert_eq!(&original, b"fixed bytes");
    }

    #[test]
    fn p6_copy_rejects_hash_hardlinks_and_unowned_parent_paths() {
        let (owner, source, mut expected, mut prefix) = fixture();
        expected.sha256 = [0; 32];
        assert!(prefix.copy_file_exclusive(&source, &expected).is_err());
        assert!(!prefix.final_path().join("mapped").exists());
        expected.sha256 = Sha256::digest(b"fixed bytes").into();
        std::fs::hard_link(
            owner.path().join("source"),
            owner.path().join("source-alias"),
        )
        .unwrap();
        assert!(prefix.copy_file_exclusive(&source, &expected).is_err());
        std::fs::remove_file(owner.path().join("source-alias")).unwrap();
        symlink(owner.path(), prefix.final_path().join("outside")).unwrap();
        expected.relative = "outside/escape".into();
        assert!(prefix.copy_file_exclusive(&source, &expected).is_err());
        assert!(!owner.path().join("escape").exists());
    }

    #[test]
    fn p6_reflink_capability_failure_retains_only_owned_cleanup_authority() {
        let (owner, source, expected, mut prefix) = fixture();
        let failed = prefix.final_path().to_owned();
        assert!(
            prefix
                .try_reflink_with(&source, &expected, |_, _| Err(
                    io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP)
                ))
                .unwrap()
                .is_none()
        );
        assert!(failed.join("mapped").is_file());
        prefix.discard().unwrap();
        assert!(!failed.exists());
        assert_eq!(
            std::fs::read(owner.path().join("source")).unwrap(),
            b"fixed bytes"
        );
    }

    #[test]
    fn p6_reflink_io_permissions_space_and_changed_sources_never_request_fallback() {
        for code in [
            nix::libc::EIO,
            nix::libc::ENOSPC,
            nix::libc::EACCES,
            nix::libc::EPERM,
            nix::libc::EINVAL,
        ] {
            let (_owner, source, expected, mut prefix) = fixture();
            assert!(
                prefix
                    .try_reflink_with(&source, &expected, |_, _| Err(
                        io::Error::from_raw_os_error(code)
                    ))
                    .is_err()
            );
            prefix.discard().unwrap();
        }
        let (owner, source, expected, mut prefix) = fixture();
        assert!(
            prefix
                .try_reflink_with(&source, &expected, |_, _| {
                    std::fs::write(owner.path().join("source"), b"other bytes")?;
                    Err(io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP))
                })
                .is_err()
        );
        prefix.discard().unwrap();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn fixture() -> (
        tempfile::TempDir,
        File,
        PrefixExpectedFile,
        UnpublishedPrefix,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let source_path = root.join("source");
        std::fs::write(&source_path, b"fixed bytes").unwrap();
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let source = File::open(source_path).unwrap();
        let expected = PrefixExpectedFile {
            relative: "mapped".into(),
            sha256: Sha256::digest(b"fixed bytes").into(),
            size: 11,
            mode: 0o644,
        };
        let store = EnvironmentStore::open(
            &root.join("store"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        (owner, source, expected, store.allocate().unwrap())
    }
    #[test]
    fn f3_cow_clone_has_independent_inode_and_writes_do_not_pollute_source() {
        let (_owner, source, expected, mut prefix) = fixture();
        prefix.clone_file_exclusive(&source, &expected).unwrap();
        let mapped = prefix.final_path().join("mapped");
        let output = std::fs::metadata(&mapped).unwrap();
        assert_ne!(output.ino(), source.metadata().unwrap().ino());
        assert_eq!(output.nlink(), 1);
        std::fs::write(&mapped, b"other bytes").unwrap();
        let mut bytes = [0u8; 11];
        source.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"fixed bytes");
    }
    #[test]
    fn f3_cow_clone_rejects_wrong_hash_existing_leaf_and_symlink_parent() {
        let (owner, source, mut expected, mut prefix) = fixture();
        expected.sha256 = [0; 32];
        assert!(prefix.clone_file_exclusive(&source, &expected).is_err());
        assert!(!prefix.final_path().join("mapped").exists());
        expected.sha256 = Sha256::digest(b"fixed bytes").into();
        std::fs::write(prefix.final_path().join("mapped"), b"sentinel").unwrap();
        assert!(prefix.clone_file_exclusive(&source, &expected).is_err());
        assert_eq!(
            std::fs::read(prefix.final_path().join("mapped")).unwrap(),
            b"sentinel"
        );
        symlink(owner.path(), prefix.final_path().join("outside")).unwrap();
        expected.relative = "outside/escape".into();
        assert!(prefix.clone_file_exclusive(&source, &expected).is_err());
        assert!(!owner.path().join("escape").exists());
    }

    #[test]
    fn f3_cow_clone_records_ownership_and_discards_only_its_clone() {
        records_owned_projection(UnpublishedPrefix::clone_file_exclusive);
    }

    #[test]
    fn p6_verified_copy_records_ownership_and_discards_only_its_output() {
        records_owned_projection(UnpublishedPrefix::copy_file_exclusive);
    }

    fn records_owned_projection(
        project: fn(
            &mut UnpublishedPrefix,
            &File,
            &PrefixExpectedFile,
        ) -> Result<PrefixFileReceipt>,
    ) {
        use super::super::control::ownership::{
            OwnershipStreamLimits,
            events::{
                OwnershipEvent, OwnershipEventLimits, decode_ownership_event,
                validate_ownership_events,
            },
        };
        use crate::packages::python_environment::journal::{
            JournalLimits, JournalPrepare, JournalTarget, prepare_journal_metadata,
        };
        use crate::packages::python_environment::model::{
            InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
        };
        let (_owner, source, expected_file, mut prefix) = fixture();
        let store = EnvironmentStore::open(
            prefix.final_path().parent().unwrap(),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let generation = prefix.generation_identity().unwrap();
        let key = SelectionKey::new(
            uuid::Uuid::from_u128(1),
            "clone",
            [3; 32],
            ModelLimits::default(),
        )
        .unwrap();
        let expected = SelectionStamp::default();
        let inputs = [InputReference::new(
            InputKind::Runtime,
            pakro_store::Digest::Sha256([7; 32]),
        )];
        let journal = store
            .with_control(control::ControlLimits::default(), |control| {
                prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce: uuid::Uuid::from_u128(104),
                        key: &key,
                        expected: &expected,
                        target: JournalTarget::Generation {
                            identity: &generation,
                            graph: [4; 32],
                            inputs: &inputs,
                        },
                    },
                    JournalLimits::default(),
                )
            })
            .unwrap();
        store
            .with_control(control::ControlLimits::default(), |control| {
                control.attach_prefix_ownership(
                    &mut prefix,
                    &journal,
                    JournalLimits::default(),
                    OwnershipObserverLimits::default(),
                )
            })
            .unwrap();
        project(&mut prefix, &source, &expected_file).unwrap();
        let log = store
            .with_control(control::ControlLimits::default(), |control| {
                control.read_ownership_stream(
                    &journal,
                    JournalLimits::default(),
                    OwnershipStreamLimits::default(),
                )
            })
            .unwrap()
            .unwrap();
        let events: Vec<_> = log
            .records()
            .iter()
            .map(|record| {
                decode_ownership_event(record.payload(), OwnershipEventLimits::default()).unwrap()
            })
            .collect();
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[0], OwnershipEvent::CreateIntent { op: 1, relative, .. } if relative == b"mapped")
        );
        assert!(matches!(&events[1], OwnershipEvent::Created { op: 1, .. }));
        assert!(matches!(
            &events[2],
            OwnershipEvent::FileFinished {
                op: 1,
                size: 11,
                mode: 0o644
            }
        ));
        validate_ownership_events(
            &log,
            OwnershipEventLimits {
                prefix: prefix.limits,
                final_prefix_bytes: prefix.final_path().as_os_str().as_encoded_bytes().len(),
                ..OwnershipEventLimits::default()
            },
        )
        .unwrap();
        let mapped = prefix.final_path().join("mapped");
        prefix.discard().unwrap();
        assert!(!mapped.exists());
        let mut original = [0u8; 11];
        source.read_exact_at(&mut original, 0).unwrap();
        assert_eq!(&original, b"fixed bytes");
    }
}
