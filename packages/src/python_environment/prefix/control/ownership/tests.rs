use super::*;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use pakro_store::Digest;
use uuid::Uuid;

use crate::packages::python_environment::journal::{
    JournalPrepare, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
    prefix: UnpublishedPrefix,
    journal: JournalRecord,
    header: Header,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::open(
            &root.path().canonicalize().unwrap().join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .expect("real store prerequisite");
        let prefix = store.allocate().expect("real prefix prerequisite");
        let identity = prefix
            .generation_identity()
            .expect("real generation identity prerequisite");
        let key = SelectionKey::new(
            Uuid::from_u128(1),
            "开发 main",
            [3; 32],
            ModelLimits::default(),
        )
        .unwrap();
        let expected = SelectionStamp::default();
        let refs = [InputReference::new(
            InputKind::Runtime,
            Digest::Sha256([7; 32]),
        )];
        let (journal, journal_sha256) = store
            .with_control(ControlLimits::default(), |control| {
                let journal = prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce: Uuid::from_u128(101),
                        key: &key,
                        expected: &expected,
                        target: JournalTarget::Generation {
                            identity: &identity,
                            graph: [4; 32],
                            inputs: &refs,
                        },
                    },
                    JournalLimits::default(),
                )?;
                let snapshot = control
                    .read(
                        ControlNamespace::Journals,
                        &format!("{}.json", journal.nonce().simple()),
                    )?
                    .unwrap();
                assert_eq!(*snapshot.sha256(), hash(snapshot.bytes()));
                assert!(
                    control
                        .list_names(ControlNamespace::Ownership, 10, 1024)?
                        .is_empty()
                );
                Ok((journal, *snapshot.sha256()))
            })
            .expect("actual journal persist + ownership namespace prerequisites must be GREEN");
        let mut header = Header {
            nonce: journal.nonce(),
            journal_sha256,
            identity: identity.facts().unwrap(),
            sha256: [0; 32],
        };
        let bytes = golden_header(&header);
        header.sha256 = bytes[bytes.len() - 32..].try_into().unwrap();
        Self {
            _root: root,
            store,
            prefix,
            journal,
            header,
        }
    }
    fn path(&self) -> PathBuf {
        self.directory().join(filename(self.journal.nonce()))
    }
    fn directory(&self) -> PathBuf {
        self.store.root_path().join(".control/ownership")
    }
    fn create(&self, limits: OwnershipStreamLimits) -> Result<OwnershipStream> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.create_ownership_stream(
                    &self.journal,
                    &self.prefix,
                    JournalLimits::default(),
                    limits,
                )
            })
    }
    fn read(&self, limits: OwnershipStreamLimits) -> Result<Option<OwnershipLog>> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.read_ownership_stream(&self.journal, JournalLimits::default(), limits)
            })
    }
    fn seed(&self, bytes: &[u8]) {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.replace(
                    ControlNamespace::Ownership,
                    &filename(self.journal.nonce()),
                    None,
                    bytes,
                )?;
                Ok(())
            })
            .expect("real independent control seed prerequisite");
        assert_eq!(fs::read(self.path()).unwrap(), bytes);
    }
    fn seeded_writer(&self, payloads: &[&[u8]], limits: OwnershipStreamLimits) -> OwnershipStream {
        let (bytes, head) = golden_log(&self.header, payloads);
        self.seed(&bytes);
        self.store
            .with_control(ControlLimits::default(), |control| {
                let parent = &control.namespaces[ControlNamespace::Ownership.index()];
                let file = File::from(openat(
                    parent,
                    filename(self.journal.nonce()).as_str(),
                    OFlag::O_WRONLY | OFlag::O_APPEND | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
                assert_eq!(file.metadata()?.mode() & 0o7777, 0o600);
                assert_eq!(file.metadata()?.nlink(), 1);
                let observed = file.metadata()?;
                // Independent test-only capability setup over actual FDs. This does
                // not use the wrong create implementation or manufacture a D seal.
                Ok(OwnershipStream {
                    binding: Some(BoundFiles {
                        store: control.store.root.try_clone()?,
                        control: control.root.try_clone()?,
                        parent: parent.try_clone()?,
                        file,
                        identity: Some(self.prefix.generation_identity()?),
                        uid: nix::unistd::geteuid().as_raw(),
                        observed,
                    }),
                    header: self.header.clone(),
                    limits,
                    next_sequence: payloads.len() as u64 + 1,
                    head,
                    total_bytes: bytes.len() as u64,
                    poisoned: false,
                })
            })
            .expect("actual pinned FD writer fixture prerequisite")
    }
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    match Digest::from_sha256_bytes(bytes) {
        Digest::Sha256(value) => value,
        Digest::Sha512(_) => unreachable!("fixed SHA256 constructor"),
    }
}

/// Independent fixed binary wire, without production framing helpers.
fn golden_header(header: &Header) -> Vec<u8> {
    let mut bytes = b"pakro-ownership-v1\0".to_vec();
    bytes.extend_from_slice(header.nonce.as_bytes());
    bytes.extend_from_slice(&header.journal_sha256);
    assert_eq!(header.identity.allocation.len(), 34);
    bytes.extend_from_slice(header.identity.allocation.as_bytes());
    for pair in [
        header.identity.store,
        header.identity.generation,
        header.identity.leases,
        header.identity.build,
        header.identity.lock,
    ] {
        for value in pair {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    bytes.extend_from_slice(&hash(&bytes));
    assert_eq!(bytes.len(), 213);
    bytes
}
fn golden_frame(sequence: u64, previous: [u8; 32], payload: &[u8]) -> (Vec<u8>, [u8; 32]) {
    let mut frame = sequence.to_be_bytes().to_vec();
    frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    frame.extend_from_slice(&previous);
    frame.extend_from_slice(&hash(payload));
    let mut signed = b"pakro-ownership-frame/v1\0".to_vec();
    signed.extend_from_slice(&frame);
    let digest = hash(&signed);
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&digest);
    (frame, digest)
}
fn golden_log(header: &Header, payloads: &[&[u8]]) -> (Vec<u8>, [u8; 32]) {
    let mut bytes = golden_header(header);
    let mut head = bytes[bytes.len() - 32..].try_into().unwrap();
    for (index, payload) in payloads.iter().enumerate() {
        let (frame, next) = golden_frame(index as u64 + 1, head, payload);
        bytes.extend_from_slice(&frame);
        head = next;
    }
    (bytes, head)
}
fn snapshot(path: &Path) -> (u64, u64, Vec<u8>) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino(), fs::read(path).unwrap())
}

