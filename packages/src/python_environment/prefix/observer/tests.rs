use super::*;
use crate::packages::python_environment::prefix::OwnershipObserverLimits;

use crate::packages::python_environment::journal::{
    JournalPrepare, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use crate::packages::python_environment::prefix::control::ControlNamespace;
use pakro_store::Digest;
use std::fs;
use std::os::unix::fs::MetadataExt;
use uuid::Uuid;

pub(super) struct Fixture {
    _root: tempfile::TempDir,
    pub(super) store: EnvironmentStore,
    pub(super) prefix: UnpublishedPrefix,
    pub(super) journal: JournalRecord,
}
impl Fixture {
    fn new() -> Self {
        Self::with_limits(PrefixLimits::default())
    }
    pub(super) fn with_limits(limits: PrefixLimits) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::open(
            &root.path().canonicalize().unwrap().join("environments"),
            nix::unistd::geteuid().as_raw(),
            limits,
        )
        .unwrap();
        let prefix = store
            .allocate()
            .expect("real prefix allocation prerequisite");
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
            .expect("actual durable journal prerequisite");
        Self {
            _root: root,
            store,
            prefix,
            journal,
        }
    }
    pub(super) fn attach(&mut self, limits: OwnershipObserverLimits) -> Result<()> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.attach_prefix_ownership(
                    &mut self.prefix,
                    &self.journal,
                    JournalLimits::default(),
                    limits,
                )
            })
    }
    pub(super) fn stream_path(&self) -> PathBuf {
        self.store
            .root_path()
            .join(".control/ownership")
            .join(format!("{}.stream", self.journal.nonce().simple()))
    }
    fn journal_path(&self) -> PathBuf {
        self.store
            .root_path()
            .join(".control/journals")
            .join(format!("{}.json", self.journal.nonce().simple()))
    }
    fn seed_stream(&self, bytes: &[u8]) {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.replace(
                    ControlNamespace::Ownership,
                    &format!("{}.stream", self.journal.nonce().simple()),
                    None,
                    bytes,
                )?;
                Ok(())
            })
            .unwrap();
    }
}
fn snapshot(path: &Path) -> (u64, u64, Vec<u8>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino(), fs::read(path).unwrap())
}

#[test]
fn attach_empty_real_prefix_persists_a_bound_stream_before_returning() {
    let mut fixture = Fixture::new();
    let before = snapshot(&fixture.journal_path());
    fixture.attach(OwnershipObserverLimits::default()).unwrap();
    let metadata = fs::symlink_metadata(fixture.stream_path())
        .expect("attach returned without a durable stream");
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(
        matches!(fixture.prefix.ownership, OwnershipRegistration::Attached(_)),
        "prefix did not retain its actual observer capability"
    );
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
    assert!(log.records().is_empty());
    assert_eq!(
        log.identity(),
        fixture.journal.generation().unwrap().identity()
    );
    assert_eq!(log.nonce(), fixture.journal.nonce());
    assert_eq!(snapshot(&fixture.journal_path()), before);
    assert!(fixture.prefix.check_binding().is_ok());
    assert!(
        !fixture
            .store
            .root_path()
            .join(".control/catalog/state.json")
            .exists()
    );
}

#[test]
fn nonempty_written_or_unknown_prefixes_cannot_be_retroactively_registered() {
    let mut accepted = Vec::new();
    for label in ["directory", "file", "written", "unknown"] {
        let mut fixture = Fixture::new();
        match label {
            "directory" => fixture.prefix.create_dir_all(Path::new("lib")).unwrap(),
            "file" => {
                fixture
                    .prefix
                    .create_file_exclusive(Path::new("empty"), 0o644)
                    .unwrap()
                    .finish()
                    .unwrap();
            }
            "written" => {
                let mut writer = fixture
                    .prefix
                    .create_file_exclusive(Path::new("file"), 0o644)
                    .unwrap();
                writer.write_all(b"actual bytes").unwrap();
                writer.finish().unwrap();
            }
            _ => fs::write(fixture.prefix.final_path().join("unknown"), b"not owned").unwrap(),
        }
        let original = fixture.prefix.final_path().to_owned();
        if fixture.attach(OwnershipObserverLimits::default()).is_ok() {
            accepted.push(label);
        }
        assert!(original.is_dir());
        assert!(!fixture.stream_path().exists());
    }
    assert!(
        accepted.is_empty(),
        "late attachment accepted: {accepted:?}"
    );
}

#[test]
fn existing_stream_is_not_overwritten_or_taken_over_and_duplicate_attach_is_rejected() {
    let mut fixture = Fixture::new();
    let stream = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.create_ownership_stream(
                &fixture.journal,
                &fixture.prefix,
                JournalLimits::default(),
                OwnershipStreamLimits::default(),
            )
        })
        .expect("actual independent raw constructor prerequisite");
    let before = snapshot(&fixture.stream_path());
    let existing = fixture.attach(OwnershipObserverLimits::default());
    assert!(existing.is_err(), "attachment took over an existing stream");
    assert_eq!(snapshot(&fixture.stream_path()), before);
    drop(stream);
    let mut second = Fixture::new();
    second.attach(OwnershipObserverLimits::default()).unwrap();
    let before = snapshot(&second.stream_path());
    assert!(second.attach(OwnershipObserverLimits::default()).is_err());
    assert_eq!(snapshot(&second.stream_path()), before);
}

