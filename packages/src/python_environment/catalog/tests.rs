use super::*;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use pakro_store::Digest;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::packages::python_environment::model::{
    GenerationRecord, InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use crate::packages::python_environment::prefix::control::{ControlLimits, ControlNamespace};
use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("environments");
        let store = EnvironmentStore::open(
            &path,
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .expect("real DE0 store prerequisite must already be GREEN");
        store
            .with_control(ControlLimits::default(), |_| Ok(()))
            .expect("real E2 control lock/namespaces prerequisite must already be GREEN");
        Self { _root: root, store }
    }
    fn path(&self) -> PathBuf {
        self.store.root_path().join(".control/catalog/state.json")
    }
    fn seed(&self, bytes: &[u8]) {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.replace(ControlNamespace::Catalog, "state.json", None, bytes)?;
                Ok(())
            })
            .expect("real atomic control write prerequisite must already be GREEN");
        assert_eq!(fs::read(self.path()).unwrap(), bytes);
    }
    fn seed_valid(&self, bytes: &[u8]) {
        CatalogModel::decode_catalog(bytes, CatalogCodecLimits::default()).expect(
            "independently specified codec fixture must already be GREEN, not repository RED",
        );
        self.seed(bytes);
    }
    fn independent_disk_model(&self) -> CatalogModel {
        CatalogModel::decode_catalog(
            &fs::read(self.path()).unwrap(),
            CatalogCodecLimits::default(),
        )
        .unwrap()
    }
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
fn allocation(index: u128) -> String {
    format!("g-{index:032x}")
}
fn golden() -> Vec<u8> {
    let key = json!({"project":Uuid::from_u128(1).to_string(),"group":"开发 main","target":"03".repeat(32)});
    serde_json::to_vec(&json!({
        "schema":3,
        "generations":[{"allocation":allocation(1),"target":"03".repeat(32),"shared_layout":null,"graph":"04".repeat(32),"receipt":"05".repeat(32),
            "inputs":[{"kind":"artifact","digest":format!("sha256-{}","01".repeat(32))}]}],
        "selections":[{"key":key,"stamp":{"revision":1,"generation":allocation(1),"language":null,"transaction":Uuid::from_u128(101).to_string()}}],
        "transactions":[{"id":Uuid::from_u128(101).to_string(),"key":key,
            "expected":{"revision":0,"generation":null,"language":null,"transaction":null},"generation":allocation(1),"language":null}],
        "projects":[]
    })).unwrap()
}
fn empty_golden() -> &'static [u8] {
    br#"{"schema":2,"generations":[],"selections":[],"transactions":[]}"#
}
fn register(model: &mut CatalogModel, index: u128) -> Result<()> {
    model.register_generation(GenerationRecord::new(
        &allocation(index),
        *owner().target(),
        [4; 32],
        [5; 32],
        vec![InputReference::new(
            if index == 1 {
                InputKind::Artifact
            } else {
                InputKind::Runtime
            },
            if index == 1 {
                Digest::Sha256([1; 32])
            } else {
                Digest::Sha512([2; 64])
            },
        )],
        ModelLimits::default(),
    )?)
}
fn metadata_identity(path: &std::path::Path) -> (u64, u64, Vec<u8>) {
    let meta = fs::symlink_metadata(path).unwrap();
    (meta.dev(), meta.ino(), fs::read(path).unwrap())
}

#[test]
fn missing_read_returns_an_empty_pure_model_without_creating_state() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let model = read_catalog_metadata(control, CatalogCodecLimits::default())?;
            assert_eq!(model.snapshot(&owner()), SelectionStamp::default());
            assert!(model.protecting_references().is_empty());
            assert!(model.retired_generations().is_empty());
            Ok(())
        })
        .unwrap();
    assert!(!fixture.path().exists());
}

