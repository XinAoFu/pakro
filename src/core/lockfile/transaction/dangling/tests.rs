use super::*;
use crate::lockfile::transaction::ProjectLockSession;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Fixture {
    _owner: tempfile::TempDir,
    parent: PathBuf,
    path: PathBuf,
    absent: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let parent = owner.path().canonicalize().unwrap().join("project");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("pakro.lock");
        let absent = parent.join("never-created.lock");
        symlink(&absent, &path).unwrap();
        Self {
            _owner: owner,
            parent,
            path,
            absent,
        }
    }
    fn capture(&self) -> PreparedNativeDanglingLock {
        PreparedNativeDanglingLock::capture(&self.path)
            .expect("actual production dangling capture prerequisite")
    }
    fn still_dangling(&self) {
        assert!(fs::symlink_metadata(&self.path).unwrap().is_symlink());
        assert!(!self.absent.exists());
    }
    fn guard(&self) -> PathBuf {
        self.parent.join(".pakro.lock.guard")
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn capture_is_readonly_and_publication_replaces_only_the_original_symlink() {
    let f = Fixture::new();
    let original = fs::symlink_metadata(&f.path).unwrap();
    let cap = f.capture();
    assert_eq!(fs::symlink_metadata(&f.path).unwrap().ino(), original.ino());
    assert!(!f.guard().exists());
    f.still_dangling();
    let oracle = f.parent.join("mode-oracle");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&oracle)
        .unwrap();
    let mode = fs::metadata(&oracle).unwrap().mode() & 0o777;
    fs::remove_file(oracle).unwrap();
    assert!(
        ProjectLockSession::acquire(&f.path, 1024, deadline()).is_err(),
        "native cap must not weaken strict acquire"
    );
    cap.publish(b"native lock bytes\n", 1024, deadline())
        .unwrap();
    let now = fs::symlink_metadata(&f.path).unwrap();
    assert!(now.is_file());
    assert_ne!(now.ino(), original.ino());
    assert_eq!(now.mode() & 0o777, mode);
    assert_eq!(fs::read(&f.path).unwrap(), b"native lock bytes\n");
    assert!(!f.absent.exists());
    assert!(f.guard().is_file());
    let session = ProjectLockSession::acquire(&f.path, 1024, deadline()).unwrap();
    assert_eq!(
        session.snapshot().unwrap().bytes(),
        Some(&b"native lock bytes\n"[..])
    );
}

