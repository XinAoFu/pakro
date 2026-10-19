//! Filesystem regressions for the context-bound native proof cache.
//! These fixtures contain proof records only; they do not verify a distribution.

use super::{ProofDirectory, cached, record};
use crate::libraries::language::repository::tests::operation_context;
use crate::plugins::core::python::{native_provenance_path, native_provenance_record};
use crate::programs::{OperationContext, OperationError};
use pakro_index::artifact::SourceArtifact;
use pakro_store::Digest;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

struct Fixture {
    _owner: tempfile::TempDir,
    root: PathBuf,
    source: SourceArtifact,
}

impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let source = SourceArtifact::new(
            "cpython-3.15.0+20261013-aarch64-apple-darwin-install_only.tar.gz",
            concat!(
                "https://github.com/astral-sh/python-build-standalone/",
                "releases/download/20261013/",
                "cpython-3.15.0%2B20261013-aarch64-apple-darwin-install_only.tar.gz",
            ),
            Digest::from_sha256_bytes(b"R06 expected verified artifact"),
        )
        .unwrap();
        Self {
            _owner: owner,
            root,
            source,
        }
    }

    fn context(&self, writable: bool) -> OperationContext {
        operation_context(&self.root, writable, Duration::from_secs(30))
    }

    fn proof_root(&self) -> PathBuf {
        self.root.join("cache/python/native-v1")
    }

    fn proof_path(&self) -> PathBuf {
        native_provenance_path(&self.proof_root(), &self.source).unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        native_provenance_record(&self.source).unwrap()
    }

    fn assert_no_context_storage(&self) {
        assert!(
            !self.root.join("cache").exists(),
            "proof lookup created the cache root"
        );
        assert!(
            !self.root.join("data").exists(),
            "proof lookup created the data root"
        );
    }
}

fn file_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn assert_control_error<T>(result: eyre::Result<T>, expected: OperationError) {
    let error = match result {
        Ok(_) => panic!("proof IO ignored its operation control"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<OperationError>(),
        Some(&expected),
        "proof IO returned a different failure before checking its context: {error:#}"
    );
}

#[test]
fn r06_provenance_missing_cache_read_creates_nothing() {
    let fixture = Fixture::new();
    let context = fixture.context(false);
    assert!(!cached(&context, &fixture.source).unwrap());
    fixture.assert_no_context_storage();
}

#[test]
fn r06_provenance_controlled_record_is_reused_without_replacing_its_inode() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    assert!(!cached(&context, &fixture.source).unwrap());
    record(&context, &fixture.source).unwrap();
    let path = fixture.proof_path();
    let original = file_identity(&path);
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes());
    assert!(cached(&context, &fixture.source).unwrap());

    record(&context, &fixture.source).unwrap();
    assert_eq!(file_identity(&path), original);
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes());
    assert!(cached(&fixture.context(false), &fixture.source).unwrap());
}

#[test]
fn r06_provenance_parent_symlink_cannot_supply_a_cached_proof() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    let outside = fixture.root.join("outside-proofs");
    fs::create_dir(&outside).unwrap();
    fs::create_dir_all(fixture.proof_root()).unwrap();
    let leaf = fixture.proof_path().file_name().unwrap().to_owned();
    let external_proof = outside.join(&leaf);
    fs::write(&external_proof, fixture.bytes()).unwrap();
    fs::set_permissions(&external_proof, fs::Permissions::from_mode(0o600)).unwrap();
    let original = file_identity(&external_proof);
    symlink(&outside, fixture.proof_root().join("provenance")).unwrap();

    assert!(
        cached(&context, &fixture.source).is_err(),
        "a parent symlink supplied an apparently valid external proof"
    );
    assert_eq!(file_identity(&external_proof), original);
    assert_eq!(fs::read(&external_proof).unwrap(), fixture.bytes());
}

