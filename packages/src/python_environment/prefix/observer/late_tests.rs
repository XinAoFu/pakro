use super::cleanup::CleanupCheckpoint;
use super::tests::Fixture;
use super::*;
use crate::packages::python_environment::prefix::control::ownership::{
    OwnershipLog,
    events::{OwnershipRecoveryPhase, validate_ownership_events},
};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn fixture() -> Fixture {
    let mut fixture = Fixture::with_limits(PrefixLimits::default());
    fixture.attach(OwnershipObserverLimits::default()).unwrap();
    fixture.prefix.create_dir_all(Path::new("lib")).unwrap();
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("lib/file"), 0o644)
        .unwrap();
    file.write_all(b"owned bytes").unwrap();
    file.finish().unwrap();
    assert_eq!(log(&fixture).records().len(), 5);
    fixture
}
fn log(fixture: &Fixture) -> OwnershipLog {
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.read_ownership_stream(
                &fixture.journal,
                JournalLimits::default(),
                OwnershipStreamLimits::default(),
            )
        })
        .unwrap()
        .unwrap()
}
fn limits(fixture: &Fixture) -> OwnershipEventLimits {
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
fn captured(store: &Path) -> PathBuf {
    let entries: Vec<_> = fs::read_dir(store.join(".quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "expected one actual captured root");
    entries.into_iter().next().unwrap()
}

#[test]
fn changed_root_actually_captured_in_the_kernel_race_is_restored_without_deletion() {
    let mut fixture = fixture();
    let root = fixture.prefix.final_path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    let original = outside.path().join("original");
    let mut reached = false;
    let error = fixture
        .prefix
        .discard_observed_with_hook(|checkpoint| {
            if matches!(checkpoint, CleanupCheckpoint::BeforeCapture) {
                reached = true;
                fs::rename(&root, &original)?;
                fs::create_dir(&root)?;
                fs::write(root.join("foreign"), b"foreign bytes")?;
            }
            Ok(())
        })
        .unwrap_err();
    assert!(reached, "actual capture hook was not reached");
    assert!(
        root.exists(),
        "changed candidate was captured but never restored: {error:?}"
    );
    assert_eq!(fs::read(root.join("foreign")).unwrap(), b"foreign bytes");
    assert_eq!(fs::read(original.join("lib/file")).unwrap(), b"owned bytes");
    assert_eq!(
        fs::read_dir(fixture.store.root_path().join(".quarantine"))
            .unwrap()
            .count(),
        0
    );
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.phase(), OwnershipRecoveryPhase::CapturePending);
    assert!(!state.capture().unwrap().captured());
}

#[test]
fn changed_capture_restoration_never_clobbers_a_new_final_root() {
    let mut fixture = fixture();
    let root = fixture.prefix.final_path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    let original = outside.path().join("original");
    let mut captured_hook = false;
    let error = fixture
        .prefix
        .discard_observed_with_hook(|checkpoint| {
            match checkpoint {
                CleanupCheckpoint::BeforeCapture => {
                    fs::rename(&root, &original)?;
                    fs::create_dir(&root)?;
                    fs::write(root.join("first-foreign"), b"first")?;
                }
                CleanupCheckpoint::Captured => {
                    captured_hook = true;
                    fs::create_dir(&root)?;
                    fs::write(root.join("second-foreign"), b"second")?;
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap_err();
    assert!(captured_hook, "real capture was not reached: {error:?}");
    assert_eq!(fs::read(root.join("second-foreign")).unwrap(), b"second");
    assert_eq!(
        fs::read(captured(fixture.store.root_path()).join("first-foreign")).unwrap(),
        b"first"
    );
    assert_eq!(fs::read(original.join("lib/file")).unwrap(), b"owned bytes");
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
}

#[test]
fn capture_completed_on_disk_but_log_failure_keeps_capture_pending_and_retains_root() {
    let mut fixture = fixture();
    let root = fixture.prefix.final_path().to_path_buf();
    let stream = fixture.stream_path();
    let mut reached = false;
    assert!(
        fixture
            .prefix
            .discard_observed_with_hook(|checkpoint| {
                if matches!(checkpoint, CleanupCheckpoint::Captured) {
                    reached = true;
                    fs::set_permissions(&stream, fs::Permissions::from_mode(0o644))?;
                }
                Ok(())
            })
            .is_err()
    );
    assert!(reached);
    assert!(!root.exists());
    fs::set_permissions(&stream, fs::Permissions::from_mode(0o600)).unwrap();
    let recovered = captured(fixture.store.root_path());
    assert_eq!(
        fs::read(recovered.join("lib/file")).unwrap(),
        b"owned bytes"
    );
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.phase(), OwnershipRecoveryPhase::CapturePending);
    assert!(!state.capture().unwrap().captured());
    drop(fixture.prefix);
    assert!(recovered.exists());
}

#[test]
fn real_unlink_permission_error_does_not_append_removed_or_continue_cleanup() {
    let mut fixture = fixture();
    let store = fixture.store.root_path().to_path_buf();
    let mut reached = false;
    let mut parent = None;
    let result = fixture.prefix.discard_observed_with_hook(|checkpoint| {
        if let CleanupCheckpoint::BeforeUnlink(relative) = checkpoint
            && relative == Path::new("lib/file")
        {
            reached = true;
            let path = captured(&store).join("lib");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
            parent = Some(path);
        }
        Ok(())
    });
    assert!(reached);
    let parent = parent.unwrap();
    if nix::unistd::geteuid().is_root() {
        // Root may legitimately unlink through 0500. That execution is success
        // coverage, not evidence of a kernel EACCES failure.
        result.unwrap();
        return;
    }
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<nix::errno::Errno>(),
        Some(&nix::errno::Errno::EACCES),
        "{error:?}"
    );
    assert_eq!(fs::read(parent.join("file")).unwrap(), b"owned bytes");
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.pending_remove().unwrap().relative(), b"lib/file");
    assert!(state.entries().iter().all(|entry| !entry.removed()));
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
}

#[test]
fn actual_unlink_before_failed_removed_log_preserves_exact_pending_progress() {
    let mut fixture = fixture();
    let store = fixture.store.root_path().to_path_buf();
    let stream = fixture.stream_path();
    let mut reached = false;
    assert!(
        fixture
            .prefix
            .discard_observed_with_hook(|checkpoint| {
                if let CleanupCheckpoint::Unlinked(relative) = checkpoint
                    && relative == Path::new("lib/file")
                {
                    reached = true;
                    fs::set_permissions(&stream, fs::Permissions::from_mode(0o644))?;
                }
                Ok(())
            })
            .is_err()
    );
    assert!(reached);
    fs::set_permissions(&stream, fs::Permissions::from_mode(0o600)).unwrap();
    let retained = captured(&store);
    assert!(!retained.join("lib/file").exists());
    assert!(retained.join("lib").is_dir());
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.pending_remove().unwrap().relative(), b"lib/file");
    assert!(state.entries().iter().all(|entry| !entry.removed()));
    drop(fixture.prefix);
    assert!(retained.join("lib").is_dir());
}

#[test]
fn root_unlink_before_failed_root_removed_log_has_only_durable_root_ready_facts() {
    let mut fixture = fixture();
    let root = fixture.prefix.final_path().to_path_buf();
    let store = fixture.store.root_path().to_path_buf();
    let stream = fixture.stream_path();
    let mut reached = false;
    assert!(
        fixture
            .prefix
            .discard_observed_with_hook(|checkpoint| {
                if matches!(checkpoint, CleanupCheckpoint::RootUnlinked) {
                    reached = true;
                    fs::set_permissions(&stream, fs::Permissions::from_mode(0o644))?;
                }
                Ok(())
            })
            .is_err()
    );
    assert!(reached);
    fs::set_permissions(&stream, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!root.exists());
    assert_eq!(fs::read_dir(store.join(".quarantine")).unwrap().count(), 0);
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.phase(), OwnershipRecoveryPhase::Captured);
    assert!(state.entries().iter().all(|entry| entry.removed()));
    assert!(state.root_removed_sequence().is_none());
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
}

#[test]
fn hardlink_added_after_remove_intent_is_rechecked_before_actual_unlink() {
    let mut fixture = fixture();
    let store = fixture.store.root_path().to_path_buf();
    let outside = tempfile::tempdir().unwrap();
    let linked = outside.path().join("linked");
    let mut reached = false;
    let error = fixture
        .prefix
        .discard_observed_with_hook(|checkpoint| {
            if let CleanupCheckpoint::BeforeUnlink(relative) = checkpoint
                && relative == Path::new("lib/file")
            {
                reached = true;
                fs::hard_link(captured(&store).join(relative), &linked)?;
            }
            Ok(())
        })
        .unwrap_err();
    assert!(reached);
    assert!(format!("{error:?}").contains("hard link"), "{error:?}");
    let file = captured(&store).join("lib/file");
    assert_eq!(fs::metadata(&file).unwrap().nlink(), 2);
    assert_eq!(fs::read(file).unwrap(), b"owned bytes");
    assert_eq!(fs::read(linked).unwrap(), b"owned bytes");
    let log = log(&fixture);
    let state = validate_ownership_events(&log, limits(&fixture)).unwrap();
    assert_eq!(state.pending_remove().unwrap().relative(), b"lib/file");
}
