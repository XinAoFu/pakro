use super::*;
use crate::packages::python_resolution::lock_model::*;
use pakro_store::Digest;
use std::fs;
use std::time::Duration;

#[path = "tests/by_id.rs"]
mod by_id;
#[path = "tests/limits.rs"]
mod limits;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn file(base: u8) -> RegistryFileRef {
    RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [base; 32],
            project: "demo",
            version: "1.0",
            filename: "demo-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        LockModelLimits::default(),
    )
    .unwrap()
}
fn open(path: &Path) -> BoundPythonLock {
    BoundPythonLock::acquire(path, LockModelLimits::default(), deadline()).unwrap()
}
fn fresh<'a>(lock: &'a BoundPythonLock, file: &RegistryFileRef) -> FreshFileSlot<'a> {
    match lock.lookup(file, ResolutionIntent::Resolve).unwrap() {
        FilePinLookup::Fresh(slot) => slot,
        other => panic!("expected a fresh slot, got {other:?}"),
    }
}
fn empty() -> &'static str {
    "lockfile_version = 4\n[python]\nschema = 2\npins = []\ngroups = []\n"
}
fn pinned() -> (String, ContentPin) {
    let limits = LockModelLimits::default();
    let pin = ContentPin::new(
        &file(1),
        Digest::Sha256([2; 32]),
        42,
        PinEvidence::Advertised,
        limits,
    )
    .unwrap();
    let key = GroupKey::new("other-project", "development", [3; 32], limits).unwrap();
    let runtime = RuntimePin::new([4; 32], 1, [3; 32]).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo",
    }];
    let nodes = [NodeInput {
        name: "demo",
        version: "1.0",
        source_pin: pin.id(),
    }];
    let graph = LockGraphInput {
        key: &key,
        semantics: LockMarkerSemantics::Current2026,
        runtime,
        root_declarations: &["demo==1.0"],
        roots: &roots,
        nodes: &nodes,
        edges: &[],
    };
    let model = PythonLockModel::from_parts(std::slice::from_ref(&pin), &[graph], limits).unwrap();
    (
        format!(
            "lockfile_version = 4\n{}",
            model.encode_projection(limits).unwrap()
        ),
        pin,
    )
}

#[test]
fn actual_missing_lock_can_authorize_only_an_explicit_fresh_lookup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let lock = open(&path);
    assert!(!path.exists());
    assert!(lock.model().is_none());
    assert!(matches!(
        lock.lookup(&file(1), ResolutionIntent::Locked).unwrap(),
        FilePinLookup::Missing
    ));
    let slot = fresh(&lock, &file(1));
    assert_eq!(slot.file(), &file(1));
    slot.recheck().unwrap();
}

#[test]
fn actual_legacy_native_sections_are_read_without_synthesizing_python_state() {
    for version in [0, 1, 2, 3, 4] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pakro.lock");
        let body = format!("lockfile_version = {version}\n[[tools.python]]\nversion='3.11.17'\n");
        fs::write(&path, &body).unwrap();
        let lock = open(&path);
        assert!(lock.model().is_none());
        fresh(&lock, &file(1)).recheck().unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), body);
    }
}

#[test]
fn an_explicit_empty_python_projection_is_strictly_decoded() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    let lock = open(&path);
    assert_eq!(lock.model().unwrap().pins().len(), 0);
    assert_eq!(lock.model().unwrap().graphs().len(), 0);
    fresh(&lock, &file(1)).recheck().unwrap();
}

#[test]
fn an_existing_exact_file_pin_in_another_group_always_selects_expected_replay() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let (body, pin) = pinned();
    fs::write(&path, body).unwrap();
    let lock = open(&path);
    for intent in [ResolutionIntent::Locked, ResolutionIntent::Resolve] {
        let FilePinLookup::Existing(existing) = lock.lookup(&file(1), intent).unwrap() else {
            panic!("existing pin was bypassed")
        };
        assert_eq!(existing.pin().id(), pin.id());
        assert_eq!(existing.pin().digest(), pin.digest());
        existing.recheck().unwrap();
    }
    fresh(&lock, &file(9)).recheck().unwrap();
}

#[test]
fn equivalent_pep440_spelling_does_not_bypass_an_existing_file_pin() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let (body, _) = pinned();
    fs::write(&path, body).unwrap();
    let lock = open(&path);
    let equivalent = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [1; 32],
            project: "demo",
            version: "1.0.0",
            filename: "demo-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        LockModelLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        lock.lookup(&equivalent, ResolutionIntent::Resolve).unwrap(),
        FilePinLookup::Existing(_)
    ));
}

#[test]
fn in_place_external_edits_revoke_a_previously_borrowed_fresh_slot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    let lock = open(&path);
    let slot = fresh(&lock, &file(1));
    slot.recheck().unwrap();
    fs::write(&path, "lockfile_version = 4\n").unwrap();
    assert!(slot.recheck().is_err());
    assert!(lock.lookup(&file(1), ResolutionIntent::Resolve).is_err());
}

