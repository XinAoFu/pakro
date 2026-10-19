use super::*;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use pakro_store::Digest;
use serde_json::{Value, json};

use crate::packages::python_environment::catalog::{edit_catalog_metadata, read_catalog_metadata};
use crate::packages::python_environment::model::{GenerationRecord, InputKind};
use crate::packages::python_environment::prefix::control::{ControlLimits, ControlNamespace};
use crate::packages::python_environment::prefix::{
    EnvironmentStore, PrefixLimits, UnpublishedPrefix,
};

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::open(
            &root.path().canonicalize().unwrap().join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .expect("real DE0 store prerequisite");
        store
            .with_control(ControlLimits::default(), |control| {
                assert!(
                    control
                        .list_names(ControlNamespace::Journals, 10, 1024)?
                        .is_empty()
                );
                let metadata = fs::symlink_metadata(store.root_path())?;
                assert_eq!(control.store_identity()?, (metadata.dev(), metadata.ino()));
                Ok(())
            })
            .expect("real bounded control-list/store-identity prerequisites");
        Self { _root: root, store }
    }
    fn allocate(&self) -> (UnpublishedPrefix, GenerationIdentity) {
        let prefix = self
            .store
            .allocate()
            .expect("real prefix allocation prerequisite");
        let identity = prefix
            .generation_identity()
            .expect("real E3 identity prerequisite");
        let facts = identity.facts().expect("real identity facts prerequisite");
        assert_eq!(facts.allocation, prefix.allocation_id());
        let metadata = fs::symlink_metadata(prefix.final_path()).unwrap();
        assert_eq!(facts.generation, [metadata.dev(), metadata.ino()]);
        // The caller retains this cleanup owner. Dropping it here would delete
        // the allocation and turn later namespace checks into a fixture failure.
        (prefix, identity)
    }
    fn directory(&self) -> PathBuf {
        self.store.root_path().join(".control/journals")
    }
    fn path(&self, nonce: Uuid) -> PathBuf {
        self.directory().join(filename(nonce))
    }
    fn state(&self) -> PathBuf {
        self.store.root_path().join(".control/catalog/state.json")
    }
    fn seed_named(&self, name: &str, bytes: &[u8]) {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.replace(ControlNamespace::Journals, name, None, bytes)?;
                Ok(())
            })
            .expect("real atomic control write prerequisite");
        assert_eq!(fs::read(self.directory().join(name)).unwrap(), bytes);
    }
    fn seed(&self, record: &JournalRecord) {
        self.seed_named(
            &filename(record.nonce),
            &serde_json::to_vec(&golden(record)).unwrap(),
        );
    }
    fn read(&self, limits: JournalLimits) -> Result<Vec<JournalRecord>> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                read_journal_metadata(control, limits)
            })
    }
}

fn filename(nonce: Uuid) -> String {
    format!("{}.json", nonce.simple())
}
fn owner() -> SelectionKey {
    SelectionKey::new(
        Uuid::from_u128(1),
        "开发 main",
        [3; 32],
        ModelLimits::default(),
    )
    .unwrap()
}
fn inputs() -> Vec<InputReference> {
    vec![
        InputReference::new(InputKind::Runtime, Digest::Sha256([1; 32])),
        InputReference::new(InputKind::Artifact, Digest::Sha512([2; 64])),
        InputReference::new(InputKind::BuildInput, Digest::Sha256([3; 32])),
    ]
}
fn independent_record(identity: &GenerationIdentity, nonce: u128) -> JournalRecord {
    JournalRecord {
        nonce: Uuid::from_u128(nonce),
        key: owner(),
        expected: JournalExpectedStamp {
            revision: 0,
            generation: None,
            transaction: Uuid::nil(),
        },
        target: RecordedTarget::Generation(JournalGenerationFacts {
            identity: identity
                .facts()
                .expect("actual identity facts prerequisite"),
            graph: [4; 32],
            inputs: inputs(),
        }),
    }
}
fn clear_record(fixture: &Fixture, nonce: u128) -> JournalRecord {
    let metadata = fs::symlink_metadata(fixture.store.root_path()).unwrap();
    JournalRecord {
        nonce: Uuid::from_u128(nonce),
        key: owner(),
        expected: JournalExpectedStamp {
            revision: 0,
            generation: None,
            transaction: Uuid::nil(),
        },
        target: RecordedTarget::ClearSelection {
            store: [metadata.dev(), metadata.ino()],
        },
    }
}