#[test]
fn create_persists_complete_identity_bound_header_before_returning_append_capability() {
    let fixture = Fixture::new();
    let mut stream = fixture.create(OwnershipStreamLimits::default()).unwrap();
    let bytes =
        fs::read(fixture.path()).expect("stream create returned without a durable regular file");
    assert_eq!(bytes, golden_header(&fixture.header));
    let metadata = fs::symlink_metadata(fixture.path()).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(fixture.prefix.check_binding().is_ok());
    assert!(
        !fixture
            .store
            .root_path()
            .join(".control/catalog/state.json")
            .exists()
    );
    let receipt = stream
        .append(&fixture.prefix, b"first opaque intent")
        .unwrap();
    assert_eq!(receipt.sequence(), 1);
    assert_eq!(receipt.payload_bytes(), 19);
    assert!(fs::metadata(fixture.path()).unwrap().len() > bytes.len() as u64);
}

#[test]
fn missing_stream_read_does_not_create_a_file_or_change_the_journal() {
    let fixture = Fixture::new();
    let journal = fixture
        .store
        .root_path()
        .join(".control/journals")
        .join(format!("{}.json", fixture.journal.nonce().simple()));
    let before = snapshot(&journal);
    assert!(
        fixture
            .read(OwnershipStreamLimits::default())
            .unwrap()
            .is_none()
    );
    assert!(!fixture.path().exists());
    assert_eq!(snapshot(&journal), before);
    assert!(fixture.prefix.check_binding().is_ok());
}

#[test]
fn seeded_actual_writer_appends_synced_frames_and_reports_exact_receipts() {
    let fixture = Fixture::new();
    let mut stream = fixture.seeded_writer(&[], OwnershipStreamLimits::default());
    let first = stream.append(&fixture.prefix, b"first").unwrap();
    let (one, head) = golden_log(&fixture.header, &[b"first"]);
    assert_eq!(fs::read(fixture.path()).unwrap(), one);
    assert_eq!(first.sequence(), 1);
    assert_eq!(first.payload_bytes(), 5);
    assert_eq!(first.chain_sha256(), &head);
    assert_eq!(first.total_bytes(), one.len() as u64);
    let second = stream
        .append(&fixture.prefix, "创建\0opaque".as_bytes())
        .unwrap();
    let (both, head) = golden_log(&fixture.header, &[b"first", "创建\0opaque".as_bytes()]);
    assert_eq!(fs::read(fixture.path()).unwrap(), both);
    assert_eq!(second.sequence(), 2);
    assert_eq!(second.chain_sha256(), &head);
    assert_eq!(second.total_bytes(), both.len() as u64);
    assert!(!format!("{stream:?}").contains("opaque"));
}