#[test]
fn r06_provenance_parent_symlink_cannot_receive_a_record() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    let outside = fixture.root.join("outside-proofs");
    fs::create_dir(&outside).unwrap();
    fs::create_dir_all(fixture.proof_root()).unwrap();
    symlink(&outside, fixture.proof_root().join("provenance")).unwrap();

    assert!(
        record(&context, &fixture.source).is_err(),
        "proof publication followed an external parent"
    );
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "proof publication wrote outside its authorized cache"
    );
    assert_eq!(
        fs::read_link(fixture.proof_root().join("provenance")).unwrap(),
        outside
    );
}

#[test]
fn r06_provenance_leaf_symlink_is_rejected_and_preserved() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    record(&context, &fixture.source).unwrap();
    let path = fixture.proof_path();
    let outside = fixture.root.join("outside-proof.json");
    fs::write(&outside, fixture.bytes()).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();

    assert!(cached(&context, &fixture.source).is_err());
    assert!(record(&context, &fixture.source).is_err());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&path).unwrap(), outside);
    assert_eq!(fs::read(&outside).unwrap(), fixture.bytes());
}

#[test]
fn r06_provenance_hardlinked_record_is_rejected_and_preserved() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    record(&context, &fixture.source).unwrap();
    let path = fixture.proof_path();
    let outside = fixture.root.join("outside-proof.json");
    fs::remove_file(&path).unwrap();
    fs::write(&outside, fixture.bytes()).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&outside, &path).unwrap();
    let original = file_identity(&path);
    assert_eq!(
        fs::metadata(&path).unwrap().nlink(),
        2,
        "the fixture must contain a real hardlink"
    );

    assert!(cached(&context, &fixture.source).is_err());
    assert!(record(&context, &fixture.source).is_err());
    assert_eq!(file_identity(&path), original);
    assert_eq!(file_identity(&outside), original);
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 2);
    assert_eq!(fs::read(&outside).unwrap(), fixture.bytes());
}

#[test]
fn r06_provenance_group_writable_record_is_rejected_without_repair() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    record(&context, &fixture.source).unwrap();
    let path = fixture.proof_path();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).unwrap();
    let original = file_identity(&path);

    assert!(cached(&context, &fixture.source).is_err());
    assert!(record(&context, &fixture.source).is_err());
    assert_eq!(file_identity(&path), original);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o620);
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes());
}

#[test]
fn r06_provenance_cancelled_context_rejects_read_and_write_without_creation() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    context.cancellation_token().cancel();

    assert_control_error(cached(&context, &fixture.source), OperationError::Cancelled);
    assert_control_error(record(&context, &fixture.source), OperationError::Cancelled);
    fixture.assert_no_context_storage();
}

#[test]
fn r06_provenance_original_deadline_rejects_read_and_write_without_creation() {
    let fixture = Fixture::new();
    // Capture must start with a valid deadline. Keep that exact Instant, then
    // let it elapse without renewing the operation or bypassing its constructor.
    let context = operation_context(&fixture.root, true, Duration::from_millis(50));
    let deadline = context.policy().deadline;
    while Instant::now() < deadline {
        std::thread::yield_now();
    }

    assert_control_error(
        cached(&context, &fixture.source),
        OperationError::DeadlineExceeded,
    );
    assert_control_error(
        record(&context, &fixture.source),
        OperationError::DeadlineExceeded,
    );
    assert_eq!(context.policy().deadline, deadline);
    fixture.assert_no_context_storage();
}

