use super::*;
use crate::packages::python_environment::journal::{
    JournalPrepare, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use pakro_store::Digest;
use uuid::Uuid;

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
    prefix: UnpublishedPrefix,
    journal: JournalRecord,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::open(
            &root.path().canonicalize().unwrap().join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let prefix = store.allocate().unwrap();
        let identity = prefix.generation_identity().unwrap();
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
        let journal = store
            .with_control(ControlLimits::default(), |control| {
                prepare_journal_metadata(
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
                )
            })
            .expect("journal actual GREEN prerequisite");
        Self {
            _root: root,
            store,
            prefix,
            journal,
        }
    }
    fn root(&self) -> [u64; 2] {
        self.journal.generation().unwrap().identity().generation
    }
    fn id(&self, index: u64) -> [u64; 2] {
        let root = self.root();
        [root[0], root[1].wrapping_add(index + 100)]
    }
    fn limits(&self) -> OwnershipEventLimits {
        OwnershipEventLimits {
            final_prefix_bytes: self
                .prefix
                .final_path()
                .as_os_str()
                .as_encoded_bytes()
                .len(),
            ..OwnershipEventLimits::default()
        }
    }
    fn log_bytes(&self, payloads: &[Vec<u8>]) -> OwnershipLog {
        let mut stream = self
            .store
            .with_control(ControlLimits::default(), |control| {
                control.create_ownership_stream(
                    &self.journal,
                    &self.prefix,
                    JournalLimits::default(),
                    OwnershipStreamLimits::default(),
                )
            })
            .expect("real raw create prerequisite");
        for (index, payload) in payloads.iter().enumerate() {
            let receipt = stream
                .append(&self.prefix, payload)
                .expect("real raw append prerequisite");
            assert_eq!(receipt.sequence(), index as u64 + 1);
        }
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.read_ownership_stream(
                    &self.journal,
                    JournalLimits::default(),
                    OwnershipStreamLimits::default(),
                )
            })
            .expect("real raw framing reader prerequisite")
            .unwrap()
    }
    fn log(&self, events: &[OwnershipEvent]) -> OwnershipLog {
        self.log_bytes(&events.iter().map(golden).collect::<Vec<_>>())
    }
    fn creation(&self) -> Vec<OwnershipEvent> {
        vec![
            dir(1, b"lib", self.root()),
            OwnershipEvent::Created {
                op: 1,
                identity: self.id(1),
            },
            file(3, b"lib/mod.py", self.id(1)),
            OwnershipEvent::Created {
                op: 3,
                identity: self.id(2),
            },
            OwnershipEvent::FileFinished {
                op: 3,
                size: 7,
                mode: 0o644,
            },
        ]
    }
    fn captured(&self) -> Vec<OwnershipEvent> {
        let mut events = self.creation();
        events.push(capture(6, self.root()));
        events.push(OwnershipEvent::Captured { op: 6 });
        events
    }
}
fn dir(op: u64, path: &[u8], parent: [u64; 2]) -> OwnershipEvent {
    OwnershipEvent::CreateIntent {
        op,
        kind: OwnershipEntryKind::Directory,
        relative: path.to_vec(),
        parent,
    }
}
fn file(op: u64, path: &[u8], parent: [u64; 2]) -> OwnershipEvent {
    OwnershipEvent::CreateIntent {
        op,
        kind: OwnershipEntryKind::File,
        relative: path.to_vec(),
        parent,
    }
}
fn capture(op: u64, root: [u64; 2]) -> OwnershipEvent {
    OwnershipEvent::CaptureIntent {
        op,
        quarantine: format!("q-{:032x}", 17).into_bytes(),
        root,
    }
}
fn remove(op: u64, kind: OwnershipEntryKind, path: &[u8], identity: [u64; 2]) -> OwnershipEvent {
    OwnershipEvent::RemoveIntent {
        op,
        kind,
        relative: path.to_vec(),
        identity,
    }
}

