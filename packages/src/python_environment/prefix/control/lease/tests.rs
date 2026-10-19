use super::*;
use std::cell::RefCell;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
    allocations: RefCell<Vec<UnpublishedPrefix>>,
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
        Self {
            _root: root,
            store,
            allocations: RefCell::new(Vec::new()),
        }
    }
    fn identity(&self) -> GenerationIdentity {
        let prefix = self.store.allocate().unwrap();
        let identity = prefix
            .generation_identity()
            .expect("identity capture prerequisite");
        self.release_build(prefix);
        identity
    }
    fn release_build(&self, prefix: UnpublishedPrefix) {
        use std::os::fd::AsRawFd;
        // Test only: release the actual EX while retaining the unpublished
        // cleanup owner. Dropping it would discard the generation itself.
        // SAFETY: the owned live Flock keeps this descriptor open for this call.
        let result = unsafe {
            nix::libc::flock(prefix.binding._build_lease.as_raw_fd(), nix::libc::LOCK_UN)
        };
        assert_eq!(result, 0);
        self.allocations.borrow_mut().push(prefix);
    }
    fn acquire(
        &self,
        identity: &GenerationIdentity,
        mode: GenerationLeaseMode,
    ) -> Result<Option<GenerationLease>> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.try_generation_lease(identity, mode)
            })
    }
    fn lock_path(&self, identity: &GenerationIdentity) -> PathBuf {
        self.store
            .root_path()
            .join(".leases/build")
            .join(format!("{}.lock", identity.allocation))
    }
}

#[test]
fn capture_is_bound_to_the_actual_allocated_generation() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    assert_eq!(identity.allocation, prefix.allocation_id());
    assert_eq!(
        FileIdentity::of(&identity.store).unwrap(),
        FileIdentity::of(&fixture.store.root).unwrap()
    );
    assert_eq!(
        FileIdentity::of(&identity.generation).unwrap(),
        FileIdentity::of(&prefix.binding.root).unwrap()
    );
    assert_eq!(
        FileIdentity::of(&identity.lock).unwrap(),
        FileIdentity::of(&prefix.binding._build_lease).unwrap()
    );
    assert!(prefix.check_binding().is_ok());
}

#[test]
fn identity_facts_preserve_all_actual_inodes_without_reconstructing_authority() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    let facts = identity.facts().unwrap();
    assert_eq!(facts.allocation, prefix.allocation_id());
    let pair = |file: &File| {
        let m = file.metadata().unwrap();
        [m.dev(), m.ino()]
    };
    assert_eq!(facts.store, pair(&identity.store));
    assert_eq!(facts.generation, pair(&identity.generation));
    assert_eq!(facts.leases, pair(&identity.leases));
    assert_eq!(facts.build, pair(&identity.build));
    assert_eq!(facts.lock, pair(&identity.lock));
    assert!(prefix.check_binding().is_ok());
}

#[test]
fn identity_facts_refuse_to_describe_a_replaced_generation() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    identity.facts().unwrap();
    let path = fixture.store.root_path().join(&identity.allocation);
    fs::rename(&path, path.with_extension("saved")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(identity.facts().is_err());
}

#[test]
fn held_build_prevents_both_live_and_collection() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    for mode in [GenerationLeaseMode::Live, GenerationLeaseMode::Collect] {
        assert!(
            fixture.acquire(&identity, mode).unwrap().is_none(),
            "build lease was bypassed"
        );
    }
    fixture.release_build(prefix);
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .is_some()
    );
}

#[test]
fn discarded_generation_identity_cannot_acquire_a_live_lease() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    drop(prefix);
    assert!(
        !fixture
            .store
            .root_path()
            .join(&identity.allocation)
            .exists()
    );
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_err()
    );
}

#[test]
fn shared_live_leases_outlive_the_control_session_and_block_collection() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    let first = fixture
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap()
        .unwrap();
    let second = fixture
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap()
        .unwrap();
    first.check_binding().unwrap();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Collect)
            .unwrap()
            .is_none()
    );
    drop(first);
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Collect)
            .unwrap()
            .is_none()
    );
    drop(second);
    let collector = fixture
        .acquire(&identity, GenerationLeaseMode::Collect)
        .unwrap()
        .unwrap();
    collector.check_binding().unwrap();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .is_none()
    );
    drop(collector);
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .is_some()
    );
}

#[test]
fn identity_from_a_different_store_cannot_acquire_a_lock() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let identity = fixture.identity();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .is_some()
    );
    let error = other
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap_err();
    assert!(error.to_string().contains("store identity"), "{error:#}");
}