/// Independently specified wire, never the production encoder as sole oracle.
fn golden(record: &JournalRecord) -> Value {
    let target = match &record.target {
        RecordedTarget::Generation(generation) => {
            let identity = &generation.identity;
            json!({"kind":"generation", "identity":{
                "allocation":identity.allocation, "store":identity.store,
                "generation":identity.generation, "leases":identity.leases,
                "build":identity.build, "lock":identity.lock,
            }, "graph":hex::encode(generation.graph), "inputs":generation.inputs.iter().map(|reference| {
                let kind = match reference.kind() {
                    InputKind::Runtime => "runtime", InputKind::Artifact => "artifact", InputKind::BuildInput => "build-input",
                InputKind::Language => "language",
                InputKind::Package => "package",
                InputKind::BuildRequest => "build-request",
                InputKind::BuildOutput => "build-output",
                };
                json!({"kind":kind,"digest":reference.digest()})
            }).collect::<Vec<_>>()})
        }
        RecordedTarget::ClearSelection { store } => json!({"kind":"clear", "store":store}),
    };
    json!({"schema":1,"nonce":record.nonce.to_string(),"key":{
        "project":record.key.project().to_string(), "group":record.key.group(), "target":hex::encode(record.key.target()),
    }, "expected":{"revision":record.expected.revision,"generation":record.expected.generation,
        "transaction":if record.expected.transaction.is_nil(){None}else{Some(record.expected.transaction.to_string())}}, "target":target})
}
fn snapshot(path: &Path) -> (u64, u64, Vec<u8>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino(), fs::read(path).unwrap())
}
fn persist_commit(fixture: &Fixture, record: &JournalRecord) -> SelectionStamp {
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |catalog| {
                if let Some(generation) = record.generation() {
                    // Pure metadata fixture only, not a D validation/publish receipt.
                    catalog.register_generation(GenerationRecord::new(
                        &generation.identity.allocation,
                        *record.key.target(),
                        generation.graph,
                        [5; 32],
                        generation.inputs.clone(),
                        ModelLimits::default(),
                    )?)?;
                }
                catalog.apply_selection(
                    &record.key,
                    &SelectionStamp::default(),
                    record
                        .generation()
                        .map(|facts| facts.identity.allocation.as_str()),
                    record.nonce,
                )?;
                Ok(catalog.snapshot(&record.key))
            })
        })
        .expect("real catalog/codec/model commit metadata prerequisite")
}
fn catalog(fixture: &Fixture) -> CatalogModel {
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            read_catalog_metadata(control, CatalogCodecLimits::default())
        })
        .expect("real fresh catalog read prerequisite")
}
fn assert_committed(decision: JournalRecoveryDecision, nonce: Uuid) {
    assert!(
        matches!(decision, JournalRecoveryDecision::CatalogCommitted(_)),
        "catalog history was not recognized as committed: {decision:?}"
    );
    assert_eq!(decision.record().nonce(), nonce);
}

#[test]
fn generation_prepare_persists_all_actual_identity_facts_without_publishing() {
    let fixture = Fixture::new();
    let (prefix, identity) = fixture.allocate();
    let expected_record = independent_record(&identity, 101);
    let key = owner();
    let expected = SelectionStamp::default();
    let refs = inputs();
    let record = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: expected_record.nonce,
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
        .unwrap();
    assert_eq!(record, expected_record);
    let bytes =
        fs::read(fixture.path(record.nonce())).expect("prepare returned without durable journal");
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        golden(&expected_record)
    );
    let metadata = fs::symlink_metadata(fixture.path(record.nonce())).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(record.key(), &owner());
    assert_eq!(record.expected().revision(), 0);
    assert_eq!(record.expected().generation(), None);
    assert!(record.expected().transaction().is_nil());
    assert!(record.expected().matches(&expected));
    assert_eq!(
        record.generation().unwrap().identity(),
        &identity.facts().unwrap()
    );
    assert_eq!(record.generation().unwrap().graph(), &[4; 32]);
    assert_eq!(record.generation().unwrap().inputs(), refs);
    assert_eq!(record.store_identity(), identity.facts().unwrap().store);
    assert!(!fixture.state().exists());
    assert!(prefix.check_binding().is_ok());
    assert_eq!(
        catalog(&fixture).snapshot(&owner()),
        SelectionStamp::default()
    );
}

