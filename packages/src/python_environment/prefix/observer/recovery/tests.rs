use super::*;
use crate::packages::python_environment::catalog::edit_catalog_metadata;
use crate::packages::python_environment::journal::{
    JournalPrepare, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    GenerationRecord, InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use crate::packages::python_environment::prefix::control::ownership::{
    OwnershipLog,
    events::{OwnershipRecoveryPhase, OwnershipRecoveryState, validate_ownership_events},
};
use crate::packages::python_environment::prefix::{
    OwnershipRecoveryLimits, OwnershipRecoveryOutcome,
};
use pakro_store::Digest;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

const CHILD: &str = "packages::python_environment::prefix::observer::recovery::tests::crash_child";
const ROOT_ENV: &str = "PAKRO_TEST_OWNERSHIP_RESTART_ROOT";
const PHASE_ENV: &str = "PAKRO_TEST_OWNERSHIP_RESTART_PHASE";
const EXIT: i32 = 71;

#[path = "guards.rs"]
mod guards;

fn setup(root: &Path) -> (EnvironmentStore, UnpublishedPrefix, JournalRecord) {
    let store = EnvironmentStore::open(
        root,
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    let key = SelectionKey::new(
        Uuid::from_u128(1),
        "restart 主组",
        [3; 32],
        ModelLimits::default(),
    )
    .unwrap();
    let inputs = [InputReference::new(
        InputKind::Runtime,
        Digest::Sha256([7; 32]),
    )];
    let journal = store
        .with_control(ControlLimits::default(), |control| {
            let journal = prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: Uuid::from_u128(101),
                    key: &key,
                    expected: &SelectionStamp::default(),
                    target: JournalTarget::Generation {
                        identity: &identity,
                        graph: [4; 32],
                        inputs: &inputs,
                    },
                },
                JournalLimits::default(),
            )?;
            control.attach_prefix_ownership(
                &mut prefix,
                &journal,
                JournalLimits::default(),
                OwnershipObserverLimits::default(),
            )?;
            Ok(journal)
        })
        .unwrap();
    (store, prefix, journal)
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        return;
    };
    let phase = std::env::var(PHASE_ENV).unwrap();
    if phase == "resume-unlinked" {
        let store = EnvironmentStore::open(
            Path::new(&root),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let journals = store
            .with_control(ControlLimits::default(), |control| {
                read_journal_metadata(control, JournalLimits::default())
            })
            .unwrap();
        assert_eq!(journals.len(), 1);
        store
            .with_control(ControlLimits::default(), |control| {
                control.recover_with_progress(
                    &journals[0],
                    OwnershipRecoveryLimits::default(),
                    |checkpoint| {
                        if let super::super::cleanup::CleanupCheckpoint::Unlinked(relative) =
                            checkpoint
                            && relative == Path::new("lib")
                        {
                            std::process::exit(EXIT)
                        }
                        Ok(())
                    },
                )?;
                Ok(())
            })
            .unwrap();
        panic!("fresh recovery interruption checkpoint was not reached");
    }
    let (_store, mut prefix, _journal) = setup(Path::new(&root));
    if phase == "header" {
        std::process::exit(EXIT)
    }
    if matches!(phase.as_str(), "intent" | "actual") {
        let _writer = prefix
            .create_file_with_checkpoint(Path::new("gap"), 0o644, |checkpoint| {
                if matches!(
                    (phase.as_str(), checkpoint),
                    ("intent", CreationCheckpoint::Intent)
                        | ("actual", CreationCheckpoint::CreatedActual)
                ) {
                    std::process::exit(EXIT)
                }
                Ok(())
            })
            .unwrap();
        panic!("creation crash checkpoint was not reached")
    }
    prefix.create_dir_all(Path::new("lib")).unwrap();
    let mut file = prefix
        .create_file_exclusive(Path::new("lib/file"), 0o644)
        .unwrap();
    file.write_all(b"restart owned bytes").unwrap();
    if phase == "unfinished" {
        std::process::exit(EXIT)
    }
    file.finish().unwrap();
    if phase == "finished" {
        std::process::exit(EXIT)
    }
    prefix
        .discard_observed_with_hook(|checkpoint| {
            use super::super::cleanup::CleanupCheckpoint as Checkpoint;
            if matches!(
                (phase.as_str(), &checkpoint),
                ("capture-pending", Checkpoint::Captured)
                    | ("captured", Checkpoint::CapturedDurable)
                    | ("root-unlinked", Checkpoint::RootUnlinked)
                    | ("complete", Checkpoint::Complete)
            ) {
                std::process::exit(EXIT)
            }
            match checkpoint {
                Checkpoint::Unlinked(relative)
                    if phase == "unlinked" && relative == Path::new("lib/file") =>
                {
                    std::process::exit(EXIT)
                }
                Checkpoint::BeforeUnlink(relative)
                    if phase == "removed" && relative == Path::new("lib") =>
                {
                    std::process::exit(EXIT)
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();
    panic!("cleanup crash checkpoint was not reached")
}

fn run_child(path: &Path, phase: &str) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env(ROOT_ENV, path)
        .env(PHASE_ENV, phase)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("actual independent child prerequisite");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "child checkpoint timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        status.code(),
        Some(EXIT),
        "child setup/checkpoint failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Restart {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
    journal: JournalRecord,
    original_journal: Vec<u8>,
}
impl Restart {
    fn child(phase: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("environments");
        run_child(&path, phase);
        let store = EnvironmentStore::open(
            &path,
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let journal = store
            .with_control(ControlLimits::default(), |control| {
                let records = read_journal_metadata(control, JournalLimits::default())?;
                assert_eq!(records.len(), 1, "fresh durable journal prerequisite");
                let journal = records.into_iter().next().unwrap();
                Ok(journal)
            })
            .unwrap();
        store
            .with_control(ControlLimits::default(), |control| {
                let held = control
                    .try_cleanup_lease(journal.generation().unwrap().identity())?
                    .expect("child exited and released actual kernel build EX");
                held.check_binding()?;
                Ok(())
            })
            .unwrap();
        let original_journal = fs::read(
            store
                .root_path()
                .join(".control/journals")
                .join(format!("{}.json", journal.nonce().simple())),
        )
        .unwrap();
        let fixture = Self {
            _root: root,
            store,
            journal,
            original_journal,
        };
        fixture.state(); // Actual A/raw/typed/lease setup, independent of recovery.
        fixture
    }
    fn log(&self) -> OwnershipLog {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.read_ownership_stream(
                    &self.journal,
                    JournalLimits::default(),
                    OwnershipStreamLimits::default(),
                )
            })
            .unwrap()
            .unwrap()
    }
    fn state(&self) -> OwnershipRecoveryState {
        let log = self.log();
        let allocation = &self.journal.generation().unwrap().identity().allocation;
        validate_ownership_events(
            &log,
            OwnershipEventLimits {
                final_prefix_bytes: self
                    .store
                    .root_path()
                    .join(allocation)
                    .as_os_str()
                    .as_encoded_bytes()
                    .len(),
                ..OwnershipEventLimits::default()
            },
        )
        .unwrap()
    }
    fn recover(&self) -> Result<OwnershipRecoveryOutcome> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control
                    .recover_uncommitted_prefix(&self.journal, OwnershipRecoveryLimits::default())
            })
    }
    fn assert_cleaned(&self, outcome: OwnershipRecoveryOutcome) {
        let OwnershipRecoveryOutcome::Cleaned(receipt) = outcome else {
            panic!("fresh recovery did not actually clean the recorded root: {outcome:?}")
        };
        assert_eq!(receipt.nonce(), self.journal.nonce());
        assert_eq!(
            receipt.identity(),
            self.journal.generation().unwrap().identity()
        );
        let log = self.log();
        assert_eq!(receipt.stream_head(), log.head());
        assert_eq!(receipt.stream_bytes(), log.total_bytes());
        assert_eq!(self.state().phase(), OwnershipRecoveryPhase::Complete);
        let allocation = &receipt.identity().allocation;
        assert!(!self.store.root_path().join(allocation).exists());
        match fs::read_dir(self.store.root_path().join(".quarantine")) {
            Ok(entries) => assert_eq!(entries.count(), 0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("quarantine inspection failed: {error}"),
        }
        let lock = fs::metadata(
            self.store
                .root_path()
                .join(".leases/build")
                .join(format!("{allocation}.lock")),
        )
        .unwrap();
        assert_eq!([lock.dev(), lock.ino()], receipt.identity().lock);
        assert_eq!(
            fs::read(
                self.store
                    .root_path()
                    .join(".control/journals")
                    .join(format!("{}.json", self.journal.nonce().simple()))
            )
            .unwrap(),
            self.original_journal
        );
    }
}