#[test]
fn missing_lock_is_an_error_and_is_never_recreated() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .is_some()
    );
    let path = fixture.lock_path(&identity);
    fs::remove_file(&path).unwrap();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn same_bytes_new_lock_inode_does_not_create_a_second_lease_domain() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    let held = fixture
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap()
        .unwrap();
    let path = fixture.lock_path(&identity);
    fs::rename(&path, path.with_extension("saved")).unwrap();
    fs::write(&path, []).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let error = fixture
        .acquire(&identity, GenerationLeaseMode::Collect)
        .unwrap_err();
    assert!(error.to_string().contains("identity"), "{error:#}");
    assert!(held.check_binding().is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(path.with_extension("saved"), &path).unwrap();
    held.check_binding().unwrap();
}

#[test]
fn replaced_generation_directory_is_rejected_even_with_the_original_lock() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    let held = fixture
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap()
        .unwrap();
    let path = fixture.store.root_path().join(&identity.allocation);
    fs::rename(&path, path.with_extension("saved")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_err()
    );
    assert!(held.check_binding().is_err());
}

#[test]
fn unsafe_lock_files_fail_without_blocking_or_following_them() {
    for variant in [
        "symlink",
        "fifo",
        "directory",
        "hardlink",
        "public",
        "privileged",
    ] {
        let fixture = Fixture::new();
        let identity = fixture.identity();
        assert!(
            fixture
                .acquire(&identity, GenerationLeaseMode::Live)
                .unwrap()
                .is_some()
        );
        let path = fixture.lock_path(&identity);
        match variant {
            "symlink" => {
                fs::rename(&path, path.with_extension("saved")).unwrap();
                symlink(path.with_extension("saved"), &path).unwrap();
            }
            "fifo" => {
                fs::remove_file(&path).unwrap();
                nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap();
            }
            "directory" => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            "hardlink" => fs::hard_link(&path, path.with_extension("alias")).unwrap(),
            "public" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            "privileged" => fs::set_permissions(&path, fs::Permissions::from_mode(0o4600)).unwrap(),
            _ => unreachable!(),
        }
        let start = Instant::now();
        assert!(
            fixture
                .acquire(&identity, GenerationLeaseMode::Live)
                .is_err(),
            "accepted {variant}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn replaced_lease_directory_and_store_are_detected_by_a_held_lease() {
    for component in [".leases/build", ""] {
        let fixture = Fixture::new();
        let identity = fixture.identity();
        let held = fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .unwrap()
            .unwrap();
        let path = fixture.store.root_path().join(component);
        let saved = path.with_extension("saved");
        fs::rename(&path, &saved).unwrap();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            held.check_binding().is_err(),
            "missed replacement {component}"
        );
        assert!(
            fixture
                .acquire(&identity, GenerationLeaseMode::Live)
                .is_err()
        );
    }
}

const CHILD_PATH: &str = "__PAKRO_GENERATION_LEASE_TEST_PATH";
const CHILD_READY: &str = "__PAKRO_GENERATION_LEASE_TEST_READY";
const CHILD_MODE: &str = "__PAKRO_GENERATION_LEASE_TEST_MODE";

#[test]
fn lease_process_helper() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mode = if std::env::var(CHILD_MODE).unwrap() == "shared" {
        FlockArg::LockSharedNonblock
    } else {
        FlockArg::LockExclusiveNonblock
    };
    let _held = Flock::lock(file, mode).unwrap();
    let ready = std::env::var_os(CHILD_READY).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(ready)
        .unwrap()
        .write_all(b"ready")
        .unwrap();
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
}

struct ChildLease(Child);
impl Drop for ChildLease {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child_lock_blocks_parent(mode: &str, parent: GenerationLeaseMode) {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    let ready = fixture.store.root_path().join("child.ready");
    let parent_name = std::thread::current().name().unwrap().to_owned();
    let helper = format!(
        "{}::lease_process_helper",
        parent_name.rsplit_once("::").unwrap().0
    );
    let mut child = ChildLease(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &helper, "--nocapture"])
            .env(CHILD_PATH, fixture.lock_path(&identity))
            .env(CHILD_READY, &ready)
            .env(CHILD_MODE, mode)
            .env("__PAKRO_TEST_FIXTURES_READY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "lease child exited before acquiring lock"
        );
        assert!(Instant::now() < deadline, "lease child never acquired lock");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.acquire(&identity, parent).unwrap().is_none());
    child.0.stdin.take().unwrap().write_all(&[1]).unwrap();
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "lease child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.acquire(&identity, parent).unwrap().is_some());
    assert!(
        fixture.lock_path(&identity).exists(),
        "lock entry was unlinked on release"
    );
}

#[test]
fn independent_process_shared_lease_blocks_collection_until_process_exit() {
    child_lock_blocks_parent("shared", GenerationLeaseMode::Collect);
}

#[test]
fn independent_process_exclusive_lease_blocks_live_until_process_exit() {
    child_lock_blocks_parent("exclusive", GenerationLeaseMode::Live);
}
