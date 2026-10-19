use super::*;

#[test]
fn current_byte_budget_checks_the_actual_original_wire_including_native_only_comments() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    let body = "lockfile_version=4\n# original-private-wire-padding\n";
    fs::write(&path, body).unwrap();
    let root = open(&path);
    assert!(root.model().is_none());
    root.check_limits(LockModelLimits {
        max_document_bytes: body.len(),
        ..LockModelLimits::default()
    })
    .unwrap();
    let error = root
        .check_limits(LockModelLimits {
            max_document_bytes: body.len() - 1,
            ..LockModelLimits::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("byte budget"));
    assert!(!format!("{error:?}").contains("original-private-wire-padding"));
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
}

#[test]
fn current_model_limits_are_rechecked_on_the_same_real_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.lock");
    let (body, pin) = pinned();
    fs::write(&path, &body).unwrap();
    let root = open(&path);
    let slot = root
        .existing_pin(pin.id())
        .unwrap()
        .expect("actual exact-ID prerequisite");
    assert_eq!(slot.pin().size(), 42);
    root.check_limits(LockModelLimits {
        max_artifact_bytes: 42,
        ..LockModelLimits::default()
    })
    .unwrap();
    let error = root
        .check_limits(LockModelLimits {
            max_artifact_bytes: 41,
            ..LockModelLimits::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("ArtifactBytes"));
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
}

#[test]
fn missing_or_python_absent_locks_still_validate_limits_and_recheck_the_actual_image() {
    for body in [None, Some("lockfile_version=4\n")] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pakro.lock");
        if let Some(body) = body {
            fs::write(&path, body).unwrap();
        }
        let root = open(&path);
        root.check_limits(LockModelLimits::default()).unwrap();
        assert!(
            root.check_limits(LockModelLimits {
                max_pins: 0,
                ..LockModelLimits::default()
            })
            .is_err()
        );
        let tiny_wire = LockModelLimits {
            max_document_bytes: 1,
            ..LockModelLimits::default()
        };
        if body.is_some() {
            assert!(root.check_limits(tiny_wire).is_err());
        } else {
            root.check_limits(tiny_wire).unwrap();
        }
        fs::write(&path, "lockfile_version=4\n# external replacement\n").unwrap();
        assert!(root.check_limits(LockModelLimits::default()).is_err());
    }
}