#[test]
fn r06_provenance_readonly_policy_reads_but_cannot_record() {
    let fixture = Fixture::new();
    record(&fixture.context(true), &fixture.source).unwrap();
    let path = fixture.proof_path();
    let original = file_identity(&path);
    let readonly = fixture.context(false);

    assert!(cached(&readonly, &fixture.source).unwrap());
    assert!(
        record(&readonly, &fixture.source).is_err(),
        "idempotence does not grant write permission"
    );
    let other = SourceArtifact::new(
        fixture.source.filename(),
        fixture.source.url().as_str(),
        Digest::from_sha256_bytes(b"different verified artifact"),
    )
    .unwrap();
    assert!(record(&readonly, &other).is_err());
    let other_path = native_provenance_path(&fixture.proof_root(), &other).unwrap();
    assert!(!other_path.exists());
    assert_eq!(file_identity(&path), original);
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes());
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn r06_provenance_concurrent_record_keeps_one_complete_file() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let operation = context.clone();
        let source = fixture.source.clone();
        let start = barrier.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            record(&operation, &source)
        }));
    }
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    for result in results {
        result.unwrap().unwrap();
    }

    let path = fixture.proof_path();
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes());
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    assert!(cached(&context, &fixture.source).unwrap());
    let original = file_identity(&path);
    record(&context, &fixture.source).unwrap();
    assert_eq!(file_identity(&path), original);
}

#[test]
fn r06_provenance_corrupt_record_is_rejected_without_overwrite() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    record(&context, &fixture.source).unwrap();
    let path = fixture.proof_path();
    let original = file_identity(&path);
    let corrupt = b"{\"schema\":1,\"incomplete\":";
    fs::write(&path, corrupt).unwrap();

    assert!(cached(&context, &fixture.source).is_err());
    assert!(record(&context, &fixture.source).is_err());
    assert_eq!(file_identity(&path), original);
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn r06_provenance_cache_root_symlink_cannot_supply_or_receive_records() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    let outside = fixture.root.join("outside-cache");
    let outside_proof_root = outside.join("python/native-v1");
    let outside_path = native_provenance_path(&outside_proof_root, &fixture.source).unwrap();
    fs::create_dir_all(outside_path.parent().unwrap()).unwrap();
    fs::write(&outside_path, fixture.bytes()).unwrap();
    fs::set_permissions(&outside_path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = file_identity(&outside_path);
    symlink(&outside, context.requested_cache_root()).unwrap();

    assert!(cached(&context, &fixture.source).is_err());
    assert!(record(&context, &fixture.source).is_err());
    let other = SourceArtifact::new(
        fixture.source.filename(),
        fixture.source.url().as_str(),
        Digest::from_sha256_bytes(b"another verified artifact"),
    )
    .unwrap();
    assert!(record(&context, &other).is_err());
    let other_outside_path = native_provenance_path(&outside_proof_root, &other).unwrap();
    assert!(!other_outside_path.exists());
    assert_eq!(file_identity(&outside_path), original);
    assert_eq!(fs::read(&outside_path).unwrap(), fixture.bytes());
    assert_eq!(
        fs::read_dir(outside_path.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        fs::read_link(context.requested_cache_root()).unwrap(),
        outside
    );
    assert!(!context.requested_data_root().exists());
}

#[test]
fn r06_provenance_held_directory_cannot_be_used_after_it_moves_outside() {
    let fixture = Fixture::new();
    let context = fixture.context(true);
    record(&context, &fixture.source).unwrap();
    let held = ProofDirectory::open(&context, false).unwrap().unwrap();
    let path = fixture.proof_path();
    let name = path.file_name().unwrap().to_str().unwrap();
    let original = file_identity(&path);
    let outside = fixture.root.join("moved-proofs");
    fs::rename(path.parent().unwrap(), &outside).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();

    assert!(held.contains(name, &fixture.bytes()).is_err());
    let other = SourceArtifact::new(
        fixture.source.filename(),
        fixture.source.url().as_str(),
        Digest::from_sha256_bytes(b"another verified artifact"),
    )
    .unwrap();
    let other_path = native_provenance_path(&fixture.proof_root(), &other).unwrap();
    let other_name = other_path.file_name().unwrap().to_str().unwrap();
    let other_bytes = native_provenance_record(&other).unwrap();
    assert!(held.publish(other_name, &other_bytes).is_err());

    assert_eq!(file_identity(&outside.join(name)), original);
    assert_eq!(fs::read(outside.join(name)).unwrap(), fixture.bytes());
    assert!(!outside.join(other_name).exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 0);
}