#[test]
fn seeded_read_observes_existing_selection_and_references_without_rewriting() {
    let fixture = Fixture::new();
    fixture.seed_valid(&golden());
    let before = metadata_identity(&fixture.path());
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let model = read_catalog_metadata(control, CatalogCodecLimits::default())?;
            assert_eq!(model.snapshot(&owner()).revision(), 1);
            assert_eq!(
                model.snapshot(&owner()).generation(),
                Some(allocation(1).as_str())
            );
            assert_eq!(model.protecting_references().len(), 1);
            assert_eq!(model.generation(&allocation(1)).unwrap().graph(), &[4; 32]);
            Ok(())
        })
        .unwrap();
    assert_eq!(metadata_identity(&fixture.path()), before);
}

#[test]
fn first_edit_persists_complete_wire_facts_and_preserves_its_callback_result() {
    let fixture = Fixture::new();
    let mut calls = 0;
    let result = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                calls += 1;
                assert_eq!(model.snapshot(&owner()).revision(), 0);
                register(model, 1)?;
                model.apply_selection(
                    &owner(),
                    &SelectionStamp::default(),
                    Some(&allocation(1)),
                    Uuid::from_u128(101),
                )?;
                Ok(("pure-edit-result", 42_u32))
            })
        })
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result, ("pure-edit-result", 42));
    let bytes = fs::read(fixture.path()).expect("repository did not persist catalog/state.json");
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        serde_json::from_slice::<Value>(&golden()).unwrap()
    );
    let metadata = fs::symlink_metadata(fixture.path()).unwrap();
    assert!(metadata.is_file() && !metadata.file_type().is_symlink());
    assert_eq!((metadata.mode() & 0o777, metadata.nlink()), (0o600, 1));
}

#[test]
fn an_edit_loads_current_state_before_mutation_and_keeps_retired_input_refs() {
    let fixture = Fixture::new();
    fixture.seed_valid(&golden());
    let mut calls = 0;
    let revisions = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                calls += 1;
                let old = model.snapshot(&owner());
                assert_eq!(
                    old.revision(),
                    1,
                    "edit was given a fresh model instead of current metadata"
                );
                register(model, 2)?;
                model.apply_selection(
                    &owner(),
                    &old,
                    Some(&allocation(2)),
                    Uuid::from_u128(102),
                )?;
                Ok((old.revision(), model.snapshot(&owner()).revision()))
            })
        })
        .unwrap();
    assert_eq!((calls, revisions), (1, (1, 2)));
    let model = fixture.independent_disk_model();
    assert_eq!(
        model.snapshot(&owner()).generation(),
        Some(allocation(2).as_str())
    );
    assert_eq!(model.retired_generations(), vec![allocation(1).as_str()]);
    assert_eq!(model.protecting_references().len(), 2);
}

