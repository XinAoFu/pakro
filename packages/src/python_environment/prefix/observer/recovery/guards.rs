use super::*;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn stream_path(fixture: &Restart) -> PathBuf {
    fixture
        .store
        .root_path()
        .join(".control/ownership")
        .join(format!("{}.stream", fixture.journal.nonce().simple()))
}
fn original_inventory_remains(fixture: &Restart) {
    let generation = fixture.journal.generation().unwrap();
    let root = fixture
        .store
        .root_path()
        .join(&generation.identity().allocation);
    assert_eq!(
        fs::read(root.join("lib/file")).unwrap(),
        b"restart owned bytes"
    );
    assert_eq!(
        fs::read(
            fixture
                .store
                .root_path()
                .join(".control/journals")
                .join(format!("{}.json", fixture.journal.nonce().simple()))
        )
        .unwrap(),
        fixture.original_journal
    );
    match fs::read_dir(fixture.store.root_path().join(".quarantine")) {
        Ok(entries) => assert_eq!(entries.count(), 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("quarantine inspection failed: {error}"),
    }
}

#[test]
fn same_bytes_leaf_replacement_after_read_is_rejected_by_the_original_pinned_inode() {
    let fixture = Restart::child("finished");
    let stream = stream_path(&fixture);
    let before = fs::read(&stream).unwrap();
    let original = fs::metadata(&stream).unwrap().ino();
    let replacement = stream.with_extension("replacement");
    let mut reached = false;
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.recover_with_checkpoint(
                &fixture.journal,
                OwnershipRecoveryLimits::default(),
                || {
                    reached = true;
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&replacement)?;
                    file.write_all(&before)?;
                    file.sync_all()?;
                    fs::rename(&replacement, &stream)?;
                    Ok(())
                },
            )
        })
        .unwrap_err();
    assert!(
        reached,
        "read/typed/EX/witness/inventory preflights did not reach resume"
    );
    assert_ne!(fs::metadata(&stream).unwrap().ino(), original);
    assert_eq!(fs::read(&stream).unwrap(), before);
    // Replacing the leaf unlinks the still-pinned old inode. Its nlink0 is
    // correctly rejected before the later explicit observation comparison.
    assert!(format!("{error:?}").contains("links"), "{error:?}");
    original_inventory_remains(&fixture);
}

#[test]
fn original_inode_content_change_after_read_is_rejected_as_changed_observation() {
    let fixture = Restart::child("finished");
    let stream = stream_path(&fixture);
    let mut changed = fs::read(&stream).unwrap();
    let original = fs::metadata(&stream).unwrap().ino();
    *changed.last_mut().unwrap() ^= 1;
    let mut reached = false;
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.recover_with_checkpoint(
                &fixture.journal,
                OwnershipRecoveryLimits::default(),
                || {
                    reached = true;
                    fs::write(&stream, &changed)?;
                    Ok(())
                },
            )
        })
        .unwrap_err();
    assert!(
        reached,
        "read/typed/EX/witness/inventory preflights did not reach resume"
    );
    assert_eq!(fs::metadata(&stream).unwrap().ino(), original);
    assert_eq!(fs::read(&stream).unwrap(), changed);
    assert!(
        format!("{error:?}").contains("read observation changed"),
        "{error:?}"
    );
    original_inventory_remains(&fixture);
}

#[test]
fn missing_final_root_without_capture_progress_is_not_treated_as_cleaned() {
    let fixture = Restart::child("finished");
    let outside = tempfile::tempdir().unwrap();
    let root = fixture
        .store
        .root_path()
        .join(&fixture.journal.generation().unwrap().identity().allocation);
    let retained = outside.path().join("original");
    fs::rename(&root, &retained).unwrap();
    let error = fixture.recover().unwrap_err();
    assert!(
        format!("{error:?}").contains("missing root without durable capture"),
        "{error:?}"
    );
    assert_eq!(
        fs::read(retained.join("lib/file")).unwrap(),
        b"restart owned bytes"
    );
}