#[test]
fn independent_binary_golden_is_read_fully_as_data_without_prefix_effects() {
    let fixture = Fixture::new();
    let payloads = [b"".as_slice(), b"opaque\0,\r\n\xff".as_slice()];
    let (bytes, head) = golden_log(&fixture.header, &payloads);
    fixture.seed(&bytes);
    let before = snapshot(&fixture.path());
    let log = fixture
        .read(OwnershipStreamLimits::default())
        .unwrap()
        .expect("valid durable stream omitted");
    assert_eq!(log.nonce(), fixture.journal.nonce());
    assert_eq!(log.journal_sha256(), &fixture.header.journal_sha256);
    assert_eq!(log.identity(), &fixture.header.identity);
    assert_eq!(log.head(), &head);
    assert_eq!(log.total_bytes(), bytes.len() as u64);
    assert_eq!(log.records().len(), 2);
    for (index, (record, payload)) in log.records().iter().zip(payloads).enumerate() {
        assert_eq!(record.sequence(), index as u64 + 1);
        assert_eq!(record.payload(), payload);
    }
    assert_eq!(log.records()[1].chain_sha256(), &head);
    assert!(!format!("{log:?}").contains("opaque"));
    assert_eq!(snapshot(&fixture.path()), before);
    assert!(fixture.prefix.check_binding().is_ok());
}

#[test]
fn create_rejects_existing_missing_clear_foreign_and_mismatching_durable_bindings() {
    let mut accepted = Vec::new();
    for label in [
        "existing",
        "missing",
        "clear",
        "foreign-journal",
        "wrong-prefix",
        "zero-payload",
        "zero-records",
        "zero-total",
    ] {
        let fixture = Fixture::new();
        let foreign = Fixture::new();
        if label == "existing" {
            fixture.seed(&golden_header(&fixture.header));
        }
        if label == "missing" {
            fixture
                .store
                .with_control(ControlLimits::default(), |control| {
                    let s = control
                        .read(
                            ControlNamespace::Journals,
                            &format!("{}.json", fixture.journal.nonce().simple()),
                        )?
                        .unwrap();
                    control.remove(&s)
                })
                .unwrap();
        }
        let key = SelectionKey::new(
            Uuid::from_u128(1),
            "开发 main",
            [3; 32],
            ModelLimits::default(),
        )
        .unwrap();
        let expected = SelectionStamp::default();
        let clear = if label == "clear" {
            Some(
                fixture
                    .store
                    .with_control(ControlLimits::default(), |control| {
                        prepare_journal_metadata(
                            control,
                            &JournalPrepare {
                                nonce: Uuid::from_u128(202),
                                key: &key,
                                expected: &expected,
                                target: JournalTarget::ClearSelection,
                            },
                            JournalLimits::default(),
                        )
                    })
                    .unwrap(),
            )
        } else {
            None
        };
        let journal = clear.as_ref().unwrap_or(if label == "foreign-journal" {
            &foreign.journal
        } else {
            &fixture.journal
        });
        let prefix = if label == "wrong-prefix" {
            &foreign.prefix
        } else {
            &fixture.prefix
        };
        let limits = match label {
            "zero-payload" => OwnershipStreamLimits {
                max_payload_bytes: 0,
                ..OwnershipStreamLimits::default()
            },
            "zero-records" => OwnershipStreamLimits {
                max_records: 0,
                ..OwnershipStreamLimits::default()
            },
            "zero-total" => OwnershipStreamLimits {
                max_total_bytes: 0,
                ..OwnershipStreamLimits::default()
            },
            _ => OwnershipStreamLimits::default(),
        };
        let before = fixture.path().exists().then(|| snapshot(&fixture.path()));
        let result = fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                control.create_ownership_stream(journal, prefix, JournalLimits::default(), limits)
            });
        if result.is_ok() {
            accepted.push(label);
        }
        if let Some(before) = before {
            assert_eq!(snapshot(&fixture.path()), before);
        } else {
            assert!(!fixture.path().exists());
        }
        assert!(fixture.prefix.check_binding().is_ok());
    }
    assert!(
        accepted.is_empty(),
        "invalid ownership create accepted: {accepted:?}"
    );
}