#[test]
fn clear_selection_is_persisted_without_allocation_and_is_not_cleanup_authority() {
    let fixture = Fixture::new();
    let (prefix, identity) = fixture.allocate();
    let old = independent_record(&identity, 101);
    let expected = persist_commit(&fixture, &old);
    let state_before = snapshot(&fixture.state());
    let mut record = clear_record(&fixture, 102);
    record.expected = JournalExpectedStamp {
        revision: expected.revision(),
        generation: expected.generation().map(str::to_owned),
        transaction: expected.transaction(),
    };
    let key = owner();
    let actual = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: record.nonce,
                    key: &key,
                    expected: &expected,
                    target: JournalTarget::ClearSelection,
                },
                JournalLimits::default(),
            )
        })
        .unwrap();
    assert_eq!(actual, record);
    let bytes = fs::read(fixture.path(record.nonce)).expect("clear intent was silently omitted");
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        golden(&record)
    );
    assert!(actual.generation().is_none());
    assert!(matches!(
        decide_recovery(record, &catalog(&fixture)).unwrap(),
        JournalRecoveryDecision::UncommittedClear(_)
    ));
    assert_eq!(snapshot(&fixture.state()), state_before);
    assert!(prefix.check_binding().is_ok());
}

#[test]
fn independently_seeded_journals_are_read_completely_in_nonce_order_without_rewrite() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let later = independent_record(&identity, 303);
    let earlier = clear_record(&fixture, 101);
    fixture.seed(&later);
    fixture.seed(&earlier);
    let before = [
        snapshot(&fixture.path(later.nonce)),
        snapshot(&fixture.path(earlier.nonce)),
    ];
    assert_eq!(
        fixture.read(JournalLimits::default()).unwrap(),
        vec![earlier.clone(), later.clone()]
    );
    assert_eq!(
        [
            snapshot(&fixture.path(later.nonce)),
            snapshot(&fixture.path(earlier.nonce))
        ],
        before
    );
}

#[test]
fn nonce_retry_is_exact_and_conflicting_nil_or_cross_store_requests_do_not_write() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let record = independent_record(&identity, 101);
    fixture.seed(&record);
    let before = snapshot(&fixture.path(record.nonce));
    let key = owner();
    let expected = SelectionStamp::default();
    let refs = inputs();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let retry = prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: record.nonce,
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
            assert_eq!(retry, record);
            Ok(())
        })
        .unwrap();
    assert_eq!(snapshot(&fixture.path(record.nonce)), before);
    let foreign = Fixture::new();
    let (_foreign_prefix, foreign_identity) = foreign.allocate();
    let another_key =
        SelectionKey::new(Uuid::from_u128(2), "other", [3; 32], ModelLimits::default()).unwrap();
    let changed_refs = vec![InputReference::new(
        InputKind::Runtime,
        Digest::Sha256([8; 32]),
    )];
    let changed_expected = persist_commit(&fixture, &record);
    let mut accepted = Vec::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            for (
                label,
                nonce,
                candidate_key,
                stamp,
                candidate_identity,
                graph,
                candidate_refs,
                clear,
            ) in [
                (
                    "key",
                    record.nonce,
                    &another_key,
                    &expected,
                    &identity,
                    [4; 32],
                    &refs,
                    false,
                ),
                (
                    "expected",
                    record.nonce,
                    &key,
                    &changed_expected,
                    &identity,
                    [4; 32],
                    &refs,
                    false,
                ),
                (
                    "graph",
                    record.nonce,
                    &key,
                    &expected,
                    &identity,
                    [8; 32],
                    &refs,
                    false,
                ),
                (
                    "inputs",
                    record.nonce,
                    &key,
                    &expected,
                    &identity,
                    [4; 32],
                    &changed_refs,
                    false,
                ),
                (
                    "target",
                    record.nonce,
                    &key,
                    &expected,
                    &identity,
                    [4; 32],
                    &refs,
                    true,
                ),
                (
                    "nil",
                    Uuid::nil(),
                    &key,
                    &expected,
                    &identity,
                    [4; 32],
                    &refs,
                    false,
                ),
                (
                    "cross-store",
                    Uuid::from_u128(909),
                    &key,
                    &expected,
                    &foreign_identity,
                    [4; 32],
                    &refs,
                    false,
                ),
            ] {
                let target = if clear {
                    JournalTarget::ClearSelection
                } else {
                    JournalTarget::Generation {
                        identity: candidate_identity,
                        graph,
                        inputs: candidate_refs,
                    }
                };
                if prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce,
                        key: candidate_key,
                        expected: stamp,
                        target,
                    },
                    JournalLimits::default(),
                )
                .is_ok()
                {
                    accepted.push(label);
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(
        accepted.is_empty(),
        "unsafe prepared journal requests accepted: {accepted:?}"
    );
    assert_eq!(snapshot(&fixture.path(record.nonce)), before);
    assert_eq!(fs::read_dir(fixture.directory()).unwrap().count(), 1);
}