#[test]
fn invalid_existing_documents_never_become_missing_or_invoke_the_edit_callback() {
    for bytes in [
        b"not JSON synthetic-private-token".to_vec(),
        br#"{"schema":4,"generations":[],"selections":[],"transactions":[]}"#.to_vec(),
    ] {
        let fixture = Fixture::new();
        fixture.seed(&bytes);
        let before = metadata_identity(&fixture.path());
        let mut calls = 0;
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                let read = read_catalog_metadata(control, CatalogCodecLimits::default());
                let edit = edit_catalog_metadata(control, CatalogCodecLimits::default(), |_| {
                    calls += 1;
                    Ok(())
                });
                assert!(
                    read.is_err() && edit.is_err(),
                    "invalid catalog was replaced by a fresh model"
                );
                for error in [read.unwrap_err(), edit.unwrap_err()] {
                    assert!(!format!("{error:#}").contains("synthetic-private-token"));
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(metadata_identity(&fixture.path()), before);
    }
}

#[test]
fn codec_and_session_byte_bounds_fail_before_editing_and_do_not_rewrite() {
    let fixture = Fixture::new();
    let bytes = golden();
    fixture.seed_valid(&bytes);
    let before = metadata_identity(&fixture.path());
    let mut accepted = vec![];
    let mut calls = 0;
    for dimension in 0..3 {
        let mut codec = CatalogCodecLimits::default();
        let mut session = ControlLimits::default();
        match dimension {
            0 => codec.max_document_bytes = 0,
            1 => codec.max_document_bytes = bytes.len() - 1,
            _ => session.max_file_bytes = bytes.len() as u64 - 1,
        }
        fixture
            .store
            .with_control(session, |control| {
                if read_catalog_metadata(control, codec).is_ok() {
                    accepted.push((dimension, "read"));
                }
                if edit_catalog_metadata(control, codec, |_| {
                    calls += 1;
                    Ok(())
                })
                .is_ok()
                {
                    accepted.push((dimension, "edit"));
                }
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        calls, 0,
        "edit callback was invoked despite a failed raw read budget"
    );
    assert!(
        accepted.is_empty(),
        "repository ignored byte budgets: {accepted:?}"
    );
    assert_eq!(metadata_identity(&fixture.path()), before);
    fixture
        .store
        .with_control(
            ControlLimits {
                max_file_bytes: bytes.len() as u64,
                ..Default::default()
            },
            |control| {
                assert_eq!(
                    read_catalog_metadata(
                        control,
                        CatalogCodecLimits {
                            max_document_bytes: bytes.len(),
                            ..Default::default()
                        }
                    )?
                    .snapshot(&owner())
                    .revision(),
                    1
                );
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn a_failed_callback_drops_its_local_model_changes_without_touching_metadata() {
    let fixture = Fixture::new();
    fixture.seed_valid(&golden());
    let before = metadata_identity(&fixture.path());
    let mut calls = 0;
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata::<()>(control, CatalogCodecLimits::default(), |model| {
                calls += 1;
                register(model, 2)?;
                eyre::bail!("deliberate pure edit failure");
            })
        })
        .unwrap_err();
    assert!(error.to_string().contains("deliberate pure edit failure"));
    assert_eq!(calls, 1);
    assert_eq!(metadata_identity(&fixture.path()), before);
    assert_eq!(
        fixture
            .independent_disk_model()
            .snapshot(&owner())
            .revision(),
        1
    );
}

#[test]
fn an_encode_budget_failure_after_the_callback_leaves_old_metadata_intact() {
    let fixture = Fixture::new();
    fixture.seed_valid(empty_golden());
    let before = metadata_identity(&fixture.path());
    let mut calls = 0;
    let result = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(
                control,
                CatalogCodecLimits {
                    max_document_bytes: empty_golden().len(),
                    ..Default::default()
                },
                |model| {
                    calls += 1;
                    register(model, 1)?;
                    model.apply_selection(
                        &owner(),
                        &SelectionStamp::default(),
                        Some(&allocation(1)),
                        Uuid::from_u128(101),
                    )?;
                    Ok(17)
                },
            )
        });
    assert_eq!(calls, 1);
    let error = result.unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
    assert_eq!(metadata_identity(&fixture.path()), before);
}

#[test]
fn sequential_edits_in_one_control_session_observe_each_committed_model() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let first = edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                register(model, 1)?;
                model.apply_selection(
                    &owner(),
                    &SelectionStamp::default(),
                    Some(&allocation(1)),
                    Uuid::from_u128(101),
                )?;
                Ok(model.snapshot(&owner()))
            })?;
            assert_eq!(first.revision(), 1);
            let second = edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                let old = model.snapshot(&owner());
                assert_eq!(old, first);
                register(model, 2)?;
                model.apply_selection(
                    &owner(),
                    &old,
                    Some(&allocation(2)),
                    Uuid::from_u128(102),
                )?;
                Ok(model.snapshot(&owner()))
            })?;
            assert_eq!(second.revision(), 2);
            let observed = read_catalog_metadata(control, CatalogCodecLimits::default())?;
            assert_eq!(observed.snapshot(&owner()), second);
            assert_eq!(observed.protecting_references().len(), 2);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fixture
            .independent_disk_model()
            .snapshot(&owner())
            .revision(),
        2
    );
}

