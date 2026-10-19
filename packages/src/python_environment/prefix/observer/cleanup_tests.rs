use super::tests::Fixture;
use super::*;
use crate::packages::python_environment::catalog::edit_catalog_metadata;
use crate::packages::python_environment::journal::{
    JournalPrepare, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    CatalogCodecLimits, GenerationRecord, ModelLimits, SelectionStamp,
};
use crate::packages::python_environment::prefix::control::ownership::{
    OwnershipLog,
    events::{
        OwnershipEvent, OwnershipRecoveryPhase, decode_ownership_event, validate_ownership_events,
    },
};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use uuid::Uuid;

fn attached(limits: OwnershipObserverLimits) -> Fixture {
    let mut fixture = Fixture::with_limits(PrefixLimits::default());
    fixture
        .attach(limits)
        .expect("actual attached-prefix prerequisite");
    assert!(read(&fixture).records().is_empty());
    fixture
}
fn finished(fixture: &mut Fixture) {
    fixture
        .prefix
        .create_dir_all(Path::new("lib/nested"))
        .unwrap();
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("lib/nested/module.py"), 0o644)
        .unwrap();
    file.write_all(b"known owned bytes").unwrap();
    file.finish().unwrap();
    let log = read(fixture);
    let state = validate_ownership_events(&log, event_limits(fixture)).unwrap();
    assert_eq!(state.entries().len(), 3);
    assert_eq!(state.phase(), OwnershipRecoveryPhase::Building);
    assert!(state.pending_create().is_none());
}
fn event_limits(fixture: &Fixture) -> OwnershipEventLimits {
    OwnershipEventLimits {
        prefix: fixture.prefix.limits,
        final_prefix_bytes: fixture
            .prefix
            .final_path()
            .as_os_str()
            .as_encoded_bytes()
            .len(),
        ..OwnershipEventLimits::default()
    }
}
fn read(fixture: &Fixture) -> OwnershipLog {
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.read_ownership_stream(
                &fixture.journal,
                JournalLimits::default(),
                OwnershipStreamLimits::default(),
            )
        })
        .expect("actual durable raw stream prerequisite")
        .unwrap()
}
fn journal_path(fixture: &Fixture) -> PathBuf {
    fixture
        .store
        .root_path()
        .join(".control/journals")
        .join(format!("{}.json", fixture.journal.nonce().simple()))
}
fn assert_complete(fixture: &Fixture) {
    let log = read(fixture);
    let state = validate_ownership_events(&log, event_limits(fixture)).unwrap();
    assert_eq!(
        state.phase(),
        OwnershipRecoveryPhase::Complete,
        "ordinary cleanup returned without durable capture/removal/completion"
    );
    assert!(state.entries().iter().all(|entry| entry.removed()));
    assert!(state.capture().unwrap().captured());
    assert!(state.root_removed_sequence().is_some());
}
fn assert_no_capture(fixture: &Fixture) {
    let log = read(fixture);
    assert!(log.records().iter().all(|record| !matches!(
        decode_ownership_event(record.payload(), event_limits(fixture)).unwrap(),
        OwnershipEvent::CaptureIntent { .. }
    )));
    assert_no_quarantine_entries(fixture);
}
fn assert_no_quarantine_entries(fixture: &Fixture) {
    match fs::read_dir(fixture.store.root_path().join(".quarantine")) {
        Ok(entries) => assert_eq!(entries.count(), 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("quarantine inspection failed: {error}"),
    }
}

#[test]
fn explicit_discard_durably_captures_removes_and_completes_without_acknowledging_metadata() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    let journal = fs::read(journal_path(&fixture)).unwrap();
    let lease = fixture
        .store
        .root_path()
        .join(".leases/build")
        .join(format!("{}.lock", fixture.prefix.allocation_id()));
    let lease_id = fs::metadata(&lease).unwrap().ino();
    fixture.prefix.discard().unwrap();
    assert!(!root.exists());
    assert_complete(&fixture);
    assert_eq!(fs::read(journal_path(&fixture)).unwrap(), journal);
    assert_eq!(
        fs::metadata(lease).unwrap().ino(),
        lease_id,
        "permanent lease was replaced"
    );
    assert!(
        !fixture
            .store
            .root_path()
            .join(".control/catalog/state.json")
            .exists()
    );
}

#[test]
fn ordinary_drop_uses_the_same_logged_cleanup_instead_of_an_unlogged_remover() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    let limits = event_limits(&fixture);
    drop(fixture.prefix);
    assert!(!root.exists());
    let log = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.read_ownership_stream(
                &fixture.journal,
                JournalLimits::default(),
                OwnershipStreamLimits::default(),
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        validate_ownership_events(&log, limits).unwrap().phase(),
        OwnershipRecoveryPhase::Complete,
        "Drop bypassed durable cleanup events"
    );
}

#[test]
fn empty_prefix_still_records_capture_and_real_root_removal_before_complete() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    fixture.prefix.discard().unwrap();
    let log = read(&fixture);
    assert_eq!(
        log.records().len(),
        4,
        "empty roots still require capture/root progress"
    );
    assert_complete(&fixture);
}