#[test]
fn strict_wire_filename_identity_and_schema_errors_are_preserved_without_payload_echo() {
    const SECRET: &str = "https://user:journal-secret@example.invalid/private?token=hidden";
    let mut accepted = Vec::new();
    for label in [
        "schema",
        "unknown",
        "missing",
        "target-kind",
        "nil",
        "default-stamp",
        "nonzero-stamp",
        "allocation",
        "store",
        "inode-pair",
        "graph",
        "weak-digest",
        "input-kind",
        "duplicate",
        "nested-duplicate",
        "trailing",
        "malformed",
        "filename-body",
        "unknown-filename",
    ] {
        let child = Fixture::new();
        let (child_prefix, child_identity) = child.allocate();
        let original = independent_record(&child_identity, 101);
        let mut own = golden(&original);
        // Every negative keeps this child's real store/inodes unchanged except
        // for the one deliberately tested identity mutation.
        match label {
            "schema" => own["schema"] = json!(2),
            "unknown" => own["unexpected"] = json!(SECRET),
            "missing" => {
                own["key"].as_object_mut().unwrap().remove("group");
            }
            "target-kind" => own["target"]["kind"] = json!(SECRET),
            "nil" => own["nonce"] = json!(Uuid::nil().to_string()),
            "default-stamp" => {
                own["expected"]["generation"] =
                    json!(original.generation().unwrap().identity.allocation)
            }
            "nonzero-stamp" => own["expected"]["revision"] = json!(1),
            "allocation" => own["target"]["identity"]["allocation"] = json!("g-INVALID"),
            "store" => {
                let pair = original.store_identity();
                own["target"]["identity"]["store"] = json!([pair[0], pair[1] ^ 1]);
            }
            "inode-pair" => own["target"]["identity"]["lock"] = json!([1, 2, 3]),
            "graph" => own["target"]["graph"] = json!("zz".repeat(32)),
            "weak-digest" => own["target"]["inputs"][0]["digest"] = json!("md5-deadbeef"),
            "input-kind" => own["target"]["inputs"][0]["kind"] = json!("unknown"),
            _ => {}
        };
        let raw = serde_json::to_string(&own).unwrap();
        let bytes = match label {
            "duplicate" => raw
                .replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1)
                .into_bytes(),
            "nested-duplicate" => raw
                .replacen("\"revision\":0", "\"revision\":0,\"revision\":0", 1)
                .into_bytes(),
            "trailing" => format!("{raw} {{\"secret\":\"{SECRET}\"}}").into_bytes(),
            "malformed" => format!("{{broken:{SECRET}").into_bytes(),
            _ => raw.into_bytes(),
        };
        let name = match label {
            "filename-body" => filename(Uuid::from_u128(202)),
            "unknown-filename" => "unknown.json".into(),
            _ => filename(original.nonce),
        };
        child.seed_named(&name, &bytes);
        let before = snapshot(&child.directory().join(&name));
        match child.read(JournalLimits::default()) {
            Ok(_) => accepted.push(label),
            Err(error) => assert!(
                !format!("{error:?}").contains(SECRET),
                "journal error echoed payload"
            ),
        }
        assert_eq!(snapshot(&child.directory().join(&name)), before);
        assert!(child_prefix.check_binding().is_ok());
    }
    assert!(
        accepted.is_empty(),
        "malformed journal cases accepted: {accepted:?}"
    );
}