#[test]
fn append_payload_record_and_total_budgets_are_checked_before_file_growth() {
    let mut accepted = Vec::new();
    for label in ["payload", "records", "total"] {
        let fixture = Fixture::new();
        let limits = match label {
            "payload" => OwnershipStreamLimits {
                max_payload_bytes: 2,
                ..OwnershipStreamLimits::default()
            },
            "records" => OwnershipStreamLimits {
                max_records: 1,
                ..OwnershipStreamLimits::default()
            },
            _ => OwnershipStreamLimits {
                max_total_bytes: 213 + 112 + 2,
                ..OwnershipStreamLimits::default()
            },
        };
        let mut writer =
            fixture.seeded_writer(if label == "records" { &[b"first"] } else { &[] }, limits);
        let before = snapshot(&fixture.path());
        if writer.append(&fixture.prefix, b"abc").is_ok() {
            accepted.push(label);
        }
        assert_eq!(snapshot(&fixture.path()), before);
    }
    assert!(
        accepted.is_empty(),
        "append ignored pre-write resource budget: {accepted:?}"
    );
    let fixture = Fixture::new();
    let mut writer = fixture.seeded_writer(
        &[],
        OwnershipStreamLimits {
            max_payload_bytes: 3,
            max_records: 1,
            max_total_bytes: 213 + 112 + 3,
        },
    );
    let receipt = writer.append(&fixture.prefix, b"abc").unwrap();
    assert_eq!(receipt.total_bytes(), 328);
    assert_eq!(fs::metadata(fixture.path()).unwrap().len(), 328);
}

#[test]
fn reader_declared_payload_events_total_and_session_limits_apply_before_allocation() {
    let fixture = Fixture::new();
    let (bytes, _) = golden_log(&fixture.header, &[b"abc", b"de"]);
    fixture.seed(&bytes);
    let default = OwnershipStreamLimits::default();
    let mut accepted = Vec::new();
    for (label, limits) in [
        (
            "payload",
            OwnershipStreamLimits {
                max_payload_bytes: 2,
                ..default
            },
        ),
        (
            "events",
            OwnershipStreamLimits {
                max_records: 1,
                ..default
            },
        ),
        (
            "total",
            OwnershipStreamLimits {
                max_total_bytes: bytes.len() as u64 - 1,
                ..default
            },
        ),
        (
            "zero-payload",
            OwnershipStreamLimits {
                max_payload_bytes: 0,
                ..default
            },
        ),
        (
            "zero-events",
            OwnershipStreamLimits {
                max_records: 0,
                ..default
            },
        ),
        (
            "zero-total",
            OwnershipStreamLimits {
                max_total_bytes: 0,
                ..default
            },
        ),
    ] {
        if fixture.read(limits).is_ok() {
            accepted.push(label);
        }
    }
    // Keep this file larger than the durable journal. Otherwise the smaller
    // control session would fail journal setup before reaching stream bytes.
    let session_fixture = Fixture::new();
    let payload = [b'x'; 4096];
    let (session_bytes, _) = golden_log(&session_fixture.header, &[&payload]);
    session_fixture.seed(&session_bytes);
    let result = session_fixture.store.with_control(
        ControlLimits {
            max_file_bytes: session_bytes.len() as u64 - 1,
            ..ControlLimits::default()
        },
        |control| {
            let journal_name = format!("{}.json", session_fixture.journal.nonce().simple());
            assert!(
                control
                    .read(ControlNamespace::Journals, &journal_name)?
                    .is_some(),
                "journal must fit this session before testing stream max+1"
            );
            control.read_ownership_stream(
                &session_fixture.journal,
                JournalLimits::default(),
                default,
            )
        },
    );
    if result.is_ok() {
        accepted.push("session");
    }
    assert!(
        accepted.is_empty(),
        "ownership reader ignored budgets: {accepted:?}"
    );
    let log = fixture
        .read(OwnershipStreamLimits {
            max_payload_bytes: 3,
            max_records: 2,
            max_total_bytes: bytes.len() as u64,
        })
        .unwrap()
        .unwrap();
    assert_eq!(log.records().len(), 2);
    assert_eq!(log.total_bytes(), bytes.len() as u64);
}