#[test]
fn missing_child_without_remove_intent_is_rejected_before_capture() {
    let fixture = Restart::child("finished");
    let root = fixture
        .store
        .root_path()
        .join(&fixture.journal.generation().unwrap().identity().allocation);
    fs::remove_file(root.join("lib/file")).unwrap();
    let error = fixture.recover().unwrap_err();
    assert!(
        format!("{error:?}").contains("missing without durable remove intent"),
        "{error:?}"
    );
    assert!(root.join("lib").is_dir());
    assert_eq!(fixture.state().phase(), OwnershipRecoveryPhase::Building);
}

#[test]
fn pending_remove_does_not_adopt_a_new_inode_at_the_same_relative_name() {
    let fixture = Restart::child("unlinked");
    let capture = fixture.state();
    let name = std::str::from_utf8(capture.capture().unwrap().quarantine()).unwrap();
    let root = fixture.store.root_path().join(".quarantine").join(name);
    fs::write(root.join("lib/file"), b"foreign replacement").unwrap();
    let error = fixture.recover().unwrap_err();
    assert!(format!("{error:?}").contains("identity"), "{error:?}");
    assert_eq!(
        fs::read(root.join("lib/file")).unwrap(),
        b"foreign replacement"
    );
    assert!(fixture.state().pending_remove().is_some());
}

#[test]
fn a_durably_removed_child_cannot_reappear_even_while_its_parent_removal_is_pending() {
    let fixture = Restart::child("removed");
    let state = fixture.state();
    assert_eq!(state.pending_remove().unwrap().relative(), b"lib");
    assert!(
        state
            .entries()
            .iter()
            .find(|entry| entry.kind() == OwnershipEntryKind::File)
            .unwrap()
            .removed()
    );
    let root = fixture
        .store
        .root_path()
        .join(".quarantine")
        .join(std::str::from_utf8(state.capture().unwrap().quarantine()).unwrap());
    fs::write(root.join("lib/file"), b"recreated foreign child").unwrap();
    let error = fixture.recover().unwrap_err();
    assert!(
        format!("{error:?}").contains("already removed ownership path was recreated"),
        "{error:?}"
    );
    assert_eq!(
        fs::read(root.join("lib/file")).unwrap(),
        b"recreated foreign child"
    );
}

#[test]
fn proven_root_ready_can_complete_when_its_empty_quarantine_parent_is_also_missing() {
    let fixture = Restart::child("root-unlinked");
    fs::remove_dir(fixture.store.root_path().join(".quarantine")).unwrap();
    fixture.assert_cleaned(fixture.recover().unwrap());
    assert!(!fixture.store.root_path().join(".quarantine").exists());
}

#[test]
fn replaced_final_or_captured_root_never_becomes_the_recorded_cleanup_capability() {
    for phase in ["finished", "captured"] {
        let fixture = Restart::child(phase);
        let outside = tempfile::tempdir().unwrap();
        let state = fixture.state();
        let root = if let Some(capture) = state.capture() {
            fixture
                .store
                .root_path()
                .join(".quarantine")
                .join(std::str::from_utf8(capture.quarantine()).unwrap())
        } else {
            fixture
                .store
                .root_path()
                .join(&fixture.journal.generation().unwrap().identity().allocation)
        };
        let retained = outside.path().join("original");
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("foreign"), b"foreign root").unwrap();
        let error = fixture.recover().unwrap_err();
        assert!(format!("{error:?}").contains("identity"), "{error:?}");
        assert_eq!(fs::read(root.join("foreign")).unwrap(), b"foreign root");
        assert_eq!(
            fs::read(retained.join("lib/file")).unwrap(),
            b"restart owned bytes"
        );
    }
}