#[test]
fn document_listing_name_total_bytes_and_session_budgets_fail_without_partial_read() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let one = independent_record(&identity, 101);
    let two = clear_record(&fixture, 202);
    fixture.seed(&one);
    fixture.seed(&two);
    let lengths = [
        fs::metadata(fixture.path(one.nonce)).unwrap().len() as usize,
        fs::metadata(fixture.path(two.nonce)).unwrap().len() as usize,
    ];
    let sum = lengths.iter().sum::<usize>();
    let maximum = *lengths.iter().max().unwrap();
    let default = JournalLimits::default();
    let mut cases = vec![
        (
            "single-document",
            JournalLimits {
                max_document_bytes: maximum - 1,
                ..default
            },
        ),
        (
            "total-document",
            JournalLimits {
                max_total_document_bytes: sum - 1,
                ..default
            },
        ),
        (
            "document-count",
            JournalLimits {
                max_documents: 1,
                ..default
            },
        ),
        (
            "total-name",
            JournalLimits {
                max_total_name_bytes: 2 * 37 - 1,
                ..default
            },
        ),
        (
            "zero-document",
            JournalLimits {
                max_document_bytes: 0,
                ..default
            },
        ),
        (
            "zero-total",
            JournalLimits {
                max_total_document_bytes: 0,
                ..default
            },
        ),
        (
            "zero-count",
            JournalLimits {
                max_documents: 0,
                ..default
            },
        ),
        (
            "zero-name",
            JournalLimits {
                max_total_name_bytes: 0,
                ..default
            },
        ),
        (
            "zero-inputs",
            JournalLimits {
                max_inputs_per_record: 0,
                ..default
            },
        ),
        (
            "zero-global-inputs",
            JournalLimits {
                max_total_inputs: 0,
                ..default
            },
        ),
    ];
    let mut model = default.model;
    model.max_group_bytes = 0;
    cases.push(("zero-model", JournalLimits { model, ..default }));
    let mut accepted = Vec::new();
    for (label, limits) in cases {
        if fixture.read(limits).is_ok() {
            accepted.push(label);
        }
    }
    let result = fixture.store.with_control(
        ControlLimits {
            max_file_bytes: (*lengths.iter().min().unwrap() - 1) as u64,
            ..ControlLimits::default()
        },
        |control| read_journal_metadata(control, default),
    );
    if result.is_ok() {
        accepted.push("session-limit");
    }
    assert!(
        accepted.is_empty(),
        "journal read budgets ignored: {accepted:?}"
    );
    let exact = JournalLimits {
        max_document_bytes: maximum,
        max_total_document_bytes: sum,
        max_documents: 2,
        max_total_name_bytes: 74,
        ..default
    };
    assert_eq!(fixture.read(exact).unwrap(), vec![one, two]);
}

