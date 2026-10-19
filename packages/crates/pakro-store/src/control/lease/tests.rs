use super::*;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Fixture {
    directory: tempfile::TempDir,
    parent: File,
    uid: u32,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = File::open(directory.path()).unwrap();
        Self {
            directory,
            parent,
            uid: nix::unistd::geteuid().as_raw(),
        }
    }
    fn lock(&self, name: &str) -> StableLock {
        StableLock::create_new(&self.parent, name, self.uid).unwrap()
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

pub(crate) fn r05_lock_inode_replacement_is_rejected() {
    let fixture = Fixture::new();
    let lock = fixture.lock("object.lock");
    let identity = lock.identity();
    let held = lock.acquire(LeaseMode::Shared, deadline()).unwrap();
    fs::rename(
        fixture.directory.path().join("object.lock"),
        fixture.directory.path().join("saved.lock"),
    )
    .unwrap();
    let replacement = fixture.lock("object.lock");
    assert_ne!(replacement.identity(), identity);
    assert!(lock.check_binding().is_err());
    assert!(held.check_binding().is_err());
    assert!(lock.try_acquire(LeaseMode::Exclusive, deadline()).is_err());
    assert!(
        StableLock::open_existing(&fixture.parent, "object.lock", fixture.uid, identity).is_err()
    );
}

#[test]
fn r05_missing_lock_is_never_recreated() {
    let fixture = Fixture::new();
    let lock = fixture.lock("object.lock");
    let identity = lock.identity();
    fs::remove_file(fixture.directory.path().join("object.lock")).unwrap();
    assert!(lock.try_acquire(LeaseMode::Shared, deadline()).is_err());
    assert!(
        StableLock::open_existing(&fixture.parent, "object.lock", fixture.uid, identity).is_err()
    );
    assert!(!fixture.directory.path().join("object.lock").exists());
}

#[test]
fn r05_lock_open_rejects_symlinks_hardlinks_and_unsafe_modes() {
    for mutation in ["symlink", "hardlink", "mode"] {
        let fixture = Fixture::new();
        let lock = fixture.lock("object.lock");
        let identity = lock.identity();
        let path = fixture.directory.path().join("object.lock");
        match mutation {
            "symlink" => {
                let saved = fixture.directory.path().join("saved.lock");
                fs::rename(&path, &saved).unwrap();
                std::os::unix::fs::symlink(&saved, &path).unwrap();
            }
            "hardlink" => {
                fs::hard_link(&path, fixture.directory.path().join("alias.lock")).unwrap()
            }
            _ => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
        }
        assert!(
            StableLock::open_existing(&fixture.parent, "object.lock", fixture.uid, identity)
                .is_err()
        );
        assert!(lock.try_acquire(LeaseMode::Shared, deadline()).is_err());
    }
}

#[test]
fn r05_stable_lock_names_cannot_escape_the_parent() {
    let fixture = Fixture::new();
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "nested/lock",
        "/absolute",
        "bad\0lock",
    ] {
        assert!(StableLock::create_new(&fixture.parent, name, fixture.uid).is_err());
    }
}

#[test]
fn r05_lock_release_keeps_same_inode_and_last_clone_retains_lock() {
    let fixture = Fixture::new();
    let lock = fixture.lock("object.lock");
    let identity = lock.identity();
    let held = lock.acquire(LeaseMode::Shared, deadline()).unwrap();
    let clone = held.clone();
    drop(held);
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_none()
    );
    drop(clone);
    assert_eq!(
        FileIdentity::of(&File::open(fixture.directory.path().join("object.lock")).unwrap())
            .unwrap(),
        identity
    );
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_some()
    );
}