#[test]
fn foreign_control_journal_or_prefix_never_binds_another_store() {
    let mut accepted = Vec::new();
    for label in ["control", "journal", "prefix"] {
        let mut own = Fixture::new();
        let mut other = Fixture::new();
        let result = match label {
            "control" => other
                .store
                .with_control(ControlLimits::default(), |control| {
                    control.attach_prefix_ownership(
                        &mut own.prefix,
                        &own.journal,
                        JournalLimits::default(),
                        OwnershipObserverLimits::default(),
                    )
                }),
            "journal" => own.store.with_control(ControlLimits::default(), |control| {
                control.attach_prefix_ownership(
                    &mut own.prefix,
                    &other.journal,
                    JournalLimits::default(),
                    OwnershipObserverLimits::default(),
                )
            }),
            _ => own.store.with_control(ControlLimits::default(), |control| {
                control.attach_prefix_ownership(
                    &mut other.prefix,
                    &own.journal,
                    JournalLimits::default(),
                    OwnershipObserverLimits::default(),
                )
            }),
        };
        if result.is_ok() {
            accepted.push(label);
        }
        assert!(!own.stream_path().exists());
        assert!(!other.stream_path().exists());
    }
    assert!(
        accepted.is_empty(),
        "cross-store ownership attachment accepted: {accepted:?}"
    );
}

#[test]
fn missing_stale_or_corrupt_durable_journal_is_not_replaced_by_caller_facts() {
    let mut accepted = Vec::new();
    for label in ["missing", "stale", "corrupt"] {
        let mut fixture = Fixture::new();
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                let name = format!("{}.json", fixture.journal.nonce().simple());
                let original = control.read(ControlNamespace::Journals, &name)?.unwrap();
                if label == "missing" {
                    control.remove(&original)?;
                } else {
                    let bytes = if label == "stale" {
                        let mut value: serde_json::Value =
                            serde_json::from_slice(original.bytes()).unwrap();
                        value["target"]["graph"] = serde_json::json!("09".repeat(32));
                        serde_json::to_vec(&value).unwrap()
                    } else {
                        b"{broken".to_vec()
                    };
                    control.replace(ControlNamespace::Journals, &name, Some(&original), &bytes)?;
                }
                Ok(())
            })
            .unwrap();
        let before = fixture
            .journal_path()
            .exists()
            .then(|| snapshot(&fixture.journal_path()));
        if fixture.attach(OwnershipObserverLimits::default()).is_ok() {
            accepted.push(label);
        }
        assert!(!fixture.stream_path().exists());
        if let Some(before) = before {
            assert_eq!(snapshot(&fixture.journal_path()), before);
        } else {
            assert!(!fixture.journal_path().exists());
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid durable journal attachment accepted: {accepted:?}"
    );
}

#[test]
fn tainted_or_discarded_prefix_does_not_gain_an_observer() {
    let mut accepted = Vec::new();
    for discarded in [false, true] {
        let mut fixture = Fixture::new();
        if discarded {
            fixture.prefix.discard().unwrap();
        } else {
            fixture.prefix.mark_failed();
        }
        if fixture.attach(OwnershipObserverLimits::default()).is_ok() {
            accepted.push(discarded);
        }
        assert!(!fixture.stream_path().exists());
        assert!(!matches!(
            fixture.prefix.ownership,
            OwnershipRegistration::Attached(_)
        ));
    }
    assert!(
        accepted.is_empty(),
        "invalid prefix lifecycle gained observer authority: {accepted:?}"
    );
}

#[test]
fn observer_limits_are_checked_before_stream_creation_and_prefix_bounds_are_not_caller_inputs() {
    let default = OwnershipObserverLimits::default();
    let mut accepted = Vec::new();
    for (label, limits) in [
        (
            "event",
            OwnershipObserverLimits {
                max_event_bytes: 0,
                ..default
            },
        ),
        (
            "events",
            OwnershipObserverLimits {
                max_events: 0,
                ..default
            },
        ),
        (
            "path",
            OwnershipObserverLimits {
                max_total_path_bytes: 0,
                ..default
            },
        ),
        (
            "stream",
            OwnershipObserverLimits {
                stream: OwnershipStreamLimits {
                    max_total_bytes: 1,
                    ..default.stream
                },
                ..default
            },
        ),
    ] {
        let mut fixture = Fixture::with_limits(PrefixLimits {
            max_entries: 2,
            max_total_bytes: 5,
            ..PrefixLimits::default()
        });
        if fixture.attach(limits).is_ok() {
            accepted.push(label);
        }
        assert!(!fixture.stream_path().exists());
    }
    assert!(
        accepted.is_empty(),
        "attachment ignored observer resource limits: {accepted:?}"
    );
}

#[test]
fn authenticated_stream_creation_failure_is_retained_on_drop_instead_of_unlogged_cleanup() {
    let mut fixture = Fixture::new();
    fixture.seed_stream(b"partial constructor evidence");
    let original = snapshot(&fixture.stream_path());
    let final_path = fixture.prefix.final_path().to_owned();
    let error = fixture
        .attach(OwnershipObserverLimits::default())
        .unwrap_err();
    assert!(
        error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::EEXIST)
            || error.chain().any(|cause| cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::AlreadyExists)),
        "attachment did not reach the actual exclusive stream-create error: {error:?}"
    );
    assert!(
        fixture.prefix.check_binding().is_err(),
        "logging failure left prefix ready"
    );
    assert!(
        matches!(fixture.prefix.ownership,OwnershipRegistration::Retained{nonce} if nonce==fixture.journal.nonce())
    );
    let stream_path = fixture.stream_path();
    let journal_path = fixture.journal_path();
    drop(fixture.prefix);
    assert!(
        final_path.is_dir(),
        "Drop removed a journal-bound root without cleanup events"
    );
    assert_eq!(snapshot(&stream_path), original);
    assert!(journal_path.is_file());
    assert_eq!(fs::read_dir(&final_path).unwrap().count(), 0);
}