#[test]
fn typed_input_seeds_charge_raw_duplicates_and_reject_before_decoding_the_next_item() {
    let mut accepted = Vec::new();
    for global in [false, true] {
        let fixture = Fixture::new();
        let (_prefix, identity) = fixture.allocate();
        let record = independent_record(&identity, 101);
        let mut raw = golden(&record);
        let reference = raw["target"]["inputs"][0].clone();
        raw["target"]["inputs"] = json!([reference,reference,{"unexpected":"must-not-be-decoded"}]);
        fixture.seed_named(&filename(record.nonce), &serde_json::to_vec(&raw).unwrap());
        let limits = if global {
            JournalLimits {
                max_total_inputs: 2,
                ..JournalLimits::default()
            }
        } else {
            JournalLimits {
                max_inputs_per_record: 2,
                ..JournalLimits::default()
            }
        };
        match fixture.read(limits) {
            Ok(_) => accepted.push(if global { "global" } else { "per-record" }),
            Err(error) => assert!(
                error.to_string().contains("input") && error.to_string().contains("limit"),
                "decoder reached malformed over-budget element: {error:?}"
            ),
        }
    }
    assert!(
        accepted.is_empty(),
        "raw input collection budgets ignored: {accepted:?}"
    );
    // The global budget is shared across documents, not reset per record.
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    fixture.seed(&independent_record(&identity, 101));
    fixture.seed(&independent_record(&identity, 202));
    assert!(
        fixture
            .read(JournalLimits {
                max_total_inputs: 5,
                ..JournalLimits::default()
            })
            .is_err()
    );
}

#[test]
fn prepare_limits_reject_before_writing_and_cannot_exceed_the_existing_batch_budget() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let key = owner();
    let expected = SelectionStamp::default();
    let refs = inputs();
    let default = JournalLimits::default();
    let mut accepted = Vec::new();
    for (label, limits) in [
        (
            "encoded-bytes",
            JournalLimits {
                max_document_bytes: 1,
                ..default
            },
        ),
        (
            "raw-per-record-inputs",
            JournalLimits {
                max_inputs_per_record: 2,
                ..default
            },
        ),
        (
            "raw-total-inputs",
            JournalLimits {
                max_total_inputs: 2,
                ..default
            },
        ),
        (
            "zero-documents",
            JournalLimits {
                max_documents: 0,
                ..default
            },
        ),
    ] {
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                if prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce: Uuid::from_u128(901),
                        key: &key,
                        expected: &expected,
                        target: JournalTarget::Generation {
                            identity: &identity,
                            graph: [4; 32],
                            inputs: &refs,
                        },
                    },
                    limits,
                )
                .is_ok()
                {
                    accepted.push(label);
                }
                Ok(())
            })
            .unwrap();
    }
    assert!(
        accepted.is_empty(),
        "prepare ignored budgets before durable write: {accepted:?}"
    );
    assert!(fs::read_dir(fixture.directory()).unwrap().next().is_none());
    let original = independent_record(&identity, 101);
    fixture.seed(&original);
    let before = snapshot(&fixture.path(original.nonce));
    let result = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: Uuid::from_u128(902),
                    key: &key,
                    expected: &expected,
                    target: JournalTarget::ClearSelection,
                },
                JournalLimits {
                    max_documents: 1,
                    ..default
                },
            )
        });
    assert!(
        result.is_err(),
        "prepare exceeded current journal batch count"
    );
    assert_eq!(snapshot(&fixture.path(original.nonce)), before);
    assert!(!fixture.path(Uuid::from_u128(902)).exists());
}

#[test]
fn committed_nonce_is_recognized_from_real_catalog_metadata_without_active_mutation() {
    let fixture = Fixture::new();
    let (prefix, identity) = fixture.allocate();
    let record = independent_record(&identity, 101);
    fixture.seed(&record);
    persist_commit(&fixture, &record);
    let state_before = snapshot(&fixture.state());
    let journal_before = snapshot(&fixture.path(record.nonce));
    assert_committed(
        decide_recovery(record.clone(), &catalog(&fixture)).unwrap(),
        record.nonce,
    );
    let decisions = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            plan_journal_recovery(
                control,
                JournalLimits::default(),
                CatalogCodecLimits::default(),
            )
        })
        .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_committed(decisions.into_iter().next().unwrap(), record.nonce);
    assert_eq!(snapshot(&fixture.state()), state_before);
    assert_eq!(snapshot(&fixture.path(record.nonce)), journal_before);
    assert!(prefix.check_binding().is_ok());
}