#[test]
fn known_created_unfinished_file_can_be_cleaned_without_fabricating_finished_stats() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("partial"), 0o644)
        .unwrap();
    file.write_all(b"partial output").unwrap();
    drop(file);
    assert!(fixture.prefix.check_binding().is_err());
    assert_eq!(read(&fixture).records().len(), 2);
    fixture.prefix.discard().unwrap();
    assert_complete(&fixture);
    assert!(read(&fixture).records().iter().all(|record| !matches!(
        decode_ownership_event(record.payload(), event_limits(&fixture)).unwrap(),
        OwnershipEvent::FileFinished { .. }
    )));
}

#[test]
fn untracked_entry_is_retained_before_any_capture_or_owned_unlink() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    fs::write(root.join("foreign"), b"external").unwrap();
    let error = fixture.prefix.discard().unwrap_err();
    assert!(format!("{error:?}").contains("untracked"), "{error:?}");
    assert_eq!(fs::read(root.join("foreign")).unwrap(), b"external");
    assert_eq!(
        fs::read(root.join("lib/nested/module.py")).unwrap(),
        b"known owned bytes"
    );
    assert_no_capture(&fixture);
}

#[test]
fn replaced_owned_inode_is_never_captured_or_deleted_as_the_original() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    let file = root.join("lib/nested/module.py");
    let original = file.with_extension("original");
    fs::rename(&file, &original).unwrap();
    fs::write(&file, b"replacement").unwrap();
    assert!(fixture.prefix.discard().is_err());
    assert_eq!(fs::read(&file).unwrap(), b"replacement");
    assert_eq!(fs::read(&original).unwrap(), b"known owned bytes");
    assert_no_capture(&fixture);
}

#[test]
fn broken_log_namespace_blocks_discard_and_later_drop_even_after_repair() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    fs::set_permissions(fixture.stream_path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "unlogged cleanup ignored its unavailable stream"
    );
    fs::set_permissions(fixture.stream_path(), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(root.join("lib/nested/module.py").exists());
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
    drop(fixture.prefix);
    assert!(root.join("lib/nested/module.py").exists());
}

#[test]
fn capture_event_quota_failure_happens_before_rename_or_delete() {
    let mut fixture = attached(OwnershipObserverLimits {
        stream: OwnershipStreamLimits {
            max_records: 3,
            ..OwnershipStreamLimits::default()
        },
        ..OwnershipObserverLimits::default()
    });
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("file"), 0o644)
        .unwrap();
    file.write_all(b"owned").unwrap();
    file.finish().unwrap();
    assert_eq!(read(&fixture).records().len(), 3);
    let root = fixture.prefix.final_path().to_path_buf();
    assert!(
        fixture.prefix.discard().is_err(),
        "capture skipped its durable quota gate"
    );
    assert_eq!(fs::read(root.join("file")).unwrap(), b"owned");
    assert_no_capture(&fixture);
}

fn catalog_protected_generation(active: bool) {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let generation = fixture.journal.generation().unwrap();
    let record = GenerationRecord::new(
        &generation.identity().allocation,
        *fixture.journal.key().target(),
        *generation.graph(),
        [9; 32],
        generation.inputs().to_vec(),
        ModelLimits::default(),
    )
    .unwrap();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |catalog| {
                catalog.register_generation(record)?;
                if active {
                    catalog.apply_selection(
                        fixture.journal.key(),
                        &SelectionStamp::default(),
                        Some(fixture.prefix.allocation_id()),
                        fixture.journal.nonce(),
                    )?;
                }
                Ok(())
            })
        })
        .unwrap();
    let root = fixture.prefix.final_path().to_path_buf();
    let before = fs::read(fixture.stream_path()).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "catalog-protected generation was removed"
    );
    assert_eq!(
        fs::read(root.join("lib/nested/module.py")).unwrap(),
        b"known owned bytes"
    );
    assert_eq!(fs::read(fixture.stream_path()).unwrap(), before);
    assert_no_capture(&fixture);
}
#[test]
fn active_catalog_generation_is_protected_from_unpublished_cleanup() {
    catalog_protected_generation(true);
}
#[test]
fn retired_catalog_generation_remains_protected_without_gc_authority() {
    catalog_protected_generation(false);
}

#[test]
fn another_pending_transaction_for_the_same_generation_blocks_cleanup() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let identity = fixture.prefix.generation_identity().unwrap();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: Uuid::from_u128(202),
                    key: fixture.journal.key(),
                    expected: &SelectionStamp::default(),
                    target: JournalTarget::Generation {
                        identity: &identity,
                        graph: [5; 32],
                        inputs: fixture.journal.generation().unwrap().inputs(),
                    },
                },
                JournalLimits::default(),
            )?;
            Ok(())
        })
        .unwrap();
    let root = fixture.prefix.final_path().to_path_buf();
    assert!(
        fixture.prefix.discard().is_err(),
        "another durable journal lost its generation"
    );
    assert!(root.join("lib/nested/module.py").exists());
    assert_no_capture(&fixture);
}

#[test]
fn corrupted_log_fails_closed_before_any_generation_namespace_change() {
    let mut fixture = attached(OwnershipObserverLimits::default());
    finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    let mut body = fs::read(fixture.stream_path()).unwrap();
    *body.last_mut().unwrap() ^= 1;
    fs::write(fixture.stream_path(), body).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "corrupted ownership evidence was ignored"
    );
    assert!(root.join("lib/nested/module.py").exists());
    assert_no_quarantine_entries(&fixture);
}