/// Exact independent binary protocol; production encoder is not an oracle.
fn golden(event: &OwnershipEvent) -> Vec<u8> {
    let (tag, operation) = match event {
        OwnershipEvent::CreateIntent { op, .. } => (1, *op),
        OwnershipEvent::Created { op, .. } => (2, *op),
        OwnershipEvent::FileFinished { op, .. } => (3, *op),
        OwnershipEvent::CaptureIntent { op, .. } => (4, *op),
        OwnershipEvent::Captured { op } => (5, *op),
        OwnershipEvent::RemoveIntent { op, .. } => (6, *op),
        OwnershipEvent::Removed { op } => (7, *op),
        OwnershipEvent::RootRemoved { capture_op } => (8, *capture_op),
        OwnershipEvent::Complete {
            root_removed_sequence,
        } => (9, *root_removed_sequence),
    };
    let mut bytes = vec![1, tag];
    bytes.extend_from_slice(&operation.to_be_bytes());
    match event {
        OwnershipEvent::CreateIntent {
            kind,
            relative,
            parent,
            ..
        } => {
            bytes.push(kind_byte(*kind));
            path(&mut bytes, relative);
            pair(&mut bytes, *parent);
        }
        OwnershipEvent::Created { identity, .. } => pair(&mut bytes, *identity),
        OwnershipEvent::FileFinished { size, mode, .. } => {
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&mode.to_be_bytes());
        }
        OwnershipEvent::CaptureIntent {
            quarantine, root, ..
        } => {
            path(&mut bytes, quarantine);
            pair(&mut bytes, *root);
        }
        OwnershipEvent::RemoveIntent {
            kind,
            relative,
            identity,
            ..
        } => {
            bytes.push(kind_byte(*kind));
            path(&mut bytes, relative);
            pair(&mut bytes, *identity);
        }
        _ => {}
    };
    bytes
}
fn kind_byte(kind: OwnershipEntryKind) -> u8 {
    match kind {
        OwnershipEntryKind::Directory => 1,
        OwnershipEntryKind::File => 2,
    }
}
fn path(bytes: &mut Vec<u8>, path: &[u8]) {
    bytes.extend_from_slice(&(path.len() as u32).to_be_bytes());
    bytes.extend_from_slice(path);
}
fn pair(bytes: &mut Vec<u8>, pair: [u64; 2]) {
    for number in pair {
        bytes.extend_from_slice(&number.to_be_bytes());
    }
}
fn variants() -> Vec<OwnershipEvent> {
    vec![
        dir(1, b"lib", [1, 2]),
        OwnershipEvent::Created {
            op: 1,
            identity: [1, 3],
        },
        OwnershipEvent::FileFinished {
            op: 3,
            size: 7,
            mode: 0o755,
        },
        capture(6, [1, 2]),
        OwnershipEvent::Captured { op: 6 },
        remove(8, OwnershipEntryKind::File, b"lib/a", [1, 3]),
        OwnershipEvent::Removed { op: 8 },
        OwnershipEvent::RootRemoved { capture_op: 6 },
        OwnershipEvent::Complete {
            root_removed_sequence: 12,
        },
    ]
}