#[test]
fn superseded_and_gc_history_remain_committed_but_nonce_fact_conflicts_are_rejected() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let record = independent_record(&identity, 101);
    let first = persist_commit(&fixture, &record);
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |catalog| {
                catalog.apply_selection(&record.key, &first, None, Uuid::from_u128(202))?;
                catalog.forget_retired(&record.generation().unwrap().identity.allocation)?;
                Ok(())
            })
        })
        .unwrap();
    let model = catalog(&fixture);
    assert!(
        model
            .generation(&record.generation().unwrap().identity.allocation)
            .is_none()
    );
    assert_eq!(model.snapshot(&record.key).revision(), 2);
    assert!(model.snapshot(&record.key).generation().is_none());
    assert_committed(
        decide_recovery(record.clone(), &model).unwrap(),
        record.nonce,
    );
    let mut conflicts = Vec::new();
    let mut changed = record.clone();
    changed.key =
        SelectionKey::new(Uuid::from_u128(9), "other", [3; 32], ModelLimits::default()).unwrap();
    conflicts.push(("key", changed));
    let mut changed = record.clone();
    changed.expected = JournalExpectedStamp {
        revision: first.revision(),
        generation: first.generation().map(str::to_owned),
        transaction: first.transaction(),
    };
    conflicts.push(("expected", changed));
    let mut changed = record.clone();
    changed.target = RecordedTarget::ClearSelection {
        store: record.store_identity(),
    };
    conflicts.push(("result", changed));
    let mut accepted = Vec::new();
    for (label, record) in conflicts {
        if decide_recovery(record, &model).is_ok() {
            accepted.push(label);
        }
    }
    assert!(
        accepted.is_empty(),
        "conflicting nonce facts accepted: {accepted:?}"
    );
    // A clear/tombstone commit is also first-class history.
    let mut clear = clear_record(&fixture, 202);
    clear.expected = JournalExpectedStamp {
        revision: first.revision(),
        generation: first.generation().map(str::to_owned),
        transaction: first.transaction(),
    };
    assert_committed(decide_recovery(clear.clone(), &model).unwrap(), clear.nonce);
}

#[test]
fn uncommitted_candidate_preserves_original_facts_and_refs_without_deleting_or_publishing() {
    let fixture = Fixture::new();
    let (prefix, identity) = fixture.allocate();
    let record = independent_record(&identity, 101);
    fixture.seed(&record);
    let before = snapshot(&fixture.path(record.nonce));
    let decision = decide_recovery(record.clone(), &catalog(&fixture)).unwrap();
    assert!(matches!(
        decision,
        JournalRecoveryDecision::CleanupCandidate(_)
    ));
    assert_eq!(decision.record(), &record);
    assert!(prefix.check_binding().is_ok());
    let decisions = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            plan_journal_recovery(
                control,
                JournalLimits::default(),
                CatalogCodecLimits::default(),
            )
        })
        .unwrap();
    assert_eq!(
        decisions.len(),
        1,
        "fresh journal was silently omitted from recovery"
    );
    assert!(
        matches!(&decisions[0],JournalRecoveryDecision::CleanupCandidate(candidate) if candidate==&record)
    );
    assert_eq!(snapshot(&fixture.path(record.nonce)), before);
    assert!(!fixture.state().exists());
    assert!(prefix.check_binding().is_ok());
}

