use super::tests::Fixture;
use super::*;
use crate::packages::python_environment::prefix::control::ownership::{
    OwnershipLog,
    events::{
        OwnershipEvent, OwnershipEventLimits, decode_ownership_event, encode_ownership_event,
        validate_ownership_events,
    },
};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn attached(limits: OwnershipObserverLimits, prefix: PrefixLimits) -> Fixture {
    let mut fixture = Fixture::with_limits(prefix);
    fixture.attach(limits).expect("actual attach8 prerequisite");
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Attached(_)
    ));
    assert!(read(&fixture.store, &fixture.journal).records().is_empty());
    fixture
}
fn read(store: &EnvironmentStore, journal: &JournalRecord) -> OwnershipLog {
    store
        .with_control(ControlLimits::default(), |control| {
            control.read_ownership_stream(
                journal,
                JournalLimits::default(),
                OwnershipStreamLimits::default(),
            )
        })
        .expect("actual raw framing prerequisite")
        .unwrap()
}
fn events(log: &OwnershipLog) -> Vec<OwnershipEvent> {
    log.records()
        .iter()
        .map(|record| {
            decode_ownership_event(record.payload(), OwnershipEventLimits::default())
                .expect("actual typed codec prerequisite")
        })
        .collect()
}
fn identity(path: &Path) -> [u64; 2] {
    let metadata = fs::symlink_metadata(path).unwrap();
    [metadata.dev(), metadata.ino()]
}

#[test]
fn nested_directories_record_each_intent_and_actual_parent_before_descendant_creation() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    fixture.prefix.create_dir_all(Path::new("a/b")).unwrap();
    let observed = events(&read(&fixture.store, &fixture.journal));
    assert_eq!(
        observed.len(),
        4,
        "actual mkdir returned without durable ownership events"
    );
    assert_eq!(
        observed,
        vec![
            OwnershipEvent::CreateIntent {
                op: 1,
                kind: control::ownership::events::OwnershipEntryKind::Directory,
                relative: b"a".to_vec(),
                parent: identity(fixture.prefix.final_path())
            },
            OwnershipEvent::Created {
                op: 1,
                identity: identity(&fixture.prefix.final_path().join("a"))
            },
            OwnershipEvent::CreateIntent {
                op: 3,
                kind: control::ownership::events::OwnershipEntryKind::Directory,
                relative: b"a/b".to_vec(),
                parent: identity(&fixture.prefix.final_path().join("a"))
            },
            OwnershipEvent::Created {
                op: 3,
                identity: identity(&fixture.prefix.final_path().join("a/b"))
            },
        ]
    );
}

#[test]
fn exclusive_file_writer_is_not_exposed_until_created_identity_is_durable() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let path = fixture.prefix.final_path().join("output");
    let writer = fixture
        .prefix
        .create_file_exclusive(Path::new("output"), 0o755)
        .unwrap();
    let observed = events(&read(&fixture.store, &fixture.journal));
    assert_eq!(
        observed.len(),
        2,
        "writer escaped before CreateIntent/Created durability"
    );
    assert!(
        matches!(&observed[0],OwnershipEvent::CreateIntent{op:1,relative,..} if relative==b"output")
    );
    assert_eq!(
        observed[1],
        OwnershipEvent::Created {
            op: 1,
            identity: identity(&path)
        }
    );
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    drop(writer);
}