#[test]
fn header_only_child_exit_recovers_an_empty_real_generation() {
    let fixture = Restart::child("header");
    assert!(fixture.state().entries().is_empty());
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn finished_child_tree_is_reloaded_and_cleaned_without_old_prefix_pins() {
    let fixture = Restart::child("finished");
    assert_eq!(fixture.state().entries().len(), 2);
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn created_unfinished_child_file_uses_actual_known_inode_without_false_finish() {
    let fixture = Restart::child("unfinished");
    assert!(
        fixture
            .state()
            .pending_create()
            .unwrap()
            .created()
            .is_some()
    );
    fixture.assert_cleaned(fixture.recover().unwrap());
    assert!(
        fixture
            .state()
            .entries()
            .iter()
            .find(|entry| entry.kind() == OwnershipEntryKind::File)
            .unwrap()
            .finished()
            .is_none()
    );
}
#[test]
fn intent_without_kernel_create_is_uncertain_even_when_its_path_is_missing() {
    let fixture = Restart::child("intent");
    assert!(fixture.state().requires_retention());
    let root = fixture
        .store
        .root_path()
        .join(&fixture.journal.generation().unwrap().identity().allocation);
    assert!(!root.join("gap").exists());
    assert!(matches!(
        fixture.recover().unwrap(),
        OwnershipRecoveryOutcome::RetainedUncertain
    ));
    assert!(root.is_dir());
}
#[test]
fn actual_create_before_created_log_is_uncertain_and_is_never_adopted_by_name() {
    let fixture = Restart::child("actual");
    assert!(fixture.state().requires_retention());
    let root = fixture
        .store
        .root_path()
        .join(&fixture.journal.generation().unwrap().identity().allocation);
    assert!(root.join("gap").is_file());
    assert!(matches!(
        fixture.recover().unwrap(),
        OwnershipRecoveryOutcome::RetainedUncertain
    ));
    assert!(root.join("gap").is_file());
}
#[test]
fn actual_capture_before_captured_log_is_resumed_from_the_same_q_inode() {
    let fixture = Restart::child("capture-pending");
    assert_eq!(
        fixture.state().phase(),
        OwnershipRecoveryPhase::CapturePending
    );
    assert!(!fixture.state().capture().unwrap().captured());
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn durably_captured_child_root_is_resumed_from_quarantine() {
    let fixture = Restart::child("captured");
    assert_eq!(fixture.state().phase(), OwnershipRecoveryPhase::Captured);
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn actual_unlink_before_removed_log_allows_only_its_exact_missing_path() {
    let fixture = Restart::child("unlinked");
    assert_eq!(
        fixture.state().pending_remove().unwrap().relative(),
        b"lib/file"
    );
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn actual_root_unlink_before_root_removed_requires_durable_root_ready_progress() {
    let fixture = Restart::child("root-unlinked");
    assert_eq!(fixture.state().phase(), OwnershipRecoveryPhase::Captured);
    assert!(
        fixture
            .state()
            .entries()
            .iter()
            .all(|entry| entry.removed())
    );
    assert!(fixture.state().root_removed_sequence().is_none());
    fixture.assert_cleaned(fixture.recover().unwrap());
}
#[test]
fn durable_complete_is_freshly_idempotent_without_new_frames_or_root_recreation() {
    let fixture = Restart::child("complete");
    assert_eq!(fixture.state().phase(), OwnershipRecoveryPhase::Complete);
    let before = fixture.log().total_bytes();
    fixture.assert_cleaned(fixture.recover().unwrap());
    fixture.assert_cleaned(fixture.recover().unwrap());
    assert_eq!(fixture.log().total_bytes(), before);
}

#[test]
fn a_second_recovery_process_can_crash_after_unlink_and_a_third_session_resumes_exactly() {
    let fixture = Restart::child("unlinked");
    run_child(fixture.store.root_path(), "resume-unlinked");
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
    let reopened = EnvironmentStore::open(
        fixture.store.root_path(),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let outcome = reopened
        .with_control(ControlLimits::default(), |control| {
            control.recover_uncommitted_prefix(&fixture.journal, OwnershipRecoveryLimits::default())
        })
        .unwrap();
    fixture.assert_cleaned(outcome);
}
#[test]
fn independently_reopened_cleanup_does_not_bypass_the_current_real_build_ex() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("environments");
    let (store, _prefix, journal) = setup(&path);
    assert!(matches!(
        store
            .with_control(ControlLimits::default(), |control| control
                .recover_uncommitted_prefix(
                    &journal,
                    OwnershipRecoveryLimits::default()
                ))
            .unwrap(),
        OwnershipRecoveryOutcome::Busy
    ));
}
#[test]
fn superseded_committed_transaction_is_reported_without_rolling_back_its_generation() {
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
                catalog.apply_selection(
                    fixture.journal.key(),
                    &SelectionStamp::default(),
                    Some(&generation.identity().allocation),
                    fixture.journal.nonce(),
                )?;
                let current = catalog.snapshot(fixture.journal.key());
                catalog.apply_selection(
                    fixture.journal.key(),
                    &current,
                    None,
                    Uuid::from_u128(202),
                )?;
                Ok(())
            })
        })
        .unwrap();
    let before = fixture.log().total_bytes();
    assert!(matches!(
        fixture.recover().unwrap(),
        OwnershipRecoveryOutcome::AlreadyCommitted
    ));
    assert_eq!(fixture.log().total_bytes(), before);
    assert!(
        fixture
            .store
            .root_path()
            .join(&fixture.journal.generation().unwrap().identity().allocation)
            .join("lib/file")
            .is_file()
    );
}
#[test]
fn uncommitted_clear_has_no_generation_or_ownership_stream_to_reconstruct() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("environments");
    let store = EnvironmentStore::open(
        &path,
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let key =
        SelectionKey::new(Uuid::from_u128(1), "clear", [3; 32], ModelLimits::default()).unwrap();
    let outcome = store
        .with_control(ControlLimits::default(), |control| {
            let journal = prepare_journal_metadata(
                control,
                &JournalPrepare {
                    nonce: Uuid::from_u128(101),
                    key: &key,
                    expected: &SelectionStamp::default(),
                    target: JournalTarget::ClearSelection,
                },
                JournalLimits::default(),
            )?;
            control.recover_uncommitted_prefix(&journal, OwnershipRecoveryLimits::default())
        })
        .unwrap();
    assert!(matches!(
        outcome,
        OwnershipRecoveryOutcome::UncommittedClear
    ));
}
