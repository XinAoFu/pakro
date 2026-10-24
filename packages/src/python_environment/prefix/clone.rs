//! A clone is a new owned regular inode, never a hardlink or an ordinary-copy
//! fallback. It follows the same ownership log and cleanup rules as a writer.
use super::verify::PrefixExpectedFile;
use super::*;
use eyre::ContextCompat;
use sha2::{Digest as _, Sha256};
use std::os::unix::fs::FileExt;

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
        prefix
            .clone_file_exclusive(&source, &expected_file)
            .unwrap();
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