#[test]
fn same_bytes_in_a_replacement_inode_revoke_the_original_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    let lock = open(&path);
    let slot = fresh(&lock, &file(1));
    let replacement = root.path().join("replacement");
    fs::write(&replacement, empty()).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert!(slot.recheck().is_err());
}

#[test]
fn a_missing_snapshot_is_revoked_if_an_external_file_appears() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let lock = open(&path);
    let slot = fresh(&lock, &file(1));
    fs::write(&path, empty()).unwrap();
    assert!(slot.recheck().is_err());
}

#[test]
fn invalid_root_or_python_data_never_turns_into_a_fresh_empty_model() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    drop(open(&path));
    for body in [
        "lockfile_version = 5\n",
        "lockfile_version = -1\n",
        "lockfile_version = 'private-secret'\n",
        "lockfile_version = 3\n[python]\nschema=2\npins=[]\ngroups=[]\n",
        "lockfile_version = 4\npython='private-secret'\n",
        "lockfile_version = 4\n[python]\nschema=99\npins=[]\ngroups=[]\n",
        "lockfile_version = 4\n[python]\nschema=2\npins=[]\ngroups=[]\nunknown='private-secret'\n",
        "lockfile_version=4\nlockfile_version=4\n",
        "lockfile_version=4\n[other]\nbad='private-secret' junk\n",
    ] {
        fs::write(&path, body).unwrap();
        let error =
            BoundPythonLock::acquire(&path, LockModelLimits::default(), deadline()).unwrap_err();
        assert!(!format!("{error:#}").contains("private-secret"));
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
    }
}

#[test]
fn actual_raw_document_bytes_are_bounded_before_root_deserialization() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    drop(open(&path));
    let exact = LockModelLimits {
        max_document_bytes: empty().len(),
        ..Default::default()
    };
    drop(BoundPythonLock::acquire(&path, exact, deadline()).unwrap());
    let tight = LockModelLimits {
        max_document_bytes: empty().len() - 1,
        ..Default::default()
    };
    assert!(BoundPythonLock::acquire(&path, tight, deadline()).is_err());
    let zero = LockModelLimits {
        max_document_bytes: 0,
        ..Default::default()
    };
    assert!(BoundPythonLock::acquire(&path, zero, deadline()).is_err());
}

#[test]
fn conflicting_expected_pins_for_the_same_file_never_grant_a_fresh_slot() {
    let limits = LockModelLimits::default();
    let first = ContentPin::new(
        &file(1),
        Digest::Sha256([2; 32]),
        42,
        PinEvidence::Advertised,
        limits,
    )
    .unwrap();
    let second = ContentPin::new(
        &file(1),
        Digest::Sha256([3; 32]),
        43,
        PinEvidence::ObservedDuringFreshResolution,
        limits,
    )
    .unwrap();
    let keys = [
        GroupKey::new("", "main", [4; 32], limits).unwrap(),
        GroupKey::new("", "dev", [4; 32], limits).unwrap(),
    ];
    let runtime = RuntimePin::new([5; 32], 1, [4; 32]).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo",
    }];
    let first_nodes = [NodeInput {
        name: "demo",
        version: "1.0",
        source_pin: first.id(),
    }];
    let second_nodes = [NodeInput {
        name: "demo",
        version: "1.0",
        source_pin: second.id(),
    }];
    let graphs = [
        LockGraphInput {
            key: &keys[0],
            semantics: LockMarkerSemantics::Current2026,
            runtime,
            root_declarations: &["demo==1.0"],
            roots: &roots,
            nodes: &first_nodes,
            edges: &[],
        },
        LockGraphInput {
            key: &keys[1],
            semantics: LockMarkerSemantics::Current2026,
            runtime,
            root_declarations: &["demo==1.0"],
            roots: &roots,
            nodes: &second_nodes,
            edges: &[],
        },
    ];
    let model = PythonLockModel::from_parts(&[first, second], &graphs, limits).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(
        &path,
        format!(
            "lockfile_version=4\n{}",
            model.encode_projection(limits).unwrap()
        ),
    )
    .unwrap();
    let lock = open(&path);
    for intent in [ResolutionIntent::Locked, ResolutionIntent::Resolve] {
        assert!(
            lock.lookup(&file(1), intent)
                .unwrap_err()
                .to_string()
                .contains("conflicting pins")
        );
    }
}

#[test]
fn limits_unrelated_to_document_size_are_validated_before_a_missing_lock_guard_is_created() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let limits = LockModelLimits {
        max_pins: 0,
        ..Default::default()
    };
    assert!(BoundPythonLock::acquire(&path, limits, deadline()).is_err());
    assert!(!root.path().join(".pakro.lock.guard").exists());
    assert!(!path.exists());
}