#[test]
fn unsafe_or_unknown_journal_entries_fail_the_entire_batch_and_leave_evidence() {
    let mut accepted = Vec::new();
    for label in [
        "symlink",
        "fifo",
        "hardlink",
        "mode",
        "unknown",
        "temporary",
        "corrupt-peer",
    ] {
        let fixture = Fixture::new();
        let (prefix, identity) = fixture.allocate();
        let record = independent_record(&identity, 101);
        fixture.seed(&record);
        let before = snapshot(&fixture.path(record.nonce));
        let path = fixture.path(Uuid::from_u128(202));
        match label {
            "symlink" => symlink(fixture.path(record.nonce), &path).unwrap(),
            "fifo" => nix::unistd::mkfifo(
                &path,
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .unwrap(),
            "hardlink" => fs::hard_link(fixture.path(record.nonce), &path).unwrap(),
            "mode" => {
                fs::write(&path, b"{}").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            "unknown" => {
                let unknown = fixture.directory().join("unknown.json");
                fs::write(&unknown, b"{}").unwrap();
                fs::set_permissions(&unknown, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "temporary" => {
                let temp = fixture.directory().join(".tmp-crash");
                fs::write(&temp, b"{}").unwrap();
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "corrupt-peer" => fixture.seed_named(&filename(Uuid::from_u128(202)), b"{broken"),
            _ => unreachable!(),
        }
        if fixture.read(JournalLimits::default()).is_ok() {
            accepted.push(label);
        }
        let recovery = fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                plan_journal_recovery(
                    control,
                    JournalLimits::default(),
                    CatalogCodecLimits::default(),
                )
            });
        if recovery.is_ok() {
            accepted.push(label);
        }
        assert_eq!(snapshot(&fixture.path(record.nonce)), before);
        assert!(prefix.check_binding().is_ok());
        assert_eq!(fs::read_dir(fixture.directory()).unwrap().count(), 2);
    }
    assert!(
        accepted.is_empty(),
        "unsafe entries were silently skipped: {accepted:?}"
    );
}

#[test]
fn r04_journal_rejects_wrong_typed_key_algorithms_before_read_or_prepare() {
    for kind in [
        InputKind::Language,
        InputKind::Package,
        InputKind::BuildRequest,
    ] {
        let fixture = Fixture::new();
        let (_prefix, identity) = fixture.allocate();
        let invalid = InputReference::new(kind, Digest::Sha512([9; 64]));
        let key = owner();
        let expected = SelectionStamp::default();
        let refs = vec![invalid.clone()];
        let prepared = fixture
            .store
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
            });
        assert!(prepared.is_err());
        assert!(!fixture.path(Uuid::from_u128(101)).exists());
        let mut record = independent_record(&identity, 102);
        if let RecordedTarget::Generation(generation) = &mut record.target {
            generation.inputs = vec![invalid];
        }
        fixture.seed(&record);
        assert!(fixture.read(JournalLimits::default()).is_err());
    }
}

#[test]
fn r04_journal_roundtrip_and_recovery_accept_two_projects_sharing_typed_content() {
    let fixture = Fixture::new();
    let (_prefix, identity) = fixture.allocate();
    let refs: Vec<_> = [
        InputKind::Language,
        InputKind::Package,
        InputKind::BuildRequest,
        InputKind::BuildOutput,
    ]
    .into_iter()
    .map(|kind| InputReference::new(kind, Digest::Sha256([8; 32])))
    .collect();
    let other =
        SelectionKey::new(Uuid::from_u128(2), "other", [3; 32], ModelLimits::default()).unwrap();
    let keys = [owner(), other];
    let mut records = Vec::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            for (index, key) in keys.iter().enumerate() {
                records.push(prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce: Uuid::from_u128(101 + index as u128),
                        key,
                        expected: &SelectionStamp::default(),
                        target: JournalTarget::Generation {
                            identity: &identity,
                            graph: [4; 32],
                            inputs: &refs,
                        },
                    },
                    JournalLimits::default(),
                )?);
            }
            assert_eq!(
                read_journal_metadata(control, JournalLimits::default())?,
                records
            );
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |catalog| {
                let allocation = identity.facts()?.allocation;
                catalog.register_shared_generation(GenerationRecord::new_shared(
                    &allocation,
                    [3; 32],
                    [4; 32],
                    [5; 32],
                    [6; 32],
                    refs.clone(),
                    ModelLimits::default(),
                )?)?;
                for record in &records {
                    catalog.apply_selection(
                        record.key(),
                        &SelectionStamp::default(),
                        Some(&allocation),
                        record.nonce(),
                    )?;
                }
                Ok(())
            })?;
            let decisions = plan_journal_recovery(
                control,
                JournalLimits::default(),
                CatalogCodecLimits::default(),
            )?;
            assert_eq!(decisions.len(), 2);
            for (decision, record) in decisions.into_iter().zip(&records) {
                assert_committed(decision, record.nonce());
            }
            Ok(())
        })
        .unwrap();
}