#[test]
fn corrupt_header_chain_payload_sequence_and_truncation_are_not_missing_or_repaired() {
    let mut accepted = Vec::new();
    for label in [
        "magic",
        "version",
        "header-sha",
        "journal-sha",
        "nonce",
        "identity",
        "sequence",
        "previous",
        "payload-sha",
        "chain-sha",
        "duplicate",
        "truncated-header",
        "truncated-frame",
        "trailing",
        "declared-length",
    ] {
        let fixture = Fixture::new();
        let (mut bytes, _) = golden_log(&fixture.header, &[b"opaque"]);
        match label {
            "magic" => bytes[0] ^= 1,
            "version" => bytes[17] = b'2',
            "header-sha" => bytes[212] ^= 1,
            "journal-sha" | "nonce" | "identity" => {
                bytes.truncate(213);
                let index = match label {
                    "journal-sha" => 35,
                    "nonce" => 19,
                    _ => 101,
                };
                bytes[index] ^= 1;
                let digest = hash(&bytes[..181]);
                bytes[181..213].copy_from_slice(&digest);
            }
            "sequence" => {
                bytes[213..221].copy_from_slice(&2u64.to_be_bytes());
                rehash_frame(&mut bytes);
            }
            "previous" => {
                bytes[229] ^= 1;
                rehash_frame(&mut bytes);
            }
            "payload-sha" => {
                bytes[261] ^= 1;
                rehash_frame(&mut bytes);
            }
            "chain-sha" => {
                *bytes.last_mut().unwrap() ^= 1;
            }
            "duplicate" => {
                let repeat = bytes[213..].to_vec();
                bytes.extend_from_slice(&repeat);
            }
            "truncated-header" => bytes.truncate(212),
            "truncated-frame" => {
                bytes.pop();
            }
            "trailing" => bytes.push(0),
            "declared-length" => bytes[221..229].copy_from_slice(&u64::MAX.to_be_bytes()),
            _ => unreachable!(),
        }
        fixture.seed(&bytes);
        let before = snapshot(&fixture.path());
        match fixture.read(OwnershipStreamLimits::default()) {
            Ok(_) => accepted.push(label),
            Err(error) => {
                if label == "declared-length" {
                    assert!(
                        error.to_string().contains("limit"),
                        "oversized declared payload reached allocation/read: {error:?}"
                    );
                }
            }
        }
        assert_eq!(snapshot(&fixture.path()), before);
    }
    assert!(
        accepted.is_empty(),
        "corrupt ownership frames accepted or hidden: {accepted:?}"
    );
}
fn rehash_frame(bytes: &mut [u8]) {
    let mut signed = b"pakro-ownership-frame/v1\0".to_vec();
    signed.extend_from_slice(&bytes[213..293]);
    let digest = hash(&signed);
    let end = bytes.len();
    bytes[end - 32..].copy_from_slice(&digest);
}

