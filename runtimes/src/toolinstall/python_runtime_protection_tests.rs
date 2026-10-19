use super::*;
use pakro_store::control::lease::{LeaseMode, lock_file_with_deadline, try_lock_file};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
struct Protection {
    revoked: Arc<AtomicBool>,
    marker: PathBuf,
    _lock: nix::fcntl::Flock<File>,
}
impl RuntimeProtection for Protection {
    fn check_binding(&self) -> Result<()> {
        ensure!(
            !self.revoked.load(Ordering::SeqCst) && !self.marker.exists(),
            "runtime protection revoked"
        );
        Ok(())
    }
}
struct Fixture {
    _owner: tempfile::TempDir,
    executable: PathBuf,
    lock_path: PathBuf,
    marker: PathBuf,
    revoked: Arc<AtomicBool>,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let value = Self {
            executable: root.join("python"),
            lock_path: root.join("stable.lock"),
            marker: root.join("ran"),
            revoked: Arc::new(AtomicBool::new(false)),
            _owner: owner,
        };
        value.script("printf 'fixture'");
        value
    }
    fn script(&self, body: &str) {
        fs::write(&self.executable, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&self.executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn handle(&self) -> PythonRuntimeHandle {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock_path)
            .unwrap();
        let guard = Protection {
            revoked: self.revoked.clone(),
            marker: self.marker.clone(),
            _lock: lock_file_with_deadline(
                file,
                LeaseMode::Shared,
                std::time::Instant::now() + Duration::from_secs(5),
            )
            .unwrap(),
        };
        PythonRuntimeHandle::from_protected_executable(&self.executable, Arc::new(guard)).unwrap()
    }
    fn exclusive_available(&self) -> bool {
        try_lock_file(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.lock_path)
                .unwrap(),
            LeaseMode::Exclusive,
        )
        .unwrap()
        .is_some()
    }
}

#[test]
fn r06_handle_clone_retains_actual_shared_flock_until_last_user_drops() {
    let fixture = Fixture::new();
    let handle = fixture.handle();
    let clone = handle.clone();
    assert!(!fixture.exclusive_available());
    drop(handle);
    assert!(!fixture.exclusive_available());
    drop(clone);
    assert!(fixture.exclusive_available());
}

#[tokio::test]
async fn r06_revoked_runtime_protection_rejects_all_probe_launches() {
    let fixture = Fixture::new();
    fixture.script(&format!(
        "printf 'ran' > '{}'\nprintf 'fixture'",
        fixture.marker.display()
    ));
    let handle = fixture.handle();
    fixture.revoked.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(
        probe_at_deadline(&handle, deadline, 1024, 1024)
            .await
            .is_err()
    );
    assert!(
        !fixture.marker.exists(),
        "base probe launched after revocation"
    );
    assert!(
        probe_layout(&handle, Path::new("/unused"), 1024, 1024, deadline)
            .await
            .is_err()
    );
    assert!(
        !fixture.marker.exists(),
        "layout probe launched after revocation"
    );
    assert!(
        probe_startup_intercept(&handle, b"{}".to_vec(), 1024, 1024, deadline)
            .await
            .is_err()
    );
    assert!(
        !fixture.marker.exists(),
        "startup probe launched after revocation"
    );
    assert!(
        probe_empty_environment(&handle, b"{}".to_vec(), 1024, 1024, deadline)
            .await
            .is_err()
    );
    assert!(
        !fixture.marker.exists(),
        "empty probe launched after revocation"
    );
}

#[tokio::test]
async fn r06_runtime_protection_rechecks_after_layout_and_startup_processes() {
    for kind in ["layout", "startup", "empty"] {
        let fixture = Fixture::new();
        fixture.script(&format!(
            "cat >/dev/null\nprintf 'ran' > '{}'\nprintf 'fixture'",
            fixture.marker.display()
        ));
        let handle = fixture.handle();
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = match kind {
            "layout" => probe_layout(&handle, Path::new("/unused"), 1024, 1024, deadline).await,
            "startup" => {
                probe_startup_intercept(&handle, b"{}".to_vec(), 1024, 1024, deadline).await
            }
            _ => probe_empty_environment(&handle, b"{}".to_vec(), 1024, 1024, deadline).await,
        };
        assert!(fixture.marker.exists());
        assert!(
            result.is_err(),
            "{kind} accepted bytes after its protected binding changed"
        );
    }
}

#[derive(Debug)]
struct SlowProtection {
    armed: AtomicBool,
    executable: PathBuf,
}
impl RuntimeProtection for SlowProtection {
    fn check_binding(&self) -> Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(40));
            // A launch after the guard returns would now produce ENOENT. This
            // distinguishes an actual spawn attempt from deadline rejection.
            fs::remove_file(&self.executable)?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn r06_protection_cannot_consume_deadline_then_launch_a_runtime() {
    let mut errors = Vec::new();
    for kind in ["base", "layout", "startup", "empty"] {
        let fixture = Fixture::new();
        let protection = Arc::new(SlowProtection {
            armed: AtomicBool::new(false),
            executable: fixture.executable.clone(),
        });
        let handle =
            PythonRuntimeHandle::from_protected_executable(&fixture.executable, protection.clone())
                .unwrap();
        protection.armed.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_millis(10);
        let result = match kind {
            "base" => probe_at_deadline(&handle, deadline, 1024, 1024)
                .await
                .map(|_| ()),
            "layout" => probe_layout(&handle, Path::new("/unused"), 1024, 1024, deadline)
                .await
                .map(|_| ()),
            "startup" => probe_startup_intercept(&handle, b"{}".to_vec(), 1024, 1024, deadline)
                .await
                .map(|_| ()),
            _ => probe_empty_environment(&handle, b"{}".to_vec(), 1024, 1024, deadline)
                .await
                .map(|_| ()),
        };
        errors.push((kind, format!("{:#}", result.unwrap_err())));
    }
    for (kind, error) in errors {
        assert!(
            error.contains("timed out"),
            "{kind} attempted launch after the deadline: {error}"
        );
    }
}