#[test]
fn both_active_and_retired_catalog_records_protect_the_generation_from_fresh_cleanup() {
    for active in [false, true] {
        let fixture = Restart::child("finished");
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                edit_catalog_metadata(control, CatalogCodecLimits::default(), |catalog| {
                    let generation = fixture.journal.generation().unwrap();
                    catalog.register_generation(GenerationRecord::new(
                        &generation.identity().allocation,
                        *fixture.journal.key().target(),
                        *generation.graph(),
                        [9; 32],
                        generation.inputs().to_vec(),
                        ModelLimits::default(),
                    )?)?;
                    if active {
                        catalog.apply_selection(
                            fixture.journal.key(),
                            &SelectionStamp::default(),
                            Some(&generation.identity().allocation),
                            Uuid::from_u128(303),
                        )?;
                    }
                    Ok(())
                })
            })
            .unwrap();
        let error = fixture.recover().unwrap_err();
        assert!(
            format!("{error:?}").contains("catalog record still protects"),
            "{error:?}"
        );
        original_inventory_remains(&fixture);
    }
}

#[test]
fn another_fresh_pending_journal_protects_the_same_generation() {
    let fixture = Restart::child("finished");
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let generation = fixture.journal.generation().unwrap();
            let identity = control.pin_generation_identity(generation.identity())?;
            prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: Uuid::from_u128(202),
                    key: fixture.journal.key(),
                    expected: &SelectionStamp::default(),
                    target: JournalTarget::Generation {
                        identity: &identity,
                        graph: *generation.graph(),
                        inputs: generation.inputs(),
                    },
                },
                JournalLimits::default(),
            )?;
            Ok(())
        })
        .unwrap();
    let error = fixture.recover().unwrap_err();
    assert!(
        format!("{error:?}").contains("another pending transaction still protects"),
        "{error:?}"
    );
    original_inventory_remains(&fixture);
}

#[test]
fn corrupt_other_journal_or_own_stream_fails_closed_without_partial_cleanup() {
    for journal in [true, false] {
        let fixture = Restart::child("finished");
        if journal {
            let path = fixture
                .store
                .root_path()
                .join(".control/journals")
                .join(format!("{}.json", Uuid::from_u128(202).simple()));
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap()
                .write_all(b"{bad metadata}")
                .unwrap();
        } else {
            let path = stream_path(&fixture);
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(path, bytes).unwrap();
        }
        let error = fixture.recover().unwrap_err();
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("journal") || diagnostic.contains("digest"),
            "{diagnostic}"
        );
        original_inventory_remains(&fixture);
    }
}

#[test]
fn tighter_full_recovery_budgets_fail_before_any_capture_or_input_acknowledgement() {
    let fixture = Restart::child("finished");
    let log = fixture.log();
    let journal_len = fixture.original_journal.len();
    let mut cases = Vec::new();
    let mut limits = OwnershipRecoveryLimits::default();
    limits.journal.max_document_bytes = journal_len - 1;
    cases.push(("journal", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.stream.max_total_bytes = log.total_bytes() - 1;
    cases.push(("raw total", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.stream.max_payload_bytes = 1;
    cases.push(("payload", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.stream.max_records = 1;
    cases.push(("raw count", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.max_event_bytes = 1;
    cases.push(("event bytes", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.max_events = 1;
    cases.push(("events", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.max_total_path_bytes = 1;
    cases.push(("paths", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.prefix.max_entries = 1;
    cases.push(("entries", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.prefix.max_cleanup_entries = 1;
    cases.push(("cleanup count", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.prefix.max_components = 1;
    cases.push(("depth", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.prefix.max_path_bytes = 1;
    cases.push(("path length", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.prefix.max_total_bytes = 1;
    cases.push(("finished bytes", limits));
    let mut limits = OwnershipRecoveryLimits::default();
    limits.namespace.max_entries = 1;
    cases.push(("namespace", limits));
    for (label, limits) in cases {
        let error = fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                control.recover_uncommitted_prefix(&fixture.journal, limits)
            })
            .unwrap_err();
        assert!(format!("{error:?}").contains("limit"), "{label}: {error:?}");
        original_inventory_remains(&fixture);
    }
}
