//! Only the actual root snapshot can mint an exact-ID existing-file borrow.
use super::*;

fn two_pins(second_digest: Digest, second_size: u64) -> (String, [ContentPin; 2]) {
    let limits = LockModelLimits::default();
    let first_file = file(1);
    let second_file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: first_file.registry_id(),
            canonical_base_fingerprint: *first_file.canonical_base_fingerprint(),
            project: first_file.project(),
            version: "1.0.0",
            filename: first_file.filename(),
            kind: first_file.kind(),
        },
        limits,
    )
    .unwrap();
    let pins = [
        ContentPin::new(
            &first_file,
            Digest::Sha256([2; 32]),
            42,
            PinEvidence::Advertised,
            limits,
        )
        .unwrap(),
        ContentPin::new(
            &second_file,
            second_digest,
            second_size,
            PinEvidence::Advertised,
            limits,
        )
        .unwrap(),
    ];
    assert_ne!(
        pins[0].id(),
        pins[1].id(),
        "exact version spelling remains in each portable ID"
    );
    let keys = [
        GroupKey::new("", "main", [3; 32], limits).unwrap(),
        GroupKey::new("", "dev", [3; 32], limits).unwrap(),
    ];
    let runtime = RuntimePin::new([4; 32], 1, [3; 32]).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo",
    }];
    let first_nodes = [NodeInput {
        name: "demo",
        version: "1.0",
        source_pin: pins[0].id(),
    }];
    let second_nodes = [NodeInput {
        name: "demo",
        version: "1.0.0",
        source_pin: pins[1].id(),
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
            root_declarations: &["demo==1.0.0"],
            roots: &roots,
            nodes: &second_nodes,
            edges: &[],
        },
    ];
    let model = PythonLockModel::from_parts(&pins, &graphs, limits).unwrap();
    let body = format!(
        "lockfile_version = 4\n{}",
        model.encode_projection(limits).unwrap()
    );
    // A real root reader, not an external model or detached pin, owns the test
    // proof below. This only checks the fixture's portable bytes beforehand.
    PythonLockModel::decode_projection(body.split_once('\n').unwrap().1, limits).unwrap();
    (body, pins)
}

#[test]
fn an_exact_id_is_borrowed_from_the_actual_pinned_root_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    let (body, pin) = pinned();
    fs::write(&path, &body).unwrap();
    let root = open(&path);
    let FilePinLookup::Existing(control) =
        root.lookup(pin.file(), ResolutionIntent::Locked).unwrap()
    else {
        panic!("actual file lookup prelude");
    };
    assert_eq!(control.pin().id(), pin.id());
    let slot = root
        .existing_pin(pin.id())
        .unwrap()
        .expect("the actual old pin ID must exist");
    assert_eq!(slot.pin().id(), pin.id());
    assert_eq!(slot.pin().digest(), pin.digest());
    assert_eq!(slot.pin().size(), pin.size());
    slot.recheck_for(&root).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
}

#[test]
fn absent_id_or_python_projection_never_issues_a_fresh_slot() {
    for body in [None, Some("lockfile_version=4\n"), Some(empty())] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pakro.lock");
        if let Some(body) = body {
            fs::write(&path, body).unwrap();
        }
        let root = open(&path);
        assert!(
            root.existing_pin(PinId::from_bytes([0xee; 32]))
                .unwrap()
                .is_none()
        );
        root.recheck().unwrap();
        assert_eq!(path.exists(), body.is_some());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    let (body, _) = pinned();
    fs::write(&path, &body).unwrap();
    let root = open(&path);
    assert!(
        root.existing_pin(PinId::from_bytes([0xee; 32]))
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
}

#[test]
fn equivalent_file_spellings_with_equal_expected_bytes_return_each_requested_id() {
    let (body, pins) = two_pins(Digest::Sha256([2; 32]), 42);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    fs::write(&path, &body).unwrap();
    let root = open(&path);
    // Both ordinary lookups are valid but may return the same first spelling.
    for pin in &pins {
        assert!(matches!(
            root.lookup(pin.file(), ResolutionIntent::Locked).unwrap(),
            FilePinLookup::Existing(_)
        ));
    }
    for pin in &pins {
        let slot = root
            .existing_pin(pin.id())
            .unwrap()
            .expect("requested exact ID must not be lost to semantic file lookup order");
        assert_eq!(slot.pin().id(), pin.id());
        assert_eq!(slot.pin().file().version(), pin.file().version());
        slot.recheck_for(&root).unwrap();
    }
}

#[test]
fn globally_conflicting_expected_bytes_and_changed_actual_image_are_rejected() {
    for (digest, size) in [(Digest::Sha256([9; 32]), 42), (Digest::Sha256([2; 32]), 43)] {
        let (body, pins) = two_pins(digest, size);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pakro.lock");
        fs::write(&path, &body).unwrap();
        let root = open(&path);
        assert!(
            root.lookup(pins[0].file(), ResolutionIntent::Locked)
                .unwrap_err()
                .to_string()
                .contains("conflicting pins")
        );
        let error = root.existing_pin(pins[0].id()).unwrap_err();
        assert!(error.to_string().contains("conflicting pins"));
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    let (body, pin) = pinned();
    fs::write(&path, &body).unwrap();
    let root = open(&path);
    fs::write(
        &path,
        "lockfile_version=4\n# external private replacement\n",
    )
    .unwrap();
    assert!(root.existing_pin(pin.id()).is_err());
    assert!(
        root.existing_pin(PinId::from_bytes([0xee; 32])).is_err(),
        "even a missing ID must check the actual image first"
    );
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("external private replacement")
    );
}