#[test]
fn r05_absolute_deadline_is_not_reset_between_locks() {
    let fixture = Fixture::new();
    let first = fixture.lock("first.lock");
    let second = fixture.lock("second.lock");
    let expiry = Instant::now() + Duration::from_millis(40);
    let mut session = OrderedLocks::new(expiry);
    session
        .try_object("a", &first, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert!(session.try_object("b", &second, LeaseMode::Shared).is_err());
    assert!(
        second
            .try_acquire(LeaseMode::Shared, Instant::now())
            .is_err()
    );
}

#[test]
fn r05_wait_times_out_while_conflicting_lease_survives() {
    let fixture = Fixture::new();
    let lock = fixture.lock("object.lock");
    let _held = lock.acquire(LeaseMode::Shared, deadline()).unwrap();
    let start = Instant::now();
    assert!(
        lock.acquire(LeaseMode::Exclusive, start + Duration::from_millis(60))
            .is_err()
    );
    assert!(start.elapsed() >= Duration::from_millis(55));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(
        lock.try_acquire(LeaseMode::Shared, deadline())
            .unwrap()
            .is_some()
    );
}

#[test]
fn r05_sorted_objects_precede_catalog_and_reject_upgrades() {
    let fixture = Fixture::new();
    let project = fixture.lock("project.lock");
    let a = fixture.lock("a.lock");
    let b = fixture.lock("b.lock");
    let catalog = fixture.lock("catalog.lock");
    let mut session = OrderedLocks::new(deadline());
    session.acquire_project(&project).unwrap();
    let held = session
        .acquire_objects(&[("b", &b, LeaseMode::Shared), ("a", &a, LeaseMode::Shared)])
        .unwrap();
    assert_eq!(held[0].identity(), a.identity());
    assert_eq!(held[1].identity(), b.identity());
    assert!(session.try_object("a", &a, LeaseMode::Exclusive).is_err());
    session.catalog(&catalog, LeaseMode::Exclusive).unwrap();
    session.catalog(&catalog, LeaseMode::Exclusive).unwrap();
    assert!(session.try_object("c", &a, LeaseMode::Shared).is_err());
    assert!(session.acquire_project(&project).is_err());
    drop(held);
    assert!(
        a.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_none()
    );
    drop(session);
    assert!(
        a.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_some()
    );
}

#[test]
fn r05_incremental_object_order_and_key_identity_cannot_change() {
    let fixture = Fixture::new();
    let a = fixture.lock("a.lock");
    let b = fixture.lock("b.lock");
    let mut session = OrderedLocks::new(deadline());
    session
        .try_object("b", &b, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    assert!(session.try_object("a", &a, LeaseMode::Shared).is_err());
    assert!(session.try_object("b", &a, LeaseMode::Shared).is_err());
    assert!(session.acquire_project(&a).is_err());
}

#[test]
fn r05_catalog_wait_rechecks_earlier_object_identity() {
    let fixture = Fixture::new();
    let object = fixture.lock("object.lock");
    let catalog = fixture.lock("catalog.lock");
    let blocker = catalog.acquire(LeaseMode::Exclusive, deadline()).unwrap();
    let mut session = OrderedLocks::new(deadline());
    session
        .try_object("object", &object, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        result_tx
            .send(session.catalog(&catalog, LeaseMode::Exclusive))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(Duration::from_millis(75)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "catalog should wait behind the real exclusive lock"
    );
    fs::rename(
        fixture.directory.path().join("object.lock"),
        fixture.directory.path().join("saved.lock"),
    )
    .unwrap();
    let _replacement = fixture.lock("object.lock");
    drop(blocker);
    let outcome = result_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(
        outcome.is_err(),
        "catalog must reject an object lock replaced during the wait"
    );
}

#[test]
fn r05_cached_catalog_rechecks_project_and_object_identity() {
    for replace in ["project.lock", "object.lock"] {
        let fixture = Fixture::new();
        let project = fixture.lock("project.lock");
        let object = fixture.lock("object.lock");
        let catalog = fixture.lock("catalog.lock");
        let mut session = OrderedLocks::new(deadline());
        session.acquire_project(&project).unwrap();
        session
            .try_object("object", &object, LeaseMode::Shared)
            .unwrap()
            .unwrap();
        session.catalog(&catalog, LeaseMode::Exclusive).unwrap();
        fs::rename(
            fixture.directory.path().join(replace),
            fixture.directory.path().join("saved.lock"),
        )
        .unwrap();
        let _replacement = fixture.lock(replace);
        assert!(
            session.catalog(&catalog, LeaseMode::Exclusive).is_err(),
            "cached catalog must recheck every earlier protection"
        );
    }
}

#[test]
fn r05_object_batch_wait_rechecks_previously_acquired_lock() {
    let fixture = Fixture::new();
    let first = fixture.lock("a.lock");
    let second = fixture.lock("b.lock");
    let blocker = second.acquire(LeaseMode::Exclusive, deadline()).unwrap();
    let mut session = OrderedLocks::new(deadline());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        result_tx
            .send(session.acquire_objects(&[
                ("a", &first, LeaseMode::Shared),
                ("b", &second, LeaseMode::Shared),
            ]))
            .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_millis(75)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    fs::rename(
        fixture.directory.path().join("a.lock"),
        fixture.directory.path().join("saved.lock"),
    )
    .unwrap();
    let _replacement = fixture.lock("a.lock");
    drop(blocker);
    let outcome = result_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
    assert!(
        outcome.is_err(),
        "batch must reject an earlier object replaced while waiting for a later one"
    );
}

#[test]
fn r05_try_object_rechecks_existing_project_and_objects() {
    for replaced in ["project.lock", "a.lock"] {
        let fixture = Fixture::new();
        let project = fixture.lock("project.lock");
        let first = fixture.lock("a.lock");
        let second = fixture.lock("b.lock");
        let mut session = OrderedLocks::new(deadline());
        session.acquire_project(&project).unwrap();
        session
            .try_object("a", &first, LeaseMode::Shared)
            .unwrap()
            .unwrap();
        fs::rename(
            fixture.directory.path().join(replaced),
            fixture.directory.path().join("saved.lock"),
        )
        .unwrap();
        let _replacement = fixture.lock(replaced);
        assert!(
            session.try_object("b", &second, LeaseMode::Shared).is_err(),
            "new object needs all existing protections to remain bound"
        );
    }
}

#[test]
fn r05_object_budget_bounds_raw_batch_before_deduplication() {
    let fixture = Fixture::new();
    let lock = fixture.lock("a.lock");
    let mut session = OrderedLocks::with_limits(
        deadline(),
        LockLimits {
            max_objects: 2,
            max_total_key_bytes: 64,
        },
    )
    .unwrap();
    assert!(
        session
            .acquire_objects(&[("a", &lock, LeaseMode::Shared); 3])
            .is_err(),
        "duplicates still consume raw batch input budget"
    );
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_some(),
        "over-budget batch must not acquire any lock"
    );
}

#[test]
fn r05_key_byte_budget_bounds_raw_batch_before_deduplication() {
    let fixture = Fixture::new();
    let lock = fixture.lock("a.lock");
    let mut session = OrderedLocks::with_limits(
        deadline(),
        LockLimits {
            max_objects: 8,
            max_total_key_bytes: 5,
        },
    )
    .unwrap();
    assert!(
        session
            .acquire_objects(&[("abc", &lock, LeaseMode::Shared); 2])
            .is_err()
    );
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_some()
    );
}

#[test]
fn r05_object_budget_covers_incremental_retained_leases() {
    let fixture = Fixture::new();
    let first = fixture.lock("a.lock");
    let second = fixture.lock("b.lock");
    let mut session = OrderedLocks::with_limits(
        deadline(),
        LockLimits {
            max_objects: 1,
            max_total_key_bytes: 64,
        },
    )
    .unwrap();
    session
        .try_object("a", &first, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    assert!(session.try_object("b", &second, LeaseMode::Shared).is_err());
}

#[test]
fn r05_key_byte_budget_covers_incremental_retained_leases() {
    let fixture = Fixture::new();
    let first = fixture.lock("a.lock");
    let second = fixture.lock("b.lock");
    let mut session = OrderedLocks::with_limits(
        deadline(),
        LockLimits {
            max_objects: 8,
            max_total_key_bytes: 3,
        },
    )
    .unwrap();
    session
        .try_object("aa", &first, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    assert!(
        session
            .try_object("bb", &second, LeaseMode::Shared)
            .is_err()
    );
}

#[test]
fn r05_invalid_zero_lock_budgets_are_rejected() {
    for limits in [
        LockLimits {
            max_objects: 0,
            max_total_key_bytes: 4,
        },
        LockLimits {
            max_objects: 4,
            max_total_key_bytes: 0,
        },
    ] {
        assert!(OrderedLocks::with_limits(deadline(), limits).is_err());
    }
}

#[test]
fn r05_shared_lock_descriptors_are_readonly() {
    use nix::fcntl::{FcntlArg, fcntl};
    let fixture = Fixture::new();
    let created = fixture.lock("object.lock");
    let reopened = StableLock::open_existing(
        &fixture.parent,
        "object.lock",
        fixture.uid,
        created.identity(),
    )
    .unwrap();
    assert_eq!(
        fcntl(&reopened.0.pin, FcntlArg::F_GETFL).unwrap() & nix::libc::O_ACCMODE,
        nix::libc::O_RDONLY
    );
    let held = reopened.acquire(LeaseMode::Shared, deadline()).unwrap();
    assert_eq!(
        fcntl(&*held.0.held, FcntlArg::F_GETFL).unwrap() & nix::libc::O_ACCMODE,
        nix::libc::O_RDONLY
    );
}

const CHILD_PATH: &str = "__PAKRO_R05_LOCK_PATH";
const CHILD_READY: &str = "__PAKRO_R05_LOCK_READY";

#[test]
fn r05_process_lease_helper() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let parent = File::open(path.parent().unwrap()).unwrap();
    let pin = File::open(&path).unwrap();
    let lock = StableLock::open_existing(
        &parent,
        path.file_name().unwrap().to_str().unwrap(),
        nix::unistd::geteuid().as_raw(),
        FileIdentity::of(&pin).unwrap(),
    )
    .unwrap();
    let _held = lock.acquire(LeaseMode::Shared, deadline()).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(std::env::var_os(CHILD_READY).unwrap())
        .unwrap()
        .write_all(b"ready")
        .unwrap();
    std::io::stdin().read_exact(&mut [0]).unwrap();
}

struct ChildLease(Child);
impl Drop for ChildLease {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn r05_shared_reader_blocks_collector() {
    let fixture = Fixture::new();
    let lock = fixture.lock("object.lock");
    let ready = fixture.directory.path().join("ready");
    let helper = format!(
        "{}::r05_process_lease_helper",
        module_path!().split_once("::").unwrap().1
    );
    let mut child = ChildLease(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &helper, "--nocapture"])
            .env(CHILD_PATH, fixture.directory.path().join("object.lock"))
            .env(CHILD_READY, &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let expiry = deadline();
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before acquiring shared lock"
        );
        assert!(Instant::now() < expiry, "child did not acquire shared lock");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, expiry)
            .unwrap()
            .is_none()
    );
    assert!(
        lock.try_acquire(LeaseMode::Shared, expiry)
            .unwrap()
            .is_some()
    );
    child.0.stdin.take().unwrap().write_all(&[1]).unwrap();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < expiry, "child did not release shared lock");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, expiry)
            .unwrap()
            .is_some()
    );
}