#[test]
fn a_fresh_query_must_fit_this_actual_lock_sessions_identity_limits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let limits = LockModelLimits {
        max_field_bytes: 3,
        ..Default::default()
    };
    let lock = BoundPythonLock::acquire(&path, limits, deadline()).unwrap();
    assert!(lock.lookup(&file(1), ResolutionIntent::Resolve).is_err());
    assert!(!path.exists());
}

#[test]
fn an_existing_pin_owner_also_loses_authority_after_an_external_lock_edit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let (body, _) = pinned();
    fs::write(&path, body).unwrap();
    let lock = open(&path);
    let FilePinLookup::Existing(slot) = lock.lookup(&file(1), ResolutionIntent::Locked).unwrap()
    else {
        panic!("expected actual existing pin")
    };
    slot.recheck().unwrap();
    fs::write(&path, empty()).unwrap();
    assert!(slot.recheck().is_err());
}

#[test]
fn a_fresh_file_slot_from_another_actual_project_cannot_authorize_this_depot() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let a = open(&first.path().join("pakro.lock"));
    let b = open(&second.path().join("pakro.lock"));
    let slot = fresh(&a, &file(1));
    slot.recheck_for(&a).unwrap();
    assert!(slot.recheck_for(&b).is_err());
    slot.recheck_for(&a).unwrap();
}

#[test]
fn an_existing_file_slot_does_not_rebind_to_identical_lock_bytes_in_another_project() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (body, _) = pinned();
    let first_path = first.path().join("pakro.lock");
    let second_path = second.path().join("pakro.lock");
    fs::write(&first_path, &body).unwrap();
    fs::write(&second_path, &body).unwrap();
    let a = open(&first_path);
    let b = open(&second_path);
    let FilePinLookup::Existing(slot) = a.lookup(&file(1), ResolutionIntent::Locked).unwrap()
    else {
        panic!("actual existing pin required")
    };
    slot.recheck_for(&a).unwrap();
    assert!(slot.recheck_for(&b).is_err());
    slot.recheck_for(&a).unwrap();
}

#[test]
fn fixed_lock_install_bind_fixed_decodes_initial_bytes_not_disk_content() {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("pakro.lock");
    let (initial, _) = pinned();
    // A different, valid document on disk must not replace the initial bytes:
    // the fixed model keeps the initial pin set.
    fs::write(&path, empty()).unwrap();
    let lock = BoundPythonLock::bind_fixed(
        &path,
        Some(initial.as_bytes()),
        LockModelLimits::default(),
        deadline(),
    )
    .unwrap();
    let model = lock.model().expect("initial bytes carry a model");
    assert_eq!(model.pins().len(), 1);
    lock.recheck().unwrap();
    // Garbage on disk, deletion and recreation never become install input or
    // a recheck failure for a fixed origin.
    fs::write(&path, "user rewrote the lock").unwrap();
    lock.recheck().unwrap();
    fs::remove_file(&path).unwrap();
    lock.recheck().unwrap();
    fs::write(&path, empty()).unwrap();
    lock.recheck().unwrap();
    drop(lock);
    let absent =
        BoundPythonLock::bind_fixed(&path, None, LockModelLimits::default(), deadline()).unwrap();
    assert!(absent.model().is_none());
    absent.recheck().unwrap();
}

#[test]
fn fixed_lock_install_fixed_input_holds_the_real_guard_and_real_checks() {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    // An expired deadline refuses construction: the fixed origin still runs
    // the session's real deadline and binding checks.
    assert!(
        BoundPythonLock::bind_fixed(
            &path,
            Some(empty().as_bytes()),
            LockModelLimits::default(),
            Instant::now(),
        )
        .is_err()
    );
    let lock = BoundPythonLock::bind_fixed(
        &path,
        Some(empty().as_bytes()),
        LockModelLimits::default(),
        deadline(),
    )
    .unwrap();
    // The exclusive parent guard is really held for the whole installation
    // window: a second session on the same parent cannot take it.
    let short = Instant::now() + Duration::from_millis(150);
    assert!(
        ProjectLockSession::acquire(&path, LockModelLimits::default().max_document_bytes, short)
            .is_err()
    );
    drop(lock);
    ProjectLockSession::acquire(
        &path,
        LockModelLimits::default().max_document_bytes,
        deadline(),
    )
    .unwrap();
}

#[test]
fn fixed_lock_install_acquired_origin_still_rejects_stale_disk_image() {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("pakro.lock");
    fs::write(&path, empty()).unwrap();
    let lock = open(&path);
    fs::write(
        &path,
        "lockfile_version = 4\n[python]\nschema = 2\npins = []\ngroups = []\n# changed\n",
    )
    .unwrap();
    assert!(lock.recheck().is_err());
}