#[test]
fn capture_accepts_only_real_notfound_symlinks_and_never_permission_or_loop_errors() {
    for kind in [
        "missing",
        "regular",
        "existing",
        "loop",
        "permission",
        "reserved",
    ] {
        let f = Fixture::new();
        fs::remove_file(&f.path).unwrap();
        let mut restore = None;
        match kind {
            "missing" => {}
            "regular" => fs::write(&f.path, b"private unchanged").unwrap(),
            "existing" => {
                fs::write(&f.absent, b"target unchanged").unwrap();
                symlink(&f.absent, &f.path).unwrap();
            }
            "loop" => symlink(&f.path, &f.path).unwrap(),
            "permission" => {
                let blocked = f.parent.join("blocked");
                fs::create_dir(&blocked).unwrap();
                symlink(blocked.join("missing"), &f.path).unwrap();
                fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
                restore = Some(blocked);
            }
            "reserved" => symlink(&f.absent, f.guard()).unwrap(),
            _ => unreachable!(),
        }
        let path = if kind == "reserved" {
            f.guard()
        } else {
            f.path.clone()
        };
        if kind == "permission" {
            assert_eq!(
                fs::canonicalize(&path).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied,
                "actual permission oracle is required"
            );
        }
        if kind == "loop" {
            assert_ne!(
                fs::canonicalize(&path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
        let result = PreparedNativeDanglingLock::capture(&path);
        if let Some(dir) = restore {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(result.is_err(), "{kind}");
        if kind == "regular" {
            assert_eq!(fs::read(&f.path).unwrap(), b"private unchanged");
        }
        if kind == "existing" {
            assert_eq!(fs::read(&f.absent).unwrap(), b"target unchanged");
        }
    }
}

#[test]
fn original_link_parent_and_target_changes_revoke_a_captured_native_capability() {
    for kind in ["link", "parent", "target"] {
        let f = Fixture::new();
        let cap = f.capture();
        match kind {
            "link" => {
                fs::rename(&f.path, f.parent.join("saved-link")).unwrap();
                symlink(&f.absent, &f.path).unwrap();
            }
            "parent" => {
                fs::rename(&f.parent, f.parent.with_extension("saved")).unwrap();
                fs::create_dir(&f.parent).unwrap();
                symlink(&f.absent, &f.path).unwrap();
            }
            "target" => fs::write(&f.absent, b"external target\n").unwrap(),
            _ => unreachable!(),
        }
        assert!(
            cap.publish(b"must not replace\n", 1024, deadline())
                .is_err(),
            "{kind}"
        );
        assert!(fs::symlink_metadata(&f.path).unwrap().is_symlink());
        if kind == "target" {
            assert_eq!(fs::read(&f.absent).unwrap(), b"external target\n");
        } else {
            assert!(!f.absent.exists());
        }
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn native_dangling_child_helper() {
    let Some(path) = std::env::var_os("PAKRO_DANGLING_CHILD_PATH") else {
        return;
    };
    let session = ProjectLockSession::acquire(
        Path::new(&path),
        1024,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    fs::write(
        std::env::var_os("PAKRO_DANGLING_CHILD_READY").unwrap(),
        b"ready",
    )
    .unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    drop(session);
}
#[test]
fn native_publication_and_a_real_strict_child_use_the_same_permanent_parent_guard() {
    let f = Fixture::new();
    let cap = f.capture();
    let other = f.parent.join("other-project.lock");
    fs::write(&other, b"other actual lock\n").unwrap();
    let ready = f.parent.join("child-ready");
    let name = std::thread::current().name().unwrap().to_owned();
    let helper = format!(
        "{}::native_dangling_child_helper",
        name.rsplit_once("::").unwrap().0
    );
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &helper, "--nocapture"])
            .env("PAKRO_DANGLING_CHILD_PATH", &other)
            .env("PAKRO_DANGLING_CHILD_READY", &ready)
            .env("__PAKRO_TEST_FIXTURES_READY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    let guard = fs::metadata(f.guard()).unwrap();
    let started = Instant::now();
    assert!(
        cap.publish(b"blocked\n", 1024, started + Duration::from_millis(100))
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    f.still_dangling();
    child.0.stdin.take().unwrap().write_all(&[1]).unwrap();
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    f.capture()
        .publish(b"after real release\n", 1024, deadline())
        .unwrap();
    assert_eq!(fs::read(&f.path).unwrap(), b"after real release\n");
    assert_eq!(fs::metadata(f.guard()).unwrap().ino(), guard.ino());
    assert!(!f.absent.exists());
}

#[test]
fn late_changes_retain_foreign_entries_and_only_clean_their_own_temporary_inode() {
    for kind in ["link", "guard", "parent", "target", "temporary"] {
        let f = Fixture::new();
        let cap = f.capture();
        let mut observed = None;
        let result = cap.publish_with_hook(b"native new bytes\n", 1024, deadline(), |temporary| {
            observed = Some(temporary.to_owned());
            assert_eq!(fs::read(temporary).unwrap(), b"native new bytes\n");
            match kind {
                "link" => {
                    fs::rename(&f.path, f.parent.join("saved-link")).unwrap();
                    symlink(&f.absent, &f.path).unwrap();
                }
                "guard" => {
                    fs::rename(f.guard(), f.parent.join("saved-guard")).unwrap();
                    fs::write(f.guard(), []).unwrap();
                    fs::set_permissions(f.guard(), fs::Permissions::from_mode(0o600)).unwrap();
                }
                "parent" => {
                    fs::rename(&f.parent, f.parent.with_extension("saved")).unwrap();
                    fs::create_dir(&f.parent).unwrap();
                    symlink(&f.absent, &f.path).unwrap();
                }
                "target" => fs::write(&f.absent, b"external target\n").unwrap(),
                "temporary" => {
                    fs::rename(temporary, f.parent.join("saved-temporary")).unwrap();
                    fs::write(temporary, b"foreign temporary\n").unwrap();
                }
                _ => unreachable!(),
            }
        });
        assert!(result.is_err(), "{kind}");
        let temporary = observed.expect("actual precommit late hook must run");
        assert!(fs::symlink_metadata(&f.path).unwrap().is_symlink());
        if kind == "temporary" {
            assert_eq!(fs::read(&temporary).unwrap(), b"foreign temporary\n");
            assert_eq!(
                fs::read(f.parent.join("saved-temporary")).unwrap(),
                b"native new bytes\n"
            );
        } else if kind == "parent" {
            assert!(
                !f.parent
                    .with_extension("saved")
                    .join(temporary.file_name().unwrap())
                    .exists()
            );
        } else {
            assert!(!temporary.exists());
        }
        if kind == "target" {
            assert_eq!(fs::read(&f.absent).unwrap(), b"external target\n");
        } else {
            assert!(!f.absent.exists());
        }
    }
}

#[test]
fn native_zero_overflow_byte_and_deadline_limits_fail_without_replacing_the_original_link() {
    for (max, past) in [(0, false), (usize::MAX, false), (3, false), (1024, true)] {
        let f = Fixture::new();
        let cap = f.capture();
        let limit = if past {
            Instant::now() - Duration::from_millis(1)
        } else {
            deadline()
        };
        assert!(cap.publish(b"long bytes\n", max, limit).is_err());
        f.still_dangling();
        assert!(!f.guard().exists());
    }
}