#[test]
fn finished_file_records_actual_size_mode_and_metadata_frame_receipt() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let mut writer = fixture
        .prefix
        .create_file_exclusive(Path::new("command"), 0o755)
        .unwrap();
    writer.write_all(b"actual bytes").unwrap();
    let receipt = writer.finish().unwrap();
    let log = read(&fixture.store, &fixture.journal);
    let observed = events(&log);
    assert_eq!(observed.len(), 3);
    assert_eq!(
        observed[2],
        OwnershipEvent::FileFinished {
            op: 1,
            size: 12,
            mode: 0o755
        }
    );
    assert_eq!(receipt.size(), 12);
    assert_eq!(receipt.mode(), 0o755);
    let state = validate_ownership_events(
        &log,
        OwnershipEventLimits {
            prefix: fixture.prefix.limits,
            final_prefix_bytes: fixture
                .prefix
                .final_path()
                .as_os_str()
                .as_encoded_bytes()
                .len(),
            ..OwnershipEventLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        state.entries()[0].finished().unwrap().frame_sha256(),
        log.records()[2].chain_sha256()
    );
    assert_eq!(
        fs::metadata(fixture.prefix.final_path().join("command"))
            .unwrap()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn already_owned_directory_and_trailing_separator_are_true_event_noops() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    fixture.prefix.create_dir_all(Path::new("owned")).unwrap();
    assert_eq!(read(&fixture.store, &fixture.journal).records().len(), 2);
    let before = fs::read(fixture.stream_path()).unwrap();
    fixture.prefix.create_dir_all(Path::new("owned/")).unwrap();
    fixture.prefix.create_dir_all(Path::new("owned")).unwrap();
    assert_eq!(fs::read(fixture.stream_path()).unwrap(), before);
}

#[test]
fn original_file_path_bytes_survive_unicode_non_utf8_and_separator_aliases() {
    let mut paths: Vec<&[u8]> = vec!["lib/模块.py".as_bytes(), b"lib//aliased"];
    // Some macOS volumes reject non-UTF-8 names at the kernel boundary. Check
    // that actual prerequisite outside an attached prefix: this must not be
    // mistaken for an ownership-codec failure or an accepted renamed path.
    let probe = tempfile::tempdir().unwrap();
    match fs::write(probe.path().join(std::ffi::OsStr::from_bytes(b"\xff")), b"") {
        Ok(()) => paths.push(b"lib/\xff"),
        Err(error) if error.raw_os_error() == Some(nix::libc::EILSEQ) => {
            let event = OwnershipEvent::CreateIntent {
                op: 1,
                kind: control::ownership::events::OwnershipEntryKind::File,
                relative: b"lib/\xff".to_vec(),
                parent: [1, 2],
            };
            let limits = OwnershipEventLimits::default();
            let encoded = encode_ownership_event(&event, limits).unwrap();
            assert_eq!(decode_ownership_event(&encoded, limits).unwrap(), event);
        }
        Err(error) => panic!("non-UTF-8 filesystem prerequisite failed: {error}"),
    }
    for raw in paths {
        let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
        fixture.prefix.create_dir_all(Path::new("lib")).unwrap();
        let path = Path::new(std::ffi::OsStr::from_bytes(raw));
        let mut writer = fixture.prefix.create_file_exclusive(path, 0o644).unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
        let observed = events(&read(&fixture.store, &fixture.journal));
        assert!(observed.iter().any(|event|matches!(event,OwnershipEvent::CreateIntent{kind:control::ownership::events::OwnershipEntryKind::File,relative,..} if relative==raw)),"original Unix path bytes were not recorded");
    }
}

#[test]
fn prefix_entry_quota_rejection_emits_no_speculative_create_intent() {
    let mut fixture = attached(
        OwnershipObserverLimits::default(),
        PrefixLimits {
            max_entries: 1,
            ..PrefixLimits::default()
        },
    );
    fixture.prefix.create_dir_all(Path::new("one")).unwrap();
    assert_eq!(read(&fixture.store, &fixture.journal).records().len(), 2);
    let before = fs::read(fixture.stream_path()).unwrap();
    assert!(fixture.prefix.create_dir_all(Path::new("two")).is_err());
    assert!(!fixture.prefix.final_path().join("two").exists());
    assert_eq!(fs::read(fixture.stream_path()).unwrap(), before);
}

#[test]
fn typed_path_budget_fails_before_any_directory_or_intent_is_created() {
    let mut fixture = attached(
        OwnershipObserverLimits {
            max_total_path_bytes: 1,
            ..OwnershipObserverLimits::default()
        },
        PrefixLimits::default(),
    );
    assert!(
        fixture
            .prefix
            .create_dir_all(Path::new("too-long"))
            .is_err(),
        "typed logging budget was bypassed"
    );
    assert!(!fixture.prefix.final_path().join("too-long").exists());
    assert!(read(&fixture.store, &fixture.journal).records().is_empty());
    assert!(fixture.prefix.check_binding().is_err());
}

#[test]
fn created_logging_quota_failure_retains_the_real_unacknowledged_inode() {
    let mut fixture = attached(
        OwnershipObserverLimits {
            stream: OwnershipStreamLimits {
                max_records: 1,
                ..OwnershipStreamLimits::default()
            },
            ..OwnershipObserverLimits::default()
        },
        PrefixLimits::default(),
    );
    let result = fixture
        .prefix
        .create_file_exclusive(Path::new("partial"), 0o644);
    assert!(result.is_err(), "Created escaped the raw record quota");
    drop(result);
    let log = read(&fixture.store, &fixture.journal);
    let observed = events(&log);
    assert_eq!(observed.len(), 1);
    assert!(matches!(
        &observed[0],
        OwnershipEvent::CreateIntent { op: 1, .. }
    ));
    assert!(fixture.prefix.final_path().join("partial").is_file());
    assert!(fixture.prefix.check_binding().is_err());
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
    let path = fixture.prefix.final_path().to_owned();
    drop(fixture.prefix);
    assert!(
        path.join("partial").is_file(),
        "Drop erased an intent-only namespace"
    );
}

#[test]
fn log_namespace_failure_prevents_creation_and_remains_failed_after_permissions_restore() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let log_path = fixture.stream_path();
    fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        fixture.prefix.create_dir_all(Path::new("never")).is_err(),
        "creation ignored its unsafe log"
    );
    assert!(!fixture.prefix.final_path().join("never").exists());
    fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(fixture.prefix.check_binding().is_err());
    assert!(
        fixture
            .prefix
            .create_dir_all(Path::new("still-never"))
            .is_err()
    );
    assert!(read(&fixture.store, &fixture.journal).records().is_empty());
}