#[test]
fn unsafe_stream_leaves_fail_without_following_symlinks_or_blocking_fifo() {
    let mut accepted = Vec::new();
    for label in ["symlink", "fifo", "hardlink", "mode"] {
        let fixture = Fixture::new();
        let bytes = golden_header(&fixture.header);
        let path = fixture.path();
        match label {
            "symlink" => {
                let outside = fixture._root.path().join("outside");
                fs::write(&outside, &bytes).unwrap();
                fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
                symlink(outside, &path).unwrap();
            }
            "fifo" => nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap(),
            "hardlink" => {
                fixture.seed(&bytes);
                fs::hard_link(&path, fixture._root.path().join("outside-link")).unwrap();
            }
            "mode" => {
                fixture.seed(&bytes);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => unreachable!(),
        }
        let (send, receive) = mpsc::channel();
        let worker = thread::spawn(move || {
            send.send(fixture.read(OwnershipStreamLimits::default()).is_err())
                .unwrap();
        });
        let result = receive.recv_timeout(Duration::from_secs(1));
        if result.is_err() {
            let _release = fs::OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(&path);
            let _ = receive.recv_timeout(Duration::from_secs(1));
            panic!("ownership reader blocked on {label}");
        }
        worker.join().unwrap();
        if !result.unwrap() {
            accepted.push(label);
        }
    }
    assert!(
        accepted.is_empty(),
        "unsafe stream leaf accepted: {accepted:?}"
    );
}

#[test]
fn writer_namespace_or_leaf_replacement_poisons_even_after_original_binding_is_restored() {
    let mut accepted = Vec::new();
    for label in ["leaf", "parent", "length"] {
        let fixture = Fixture::new();
        let mut writer = fixture.seeded_writer(&[], OwnershipStreamLimits::default());
        let original = fs::read(fixture.path()).unwrap();
        match label {
            "leaf" => {
                fs::rename(fixture.path(), fixture.directory().join("saved.stream")).unwrap();
                fs::write(fixture.path(), &original).unwrap();
                fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o600)).unwrap();
            }
            "parent" => {
                fs::rename(
                    fixture.directory(),
                    fixture.directory().with_extension("saved"),
                )
                .unwrap();
                fs::create_dir(fixture.directory()).unwrap();
                fs::set_permissions(fixture.directory(), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            "length" => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(fixture.path())
                    .unwrap()
                    .write_all(b"changed")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        if writer.append(&fixture.prefix, b"must reject").is_ok() {
            accepted.push(label);
        }
        match label {
            "leaf" => {
                fs::remove_file(fixture.path()).unwrap();
                fs::rename(fixture.directory().join("saved.stream"), fixture.path()).unwrap();
            }
            "parent" => {
                fs::remove_dir(fixture.directory()).unwrap();
                fs::rename(
                    fixture.directory().with_extension("saved"),
                    fixture.directory(),
                )
                .unwrap();
            }
            "length" => fs::OpenOptions::new()
                .write(true)
                .open(fixture.path())
                .unwrap()
                .set_len(original.len() as u64)
                .unwrap(),
            _ => unreachable!(),
        }
        if writer.append(&fixture.prefix, b"still reject").is_ok() {
            accepted.push(label);
        }
        assert!(
            writer.poisoned,
            "ownership writer recovered success authority after binding error"
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), original);
    }
    assert!(
        accepted.is_empty(),
        "writer binding violations accepted: {accepted:?}"
    );
}

#[test]
fn actual_readonly_regular_fd_write_failure_permanently_poisons_the_writer() {
    let fixture = Fixture::new();
    let mut writer = fixture.seeded_writer(&[], OwnershipStreamLimits::default());
    let before = snapshot(&fixture.path());
    let binding = writer.binding.as_mut().unwrap();
    binding.file = File::from(
        openat(
            &binding.parent,
            filename(fixture.journal.nonce()).as_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .unwrap(),
    );
    let error = writer
        .append(&fixture.prefix, b"real kernel write must fail")
        .unwrap_err();
    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
        "actual FD write was not reached: {error:?}"
    );
    assert!(writer.poisoned);
    assert_eq!(snapshot(&fixture.path()), before);
    assert!(writer.append(&fixture.prefix, b"no recovery").is_err());
    assert_eq!(snapshot(&fixture.path()), before);
}

#[test]
fn append_requires_the_same_ready_prefix_even_when_old_identity_pins_remain_open() {
    let mut accepted = Vec::new();
    for label in ["wrong", "tainted", "discarded"] {
        let mut fixture = Fixture::new();
        let foreign = Fixture::new();
        let mut writer = fixture.seeded_writer(&[], OwnershipStreamLimits::default());
        let before = snapshot(&fixture.path());
        if label == "tainted" {
            fixture.prefix.mark_failed();
        }
        if label == "discarded" {
            fixture.prefix.discard().unwrap();
        }
        let prefix = if label == "wrong" {
            &foreign.prefix
        } else {
            &fixture.prefix
        };
        if writer.append(prefix, b"not authorized").is_ok() {
            accepted.push(label);
        }
        assert_eq!(snapshot(&fixture.path()), before);
    }
    assert!(
        accepted.is_empty(),
        "append ignored explicit ready-prefix binding: {accepted:?}"
    );
}

#[test]
fn next_sequence_reports_the_next_real_frame_and_refuses_poisoned_streams() {
    let fixture = Fixture::new();
    let mut stream = fixture.create(OwnershipStreamLimits::default()).unwrap();
    assert_eq!(stream.next_sequence().unwrap(), 1);
    stream
        .append(&fixture.prefix, b"actual first frame")
        .unwrap();
    assert_eq!(stream.next_sequence().unwrap(), 2);
    let foreign = Fixture::new();
    assert!(stream.append(&foreign.prefix, b"wrong prefix").is_err());
    let error = stream.next_sequence().unwrap_err();
    assert!(error.to_string().contains("poison"));
}