#[test]
fn separate_locked_sessions_load_the_latest_selection_and_preserve_tombstones() {
    let fixture = Fixture::new();
    fixture.seed_valid(&golden());
    let old = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            Ok(read_catalog_metadata(control, CatalogCodecLimits::default())?.snapshot(&owner()))
        })
        .unwrap();
    let tombstone = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                assert_eq!(model.snapshot(&owner()), old);
                model.apply_selection(&owner(), &old, None, Uuid::from_u128(102))?;
                Ok(model.snapshot(&owner()))
            })
        })
        .unwrap();
    assert_eq!(tombstone.revision(), 2);
    assert!(tombstone.generation().is_none());
    let before = metadata_identity(&fixture.path());
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let model = read_catalog_metadata(control, CatalogCodecLimits::default())?;
            assert_eq!(model.snapshot(&owner()), tombstone);
            assert_eq!(model.protecting_references().len(), 1);
            let error = edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                model.apply_selection(
                    &owner(),
                    &SelectionStamp::default(),
                    None,
                    Uuid::from_u128(103),
                )
            })
            .unwrap_err();
            assert!(error.to_string().contains("changed since"), "{error:#}");
            Ok(())
        })
        .unwrap();
    assert_eq!(metadata_identity(&fixture.path()), before);
}

#[test]
fn callback_boundary_replacement_cannot_reuse_the_loaded_inode_or_content_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    for replacement in [false, true] {
        let fixture = Fixture::new();
        let original = golden();
        fixture.seed_valid(&original);
        let before = metadata_identity(&fixture.path());
        let expected = if replacement {
            original.clone()
        } else {
            let mut value: Value = serde_json::from_slice(&original).unwrap();
            value["generations"][0]["graph"] = json!("06".repeat(32));
            serde_json::to_vec(&value).unwrap()
        };
        CatalogModel::decode_catalog(&expected, CatalogCodecLimits::default()).unwrap();
        let retained = fixture._root.path().join("retained-original-state");
        let mut calls = 0;
        let error = fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                    calls += 1;
                    assert_eq!(model.snapshot(&owner()).revision(), 1);
                    // Controlled third-party fault at the callback boundary, not
                    // a normal pure-edit action or an arbitrary-uid threat claim.
                    if replacement {
                        fs::rename(fixture.path(), &retained)?;
                    }
                    fs::write(fixture.path(), &expected)?;
                    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o600))?;
                    let current = metadata_identity(&fixture.path());
                    if replacement {
                        assert_ne!((current.0, current.1), (before.0, before.1));
                    } else {
                        assert_eq!((current.0, current.1), (before.0, before.1));
                    }
                    Ok(37)
                })
            })
            .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.to_string().contains("changed since"), "{error:#}");
        assert_eq!(fs::read(fixture.path()).unwrap(), expected);
        if replacement {
            assert_eq!(fs::read(retained).unwrap(), original);
        }
    }
}

#[test]
fn missing_snapshot_remains_exclusive_if_a_file_appears_during_the_callback() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let third_party = golden();
    let mut created = None;
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                register(model, 1)?;
                model.apply_selection(
                    &owner(),
                    &SelectionStamp::default(),
                    Some(&allocation(1)),
                    Uuid::from_u128(101),
                )?;
                fs::write(fixture.path(), &third_party)?;
                fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o600))?;
                created = Some(metadata_identity(&fixture.path()));
                Ok(91)
            })
        })
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists),
        "{error:#}"
    );
    assert_eq!(metadata_identity(&fixture.path()), created.unwrap());
}

#[test]
fn a_tighter_control_write_budget_cannot_be_raised_by_a_larger_codec_budget() {
    let fixture = Fixture::new();
    fixture.seed_valid(empty_golden());
    let before = metadata_identity(&fixture.path());
    let mut calls = 0;
    let error = fixture
        .store
        .with_control(
            ControlLimits {
                max_file_bytes: empty_golden().len() as u64,
                ..Default::default()
            },
            |control| {
                edit_catalog_metadata(control, CatalogCodecLimits::default(), |model| {
                    calls += 1;
                    register(model, 1)?;
                    model.apply_selection(
                        &owner(),
                        &SelectionStamp::default(),
                        Some(&allocation(1)),
                        Uuid::from_u128(101),
                    )?;
                    Ok(1)
                })
            },
        )
        .unwrap_err();
    assert_eq!(calls, 1);
    assert!(error.to_string().contains("byte limit"), "{error:#}");
    assert_eq!(metadata_identity(&fixture.path()), before);
}