#[test]
fn real_output_write_error_keeps_created_identity_without_false_file_finished() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let path = fixture.prefix.final_path().join("output");
    let mut writer = fixture
        .prefix
        .create_file_exclusive(Path::new("output"), 0o644)
        .unwrap();
    assert_eq!(read(&fixture.store, &fixture.journal).records().len(), 2);
    writer.file = File::open(&path).unwrap();
    let error = writer.write_all(b"actual EBADF").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
    drop(writer);
    assert!(fixture.prefix.check_binding().is_err());
    let log = read(&fixture.store, &fixture.journal);
    assert_eq!(log.records().len(), 2);
    let state = validate_ownership_events(&log, OwnershipEventLimits::default()).unwrap();
    assert!(!state.requires_retention());
    assert!(state.entries()[0].finished().is_none());
}

#[test]
fn first_and_last_file_operations_use_the_single_stream_sequence() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    for index in 0..3 {
        let name = format!("file-{index}");
        let mut writer = fixture
            .prefix
            .create_file_exclusive(Path::new(&name), 0o644)
            .unwrap();
        writer.write_all(b"x").unwrap();
        writer.finish().unwrap();
    }
    let observed = events(&read(&fixture.store, &fixture.journal));
    assert_eq!(observed.len(), 9);
    let ops = observed
        .iter()
        .filter_map(|event| {
            if let OwnershipEvent::CreateIntent { op, .. } = event {
                Some(*op)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(ops, vec![1, 4, 7]);
}

#[test]
fn physical_file_finish_does_not_succeed_when_its_finish_log_cannot_be_persisted() {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let path = fixture.prefix.final_path().join("output");
    let log_path = fixture.stream_path();
    let mut writer = fixture
        .prefix
        .create_file_exclusive(Path::new("output"), 0o755)
        .unwrap();
    assert_eq!(read(&fixture.store, &fixture.journal).records().len(), 2);
    writer.write_all(b"bytes").unwrap();
    fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644)).unwrap();
    let result = writer.finish();
    assert!(
        result.is_err(),
        "finish ignored metadata durability failure"
    );
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
    fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read(&fixture.store, &fixture.journal).records().len(), 2);
    assert!(fixture.prefix.check_binding().is_err());
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
}

fn intent_then_real_exclusive_create_failure(kind: EntryKind) {
    let mut fixture = attached(OwnershipObserverLimits::default(), PrefixLimits::default());
    let root = fixture.prefix.final_path().to_path_buf();
    let foreign = root.join("occupied");
    match kind {
        EntryKind::File => fs::write(&foreign, b"foreign bytes").unwrap(),
        EntryKind::Directory => fs::create_dir(&foreign).unwrap(),
        EntryKind::Symlink => unreachable!("journaled prefixes do not create layout links"),
    }
    let original = identity(&foreign);
    let error = match kind {
        EntryKind::File => fixture
            .prefix
            .create_file_exclusive(Path::new("occupied"), 0o644)
            .err()
            .expect("actual exclusive create must fail on the foreign file"),
        EntryKind::Directory => fixture
            .prefix
            .create_dir_all(Path::new("occupied"))
            .unwrap_err(),
        EntryKind::Symlink => unreachable!("journaled prefixes do not create layout links"),
    };
    assert!(format!("{error:?}").contains("EEXIST"), "{error:?}");
    assert_eq!(identity(&foreign), original);
    let observed = events(&read(&fixture.store, &fixture.journal));
    assert_eq!(observed.len(), 1, "only the durable intent may be recorded");
    assert!(
        matches!(&observed[0],OwnershipEvent::CreateIntent{op:1,relative,..} if relative==b"occupied")
    );
    match kind {
        EntryKind::File => fs::remove_file(&foreign).unwrap(),
        EntryKind::Directory => fs::remove_dir(&foreign).unwrap(),
        EntryKind::Symlink => unreachable!("journaled prefixes do not create layout links"),
    }
    assert!(
        fixture.prefix.check_binding().is_err(),
        "restoring the namespace must not undo an uncertain creation"
    );
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
    drop(fixture.prefix);
    assert!(root.exists(), "intent-only recovery evidence was deleted");
}

#[test]
fn durable_file_intent_then_real_eexist_is_irreversibly_retained() {
    intent_then_real_exclusive_create_failure(EntryKind::File);
}

#[test]
fn durable_directory_intent_then_real_eexist_is_irreversibly_retained() {
    intent_then_real_exclusive_create_failure(EntryKind::Directory);
}