#[test]
fn encoder_matches_all_nine_independent_binary_golden_variants() {
    let mut mismatches = Vec::new();
    for event in variants() {
        if !encode_ownership_event(&event, OwnershipEventLimits::default())
            .is_ok_and(|bytes| bytes == golden(&event))
        {
            mismatches.push(format!("{event:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "event encoder mismatches: {mismatches:?}"
    );
}
#[test]
fn decoder_accepts_independent_binary_golden_without_using_the_encoder() {
    let mut mismatches = Vec::new();
    for event in variants() {
        if !decode_ownership_event(&golden(&event), OwnershipEventLimits::default())
            .is_ok_and(|value| value == event)
        {
            mismatches.push(format!("{event:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "event decoder mismatches: {mismatches:?}"
    );
}
#[test]
fn decoder_rejects_unknown_versions_tags_kinds_truncation_trailing_and_declared_overflow() {
    let mut cases = Vec::new();
    let original = golden(&file(1, b"path", [1, 2]));
    let mut bytes = original.clone();
    bytes[0] = 2;
    cases.push(("version", bytes));
    let mut bytes = original.clone();
    bytes[1] = 99;
    cases.push(("tag", bytes));
    let mut bytes = original.clone();
    bytes[10] = 3;
    cases.push(("kind", bytes));
    for end in [0, 1, 2, 9, 10, 14, original.len() - 1] {
        cases.push(("truncated", original[..end].to_vec()));
    }
    let mut bytes = original.clone();
    bytes.push(0);
    cases.push(("trailing", bytes));
    let mut bytes = original;
    bytes[11..15].copy_from_slice(&u32::MAX.to_be_bytes());
    cases.push(("length", bytes));
    let mut accepted = Vec::new();
    for (label, bytes) in cases {
        match decode_ownership_event(&bytes, OwnershipEventLimits::default()) {
            Ok(_) => accepted.push(label),
            Err(error) => {
                if label == "length" {
                    assert!(
                        error.to_string().contains("limit"),
                        "declared length parsed/allocated before limit: {error:?}"
                    );
                }
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "bad binary events accepted: {accepted:?}"
    );
}
#[test]
fn unix_paths_preserve_non_utf8_unicode_and_alias_spelling_but_reject_unsafe_components() {
    let mut bad_roundtrips = 0;
    for raw in ["目录/a.py".as_bytes(), b"lib/\xff", b"a//b", b"a/"] {
        let event = file(1, raw, [1, 2]);
        if !decode_ownership_event(&golden(&event), OwnershipEventLimits::default())
            .is_ok_and(|value| value == event)
        {
            bad_roundtrips += 1;
        }
    }
    let mut accepted = Vec::new();
    for raw in [
        b"".as_slice(),
        b"/absolute",
        b"../up",
        b"a/./b",
        b"a/../b",
        b"a\0b",
        b".",
    ] {
        let event = file(1, raw, [1, 2]);
        if encode_ownership_event(&event, OwnershipEventLimits::default()).is_ok() {
            accepted.push(raw);
        }
        if decode_ownership_event(&golden(&event), OwnershipEventLimits::default()).is_ok() {
            accepted.push(raw);
        }
    }
    assert!(
        accepted.is_empty() && bad_roundtrips == 0,
        "unsafe relative path accepts={}, valid raw byte mismatches={bad_roundtrips}",
        accepted.len()
    );
}
#[test]
fn budgets_charge_payload_paths_events_entries_and_stats_before_collecting_or_decoding_tail() {
    let mut accepted = Vec::new();
    let event = file(1, b"long", [1, 2]);
    let normal = OwnershipEventLimits::default();
    let mut prefix = normal.prefix;
    prefix.max_path_bytes = normal.final_prefix_bytes + 1 + 3;
    let mut components = normal.prefix;
    components.max_components = 1;
    for (label, limits, event) in [
        (
            "single-bytes",
            OwnershipEventLimits {
                max_event_bytes: 1,
                ..normal
            },
            event.clone(),
        ),
        (
            "full-path",
            OwnershipEventLimits { prefix, ..normal },
            event.clone(),
        ),
        (
            "components",
            OwnershipEventLimits {
                prefix: components,
                ..normal
            },
            file(1, b"a/b", [1, 2]),
        ),
        (
            "zero",
            OwnershipEventLimits {
                max_total_path_bytes: 0,
                ..normal
            },
            event,
        ),
    ] {
        if encode_ownership_event(&event, limits).is_ok() {
            accepted.push(label);
        }
        if decode_ownership_event(&golden(&event), limits).is_ok() {
            accepted.push(label);
        }
    }
    for label in ["event-count", "total-path", "entries", "finished-bytes"] {
        let fixture = Fixture::new();
        let mut limits = fixture.limits();
        let mut events = fixture.creation();
        match label {
            "event-count" => limits.max_events = 1,
            "total-path" => limits.max_total_path_bytes = 2,
            "entries" => limits.prefix.max_entries = 1,
            "finished-bytes" => limits.prefix.max_total_bytes = 6,
            _ => unreachable!(),
        };
        let mut payloads = events
            .drain(..)
            .map(|event| golden(&event))
            .collect::<Vec<_>>();
        if label == "event-count" {
            payloads[1] = vec![255];
        }
        let log = fixture.log_bytes(&payloads);
        match validate_ownership_events(&log, limits) {
            Ok(_) => accepted.push(label),
            Err(error) => assert!(
                error.to_string().contains("limit"),
                "budget failed after semantic decode: {error:?}"
            ),
        }
    }
    assert!(
        accepted.is_empty(),
        "typed event resource budgets ignored: {accepted:?}"
    );
}
#[test]
fn valid_creation_tree_projects_known_inodes_actual_stats_and_raw_frame_receipts() {
    let fixture = Fixture::new();
    let events = fixture.creation();
    let log = fixture.log(&events);
    let state = validate_ownership_events(&log, fixture.limits()).unwrap();
    assert_eq!(state.phase(), OwnershipRecoveryPhase::Building);
    assert_eq!(state.entries().len(), 2);
    assert!(!state.requires_retention());
    assert!(state.pending_create().is_none());
    assert_eq!(state.events(), 5);
    let directory = &state.entries()[0];
    assert_eq!(directory.relative(), b"lib");
    assert_eq!(directory.kind(), OwnershipEntryKind::Directory);
    assert_eq!(directory.parent(), fixture.root());
    assert_eq!(directory.identity(), fixture.id(1));
    assert_eq!(directory.create_op(), 1);
    assert!(directory.finished().is_none());
    assert!(!directory.removed());
    assert_eq!(
        directory.created_frame_sha256(),
        log.records()[1].chain_sha256()
    );
    let file = &state.entries()[1];
    assert_eq!(file.relative(), b"lib/mod.py");
    let finished = file.finished().unwrap();
    assert_eq!(finished.size(), 7);
    assert_eq!(finished.mode(), 0o644);
    assert_eq!(finished.frame_sha256(), log.records()[4].chain_sha256());
    assert_eq!(state.last_frame_sha256(), log.head());
    assert!(fixture.prefix.check_binding().is_ok());
}
#[test]
fn intent_without_created_is_uncertain_but_created_unfinished_retains_known_inode_for_cleanup() {
    let mut observed = Vec::new();
    for created in [false, true] {
        let fixture = Fixture::new();
        let mut events = vec![file(1, b"output", fixture.root())];
        if created {
            events.push(OwnershipEvent::Created {
                op: 1,
                identity: fixture.id(1),
            });
        }
        let log = fixture.log(&events);
        let state = validate_ownership_events(&log, fixture.limits()).unwrap();
        observed.push((
            state.requires_retention(),
            state.entries().len(),
            state.pending_create().and_then(|pending| pending.created()),
        ));
        if let Some(pending) = state.pending_create() {
            assert_eq!(pending.operation(), 1);
            assert_eq!(pending.relative(), b"output");
            assert_eq!(pending.kind(), OwnershipEntryKind::File);
            assert_eq!(pending.parent(), fixture.root());
        }
        if let Some(entry) = state.entries().first() {
            assert!(entry.finished().is_none());
        }
    }
    assert_eq!(
        observed
            .iter()
            .map(|(retain, count, identity)| (*retain, *count, identity.is_some()))
            .collect::<Vec<_>>(),
        vec![(true, 0, false), (false, 1, true)]
    );
}
#[test]
fn creation_op_parent_alias_inode_and_finish_order_conflicts_are_rejected() {
    let mut accepted = Vec::new();
    for label in [
        "intent-op",
        "unknown-created",
        "parent",
        "overlap-intent",
        "alias",
        "inode",
        "finish-directory",
        "finish-before-created",
        "duplicate-finish",
        "mode",
    ] {
        let fixture = Fixture::new();
        let root = fixture.root();
        let id = fixture.id(1);
        let mut events = fixture.creation();
        match label {
            "intent-op" => events[0] = dir(99, b"lib", root),
            "unknown-created" => {
                events[1] = OwnershipEvent::Created {
                    op: 99,
                    identity: id,
                }
            }
            "parent" => events[0] = dir(1, b"lib", [0, 0]),
            "overlap-intent" => events.insert(1, dir(2, b"other", root)),
            "alias" => {
                events = vec![
                    dir(1, b"a/", root),
                    OwnershipEvent::Created {
                        op: 1,
                        identity: id,
                    },
                    dir(3, b"a//", root),
                ];
            }
            "inode" => {
                events[3] = OwnershipEvent::Created {
                    op: 3,
                    identity: id,
                }
            }
            "finish-directory" => events.insert(
                2,
                OwnershipEvent::FileFinished {
                    op: 1,
                    size: 0,
                    mode: 0o755,
                },
            ),
            "finish-before-created" => events.swap(3, 4),
            "duplicate-finish" => events.push(OwnershipEvent::FileFinished {
                op: 3,
                size: 7,
                mode: 0o644,
            }),
            "mode" => {
                events[4] = OwnershipEvent::FileFinished {
                    op: 3,
                    size: 7,
                    mode: 0o4644,
                }
            }
            _ => unreachable!(),
        }
        let log = fixture.log(&events);
        if validate_ownership_events(&log, fixture.limits()).is_ok() {
            accepted.push(label);
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid creation histories accepted: {accepted:?}"
    );
}
#[test]
fn full_capture_reverse_depth_removal_root_removed_and_complete_are_closed_data_states() {
    let fixture = Fixture::new();
    let mut events = fixture.captured();
    events.extend([
        remove(8, OwnershipEntryKind::File, b"lib/mod.py", fixture.id(2)),
        OwnershipEvent::Removed { op: 8 },
        remove(10, OwnershipEntryKind::Directory, b"lib", fixture.id(1)),
        OwnershipEvent::Removed { op: 10 },
        OwnershipEvent::RootRemoved { capture_op: 6 },
        OwnershipEvent::Complete {
            root_removed_sequence: 12,
        },
    ]);
    let log = fixture.log(&events);
    let state = validate_ownership_events(&log, fixture.limits()).unwrap();
    assert_eq!(state.phase(), OwnershipRecoveryPhase::Complete);
    assert!(!state.requires_retention());
    assert_eq!(state.root_removed_sequence(), Some(12));
    assert!(state.entries().iter().all(OwnershipEntryFacts::removed));
    assert!(state.pending_remove().is_none());
    let capture = state.capture().unwrap();
    assert_eq!(capture.operation(), 6);
    assert!(capture.captured());
    assert_eq!(capture.root(), fixture.root());
    assert_eq!(capture.quarantine(), format!("q-{:032x}", 17).as_bytes());
    // Pure state never performs the recorded operations on this actual prefix.
    assert!(fixture.prefix.check_binding().is_ok());
    assert!(fixture.prefix.final_path().exists());
}
#[test]
fn cleanup_root_qname_path_kind_identity_order_and_terminal_conflicts_are_rejected() {
    let mut accepted = Vec::new();
    for label in [
        "qname",
        "capture-root",
        "uncaptured",
        "parent-first",
        "path",
        "kind",
        "identity",
        "removed-op",
        "root-early",
        "unknown-intent",
        "terminal",
    ] {
        let fixture = Fixture::new();
        let mut events = fixture.captured();
        match label {
            "qname" => {
                events[5] = OwnershipEvent::CaptureIntent {
                    op: 6,
                    quarantine: b"../outside".to_vec(),
                    root: fixture.root(),
                }
            }
            "capture-root" => events[5] = capture(6, [0, 0]),
            "uncaptured" => {
                events = fixture.creation();
                events.push(remove(
                    6,
                    OwnershipEntryKind::File,
                    b"lib/mod.py",
                    fixture.id(2),
                ));
            }
            "parent-first" => events.push(remove(
                8,
                OwnershipEntryKind::Directory,
                b"lib",
                fixture.id(1),
            )),
            "path" => events.push(remove(
                8,
                OwnershipEntryKind::File,
                b"unknown",
                fixture.id(2),
            )),
            "kind" => events.push(remove(
                8,
                OwnershipEntryKind::Directory,
                b"lib/mod.py",
                fixture.id(2),
            )),
            "identity" => events.push(remove(
                8,
                OwnershipEntryKind::File,
                b"lib/mod.py",
                fixture.id(3),
            )),
            "removed-op" => {
                events.push(remove(
                    8,
                    OwnershipEntryKind::File,
                    b"lib/mod.py",
                    fixture.id(2),
                ));
                events.push(OwnershipEvent::Removed { op: 99 });
            }
            "root-early" => events.push(OwnershipEvent::RootRemoved { capture_op: 6 }),
            "unknown-intent" => {
                events = vec![
                    file(1, b"unknown", fixture.root()),
                    capture(2, fixture.root()),
                    OwnershipEvent::Captured { op: 2 },
                    OwnershipEvent::RootRemoved { capture_op: 2 },
                ];
            }
            "terminal" => {
                events = vec![
                    capture(1, fixture.root()),
                    OwnershipEvent::Captured { op: 1 },
                    OwnershipEvent::RootRemoved { capture_op: 1 },
                    OwnershipEvent::Complete {
                        root_removed_sequence: 3,
                    },
                    OwnershipEvent::Complete {
                        root_removed_sequence: 3,
                    },
                ];
            }
            _ => unreachable!(),
        }
        let log = fixture.log(&events);
        if validate_ownership_events(&log, fixture.limits()).is_ok() {
            accepted.push(label);
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid cleanup histories accepted: {accepted:?}"
    );
}
#[test]
fn pending_capture_and_remove_preserve_restart_facts_without_assuming_kernel_success() {
    let mut observed = Vec::new();
    for removing in [false, true] {
        let fixture = Fixture::new();
        let mut events = fixture.creation();
        events.push(capture(6, fixture.root()));
        if removing {
            events.push(OwnershipEvent::Captured { op: 6 });
            events.push(remove(
                8,
                OwnershipEntryKind::File,
                b"lib/mod.py",
                fixture.id(2),
            ));
        }
        let log = fixture.log(&events);
        let state = validate_ownership_events(&log, fixture.limits()).unwrap();
        observed.push((
            state.phase(),
            state.capture().is_some(),
            state.pending_remove().is_some(),
        ));
        if let Some(pending) = state.pending_remove() {
            assert_eq!(pending.operation(), 8);
            assert_eq!(pending.relative(), b"lib/mod.py");
            assert_eq!(pending.kind(), OwnershipEntryKind::File);
            assert_eq!(pending.identity(), fixture.id(2));
            assert!(!state.entries()[1].removed());
        }
        assert!(fixture.prefix.check_binding().is_ok());
    }
    assert_eq!(
        observed,
        vec![
            (OwnershipRecoveryPhase::CapturePending, true, false),
            (OwnershipRecoveryPhase::Captured, true, true)
        ]
    );
}
