use super::*;
use nix::fcntl::{Flock, FlockArg};
use nix::sys::stat::Mode;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::process::Command;
use std::time::Duration;

struct Fixture {
    _owner: tempfile::TempDir,
    parent: PathBuf,
    target: PathBuf,
}
impl Fixture {
    fn new(body: Option<&[u8]>) -> Self {
        let owner = tempfile::tempdir().unwrap();
        let parent = owner.path().canonicalize().unwrap().join("project");
        fs::create_dir(&parent).unwrap();
        let target = parent.join("pakro.lock");
        if let Some(body) = body {
            fs::write(&target, body).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        }
        Self {
            _owner: owner,
            parent,
            target,
        }
    }
    fn acquire(&self) -> ProjectLockSession {
        ProjectLockSession::acquire(&self.target, 1024, Instant::now() + Duration::from_secs(3))
            .expect("real production project-lock session prerequisite")
    }
    fn guard(&self) -> PathBuf {
        self.parent.join(".pakro.lock.guard")
    }
    fn names(&self) -> Vec<std::ffi::OsString> {
        let mut names = fs::read_dir(&self.parent)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

#[test]
fn missing_is_an_actual_session_image_and_can_be_created_atomically() {
    let f = Fixture::new(None);
    let mode_oracle = f.parent.join("mode-oracle");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&mode_oracle)
        .unwrap();
    let expected_mode = fs::metadata(&mode_oracle).unwrap().mode() & 0o777;
    fs::remove_file(mode_oracle).unwrap();
    let mut session = f.acquire();
    assert_eq!(session.canonical_target(), f.target);
    let image = session.snapshot().unwrap();
    assert!(!image.exists());
    assert_eq!(image.bytes(), None);
    assert_eq!(image.sha256(), None);
    session.recheck(&image).unwrap();
    let next = session.compare_and_replace(&image, b"new lock\n").unwrap();
    assert!(next.exists());
    assert_eq!(next.bytes(), Some(&b"new lock\n"[..]));
    assert_eq!(
        next.sha256(),
        Some(&<[u8; 32]>::from(Sha256::digest(b"new lock\n")))
    );
    session.recheck(&next).unwrap();
    assert_eq!(fs::read(&f.target).unwrap(), b"new lock\n");
    assert_eq!(
        fs::metadata(&f.target).unwrap().mode() & 0o777,
        expected_mode
    );
    assert_eq!(
        f.names(),
        vec![
            std::ffi::OsString::from(".pakro.lock.guard"),
            std::ffi::OsString::from("pakro.lock")
        ]
    );
}

#[test]
fn replacement_keeps_normal_shared_repository_modes_and_the_old_inode_is_not_chmodded() {
    for mode in [0o644, 0o664] {
        let f = Fixture::new(Some(b"old lock\n"));
        fs::set_permissions(&f.target, fs::Permissions::from_mode(mode)).unwrap();
        let mut old = File::open(&f.target).unwrap();
        let old_inode = old.metadata().unwrap().ino();
        let mut session = f.acquire();
        let image = session.snapshot().unwrap();
        assert_eq!(image.bytes(), Some(&b"old lock\n"[..]));
        let next = session.compare_and_replace(&image, b"new lock\n").unwrap();
        assert_ne!(fs::metadata(&f.target).unwrap().ino(), old_inode);
        assert_eq!(fs::metadata(&f.target).unwrap().mode() & 0o777, mode);
        assert_eq!(old.metadata().unwrap().mode() & 0o777, mode);
        let mut bytes = Vec::new();
        old.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"old lock\n");
        assert_eq!(next.bytes(), Some(&b"new lock\n"[..]));
        assert!(session.recheck(&image).is_err());
        session.recheck(&next).unwrap();
    }
}

#[test]
fn readonly_recheck_preserves_current_bytes_metadata_and_permanent_guard() {
    let f = Fixture::new(Some(b"private lock body\n"));
    let session = f.acquire();
    let image = session.snapshot().unwrap();
    let before = fs::metadata(&f.target).unwrap();
    let guard = fs::metadata(f.guard()).unwrap();
    session.recheck(&image).unwrap();
    session.recheck(&image).unwrap();
    let after = fs::metadata(&f.target).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        )
    );
    assert_eq!(guard.mode() & 0o7777, 0o600);
    assert_eq!(guard.nlink(), 1);
    assert_eq!(guard.len(), 0);
    let independent = OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.guard())
        .unwrap();
    assert!(Flock::lock(independent, FlockArg::LockExclusiveNonblock).is_err());
    drop(session);
    assert_eq!(fs::metadata(f.guard()).unwrap().ino(), guard.ino());
    let independent = OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.guard())
        .unwrap();
    assert!(Flock::lock(independent, FlockArg::LockExclusiveNonblock).is_ok());
    assert_eq!(fs::read(f.target).unwrap(), b"private lock body\n");
}

#[test]
fn a_same_byte_new_inode_is_stale_for_both_readonly_recheck_and_cas() {
    let f = Fixture::new(Some(b"same bytes\n"));
    let mut session = f.acquire();
    let image = session.snapshot().unwrap();
    let old = f.target.with_extension("saved");
    fs::rename(&f.target, &old).unwrap();
    fs::write(&f.target, b"same bytes\n").unwrap();
    assert_ne!(
        fs::metadata(&old).unwrap().ino(),
        fs::metadata(&f.target).unwrap().ino()
    );
    assert!(session.recheck(&image).is_err());
    assert!(session.compare_and_replace(&image, b"unwanted\n").is_err());
    assert_eq!(fs::read(&f.target).unwrap(), b"same bytes\n");
    let fresh = session.snapshot().unwrap();
    session.compare_and_replace(&fresh, b"fresh\n").unwrap();
}

#[test]
fn same_inode_changed_bytes_reject_the_old_image_without_poisoning_a_new_read() {
    let f = Fixture::new(Some(b"initial bytes\n"));
    let mut session = f.acquire();
    let image = session.snapshot().unwrap();
    let inode = fs::metadata(&f.target).unwrap().ino();
    fs::write(&f.target, b"changed bytes\n").unwrap();
    assert_eq!(inode, fs::metadata(&f.target).unwrap().ino());
    assert!(session.recheck(&image).is_err());
    assert!(
        session
            .compare_and_replace(&image, b"must not win\n")
            .is_err()
    );
    assert_eq!(fs::read(&f.target).unwrap(), b"changed bytes\n");
    let fresh = session.snapshot().unwrap();
    session
        .compare_and_replace(&fresh, b"fresh read wins\n")
        .unwrap();
}

#[test]
fn a_missing_image_cannot_overwrite_a_file_created_since_the_read() {
    let f = Fixture::new(None);
    let mut session = f.acquire();
    let image = session.snapshot().unwrap();
    fs::write(&f.target, b"external\n").unwrap();
    assert!(session.recheck(&image).is_err());
    assert!(session.compare_and_replace(&image, b"wrong\n").is_err());
    assert_eq!(fs::read(&f.target).unwrap(), b"external\n");
    session.snapshot().unwrap();
}

#[test]
fn an_image_from_another_session_is_not_an_authorization_even_for_the_same_target() {
    for body in [None, Some(&b"same target\n"[..])] {
        let f = Fixture::new(body);
        let first = f.acquire();
        let foreign = first.snapshot().unwrap();
        drop(first);
        let mut second = f.acquire();
        assert!(second.recheck(&foreign).is_err());
        assert!(second.compare_and_replace(&foreign, b"forged\n").is_err());
        let own = second.snapshot().unwrap();
        second.compare_and_replace(&own, b"own image\n").unwrap();
    }
}

#[test]
fn an_existing_leaf_symlink_updates_its_actual_target_without_replacing_the_link() {
    let f = Fixture::new(Some(b"old\n"));
    let alias_parent = f._owner.path().canonicalize().unwrap().join("alias");
    fs::create_dir(&alias_parent).unwrap();
    let alias = alias_parent.join("pakro.lock");
    symlink(&f.target, &alias).unwrap();
    let link = fs::symlink_metadata(&alias).unwrap();
    let mut session =
        ProjectLockSession::acquire(&alias, 1024, Instant::now() + Duration::from_secs(3)).unwrap();
    assert_eq!(session.canonical_target(), f.target);
    let image = session.snapshot().unwrap();
    session.compare_and_replace(&image, b"new\n").unwrap();
    assert_eq!(fs::read(&alias).unwrap(), b"new\n");
    assert!(fs::symlink_metadata(&alias).unwrap().is_symlink());
    assert_eq!(fs::symlink_metadata(&alias).unwrap().ino(), link.ino());
    assert!(!alias_parent.join(".pakro.lock.guard").exists());
}

fn child(path: &Path, blocked: bool) {
    let name = std::thread::current().name().unwrap().to_owned();
    let helper = format!(
        "{}::project_lock_child_helper",
        name.rsplit_once("::").unwrap().0
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &helper, "--nocapture"])
        .env("PAKRO_LOCK_IO_CHILD_PATH", path)
        .env(
            "PAKRO_LOCK_IO_CHILD_BLOCKED",
            if blocked { "yes" } else { "no" },
        )
        .env("__PAKRO_TEST_FIXTURES_READY", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn project_lock_child_helper() {
    let Some(path) = std::env::var_os("PAKRO_LOCK_IO_CHILD_PATH") else {
        return;
    };
    let blocked = std::env::var("PAKRO_LOCK_IO_CHILD_BLOCKED").unwrap() == "yes";
    let started = Instant::now();
    let result =
        ProjectLockSession::acquire(Path::new(&path), 1024, started + Duration::from_millis(150));
    assert_eq!(
        result.is_err(),
        blocked,
        "actual child lock contention state disagreed"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    if let Ok(session) = result {
        session.snapshot().unwrap();
    }
}

#[test]
fn actual_child_exclusion_is_shared_by_directory_aliases_and_other_leaf_names() {
    let f = Fixture::new(Some(b"body\n"));
    let alias = f
        ._owner
        .path()
        .canonicalize()
        .unwrap()
        .join("directory-alias");
    symlink(&f.parent, &alias).unwrap();
    let other = f.parent.join("pakro.other.lock");
    fs::write(&other, b"other\n").unwrap();
    let session = f.acquire();
    child(&alias.join("pakro.lock"), true);
    child(&other, true);
    drop(session);
    child(&alias.join("pakro.lock"), false);
    child(&other, false);
    assert!(f.guard().is_file());
}

#[test]
fn exact_byte_limits_work_and_new_or_current_max_plus_one_is_rejected() {
    let f = Fixture::new(Some(b"12345678"));
    assert!(
        ProjectLockSession::acquire(&f.target, 7, Instant::now() + Duration::from_secs(1)).is_err()
    );
    let mut session =
        ProjectLockSession::acquire(&f.target, 8, Instant::now() + Duration::from_secs(3)).unwrap();
    let image = session.snapshot().unwrap();
    assert_eq!(image.bytes().unwrap().len(), 8);
    assert!(session.compare_and_replace(&image, b"123456789").is_err());
    assert_eq!(fs::read(&f.target).unwrap(), b"12345678");
    session.recheck(&image).unwrap();
    session.compare_and_replace(&image, b"87654321").unwrap();
}

#[test]
fn invalid_limits_and_expired_deadlines_fail_without_file_or_guard_creation() {
    for (limit, past) in [(0, false), (usize::MAX, false), (1024, true)] {
        let f = Fixture::new(None);
        let deadline = if past {
            Instant::now() - Duration::from_millis(1)
        } else {
            Instant::now() + Duration::from_secs(1)
        };
        assert!(ProjectLockSession::acquire(&f.target, limit, deadline).is_err());
        assert!(f.names().is_empty());
    }
}

#[test]
fn dangling_links_special_targets_hardlinks_and_privbits_are_not_missing_images() {
    for kind in ["dangling", "fifo", "directory", "hardlink", "privbits"] {
        let f = Fixture::new(None);
        match kind {
            "dangling" => symlink(f.parent.join("absent"), &f.target).unwrap(),
            "fifo" => nix::unistd::mkfifo(&f.target, Mode::from_bits_truncate(0o600)).unwrap(),
            "directory" => fs::create_dir(&f.target).unwrap(),
            "hardlink" => {
                fs::write(&f.target, b"body").unwrap();
                fs::hard_link(&f.target, f.parent.join("other")).unwrap();
            }
            "privbits" => {
                fs::write(&f.target, b"body").unwrap();
                fs::set_permissions(&f.target, fs::Permissions::from_mode(0o4644)).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&f.target).unwrap();
        let started = Instant::now();
        assert!(
            ProjectLockSession::acquire(&f.target, 1024, started + Duration::from_millis(150))
                .is_err(),
            "{kind}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let after = fs::symlink_metadata(&f.target).unwrap();
        assert_eq!(
            (before.ino(), before.mode()),
            (after.ino(), after.mode()),
            "{kind}"
        );
    }
}

#[test]
fn unsafe_existing_guards_are_rejected_without_repair_or_unlink() {
    for kind in [
        "symlink",
        "fifo",
        "directory",
        "hardlink",
        "mode",
        "bytes",
        "privbits",
    ] {
        let f = Fixture::new(Some(b"untouched\n"));
        let guard = f.guard();
        match kind {
            "symlink" => symlink(&f.target, &guard).unwrap(),
            "fifo" => nix::unistd::mkfifo(&guard, Mode::from_bits_truncate(0o600)).unwrap(),
            "directory" => fs::create_dir(&guard).unwrap(),
            _ => {
                fs::write(&guard, if kind == "bytes" { &b"x"[..] } else { &[][..] }).unwrap();
                fs::set_permissions(
                    &guard,
                    fs::Permissions::from_mode(if kind == "mode" {
                        0o644
                    } else if kind == "privbits" {
                        0o4600
                    } else {
                        0o600
                    }),
                )
                .unwrap();
                if kind == "hardlink" {
                    fs::hard_link(&guard, f.parent.join("guard-alias")).unwrap();
                }
            }
        }
        let before = fs::symlink_metadata(&guard).unwrap();
        assert!(
            ProjectLockSession::acquire(
                &f.target,
                1024,
                Instant::now() + Duration::from_millis(150)
            )
            .is_err(),
            "{kind}"
        );
        let after = fs::symlink_metadata(&guard).unwrap();
        assert_eq!(
            (before.ino(), before.mode(), before.len(), before.nlink()),
            (after.ino(), after.mode(), after.len(), after.nlink())
        );
        assert_eq!(fs::read(&f.target).unwrap(), b"untouched\n");
    }
}

#[test]
fn parent_and_permanent_guard_replacements_invalidate_an_existing_session() {
    for guard in [false, true] {
        let f = Fixture::new(Some(b"old\n"));
        let session = f.acquire();
        let image = session.snapshot().unwrap();
        if guard {
            fs::rename(f.guard(), f.parent.join("saved-guard")).unwrap();
            fs::write(f.guard(), []).unwrap();
            fs::set_permissions(f.guard(), fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            fs::rename(&f.parent, f.parent.with_extension("saved")).unwrap();
            fs::create_dir(&f.parent).unwrap();
            fs::write(&f.target, b"external\n").unwrap();
        }
        assert!(session.recheck(&image).is_err());
        drop(session);
        if guard {
            assert!(f.guard().exists());
        } else {
            assert_eq!(fs::read(&f.target).unwrap(), b"external\n");
        }
    }
}

#[test]
fn original_leaf_symlink_and_its_parent_are_pinned_even_when_the_target_is_unchanged() {
    for parent in [false, true] {
        let f = Fixture::new(Some(b"target\n"));
        let dir = f._owner.path().canonicalize().unwrap().join("alias");
        fs::create_dir(&dir).unwrap();
        let alias = dir.join("pakro.lock");
        symlink(&f.target, &alias).unwrap();
        let session =
            ProjectLockSession::acquire(&alias, 1024, Instant::now() + Duration::from_secs(3))
                .unwrap();
        let image = session.snapshot().unwrap();
        if parent {
            fs::rename(&dir, dir.with_extension("saved")).unwrap();
            fs::create_dir(&dir).unwrap();
        } else {
            fs::rename(&alias, dir.join("saved-link")).unwrap();
        }
        symlink(&f.target, &alias).unwrap();
        assert!(session.recheck(&image).is_err());
        assert_eq!(fs::read(&f.target).unwrap(), b"target\n");
    }
}

#[test]
fn late_changes_fail_closed_and_only_proven_owned_temporary_entries_are_cleaned() {
    for kind in ["target", "guard", "parent", "temporary"] {
        let f = Fixture::new(Some(b"old\n"));
        let mut session = f.acquire();
        let image = session.snapshot().unwrap();
        let mut observed = None;
        let result = session.compare_with_hook(&image, b"new\n", |temporary| {
            observed = Some(temporary.to_owned());
            assert_eq!(fs::read(temporary).unwrap(), b"new\n");
            match kind {
                "target" => fs::write(&f.target, b"external\n").unwrap(),
                "guard" => {
                    fs::rename(f.guard(), f.parent.join("saved-guard")).unwrap();
                    fs::write(f.guard(), []).unwrap();
                    fs::set_permissions(f.guard(), fs::Permissions::from_mode(0o600)).unwrap();
                }
                "parent" => {
                    fs::rename(&f.parent, f.parent.with_extension("saved")).unwrap();
                    fs::create_dir(&f.parent).unwrap();
                    fs::write(&f.target, b"external\n").unwrap();
                }
                "temporary" => {
                    fs::rename(temporary, f.parent.join("saved-temporary")).unwrap();
                    fs::write(temporary, b"external temporary\n").unwrap();
                }
                _ => unreachable!(),
            }
        });
        assert!(result.is_err(), "{kind}");
        let temporary = observed.expect("actual late hook must be reached");
        assert!(
            session
                .compare_and_replace(&image, b"retry forbidden\n")
                .is_err(),
            "{kind}"
        );
        drop(session);
        match kind {
            "temporary" => {
                assert_eq!(fs::read(temporary).unwrap(), b"external temporary\n");
                assert_eq!(
                    fs::read(f.parent.join("saved-temporary")).unwrap(),
                    b"new\n"
                );
                assert_eq!(fs::read(&f.target).unwrap(), b"old\n");
            }
            "parent" => {
                assert_eq!(fs::read(&f.target).unwrap(), b"external\n");
                assert!(
                    !f.parent
                        .with_extension("saved")
                        .join(temporary.file_name().unwrap())
                        .exists()
                );
            }
            "target" => {
                assert_eq!(fs::read(&f.target).unwrap(), b"external\n");
                assert!(!temporary.exists());
            }
            "guard" => {
                assert_eq!(fs::read(&f.target).unwrap(), b"old\n");
                assert!(!temporary.exists());
                assert!(f.guard().exists());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn image_debug_and_failures_do_not_disclose_lock_contents() {
    let body = b"credential-like-private-lock-value\n";
    let f = Fixture::new(Some(body));
    let mut session = f.acquire();
    let image = session.snapshot().unwrap();
    assert!(!format!("{image:?}").contains("credential-like"));
    fs::write(&f.target, b"changed\n").unwrap();
    let error = session
        .compare_and_replace(&image, b"unwanted\n")
        .unwrap_err();
    assert!(!format!("{error:#}").contains("credential-like"));
    assert_eq!(fs::read(&f.target).unwrap(), b"changed\n");
}

#[test]
fn guard_initialization_does_not_invalidate_a_concurrent_existing_open() {
    use std::sync::mpsc::channel;
    let f = Fixture::new(Some(b"original\n"));
    let (created_tx, created_rx) = channel();
    let (initialize_tx, initialize_rx) = channel();
    let path = f.target.clone();
    let creator = std::thread::spawn(move || {
        ProjectLockSession::acquire_with_guard_hook(
            &path,
            1024,
            Instant::now() + Duration::from_secs(3),
            |stage| {
                if stage == GuardAcquireStage::CreatedBeforeInitialize {
                    created_tx.send(()).unwrap();
                    initialize_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                }
            },
        )
    });
    created_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let before = fs::metadata(f.guard()).unwrap();
    assert_eq!(
        before.mode() & 0o7777,
        0o600,
        "actual mode600 initialization-race oracle is required"
    );
    let (observed_tx, observed_rx) = channel();
    let (check_tx, check_rx) = channel();
    let path = f.target.clone();
    let observer = std::thread::spawn(move || {
        ProjectLockSession::acquire_with_guard_hook(
            &path,
            1024,
            Instant::now() + Duration::from_secs(3),
            |stage| {
                if stage == GuardAcquireStage::ObservedBeforeCheck {
                    observed_tx.send(()).unwrap();
                    check_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                }
            },
        )
    });
    observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    initialize_tx.send(()).unwrap();
    let owner = creator
        .join()
        .unwrap()
        .expect("actual creator session prerequisite");
    let after = fs::metadata(f.guard()).unwrap();
    eprintln!(
        "guard initialization oracle: before(dev={},ino={},mode={:o},uid={},gid={},links={},size={},mtime={}.{},ctime={}.{}), after(dev={},ino={},mode={:o},uid={},gid={},links={},size={},mtime={}.{},ctime={}.{})",
        before.dev(),
        before.ino(),
        before.mode(),
        before.uid(),
        before.gid(),
        before.nlink(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
        after.dev(),
        after.ino(),
        after.mode(),
        after.uid(),
        after.gid(),
        after.nlink(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec()
    );
    drop(owner);
    check_tx.send(()).unwrap();
    let observed = observer
        .join()
        .unwrap()
        .expect("the real creator must not invalidate an already observed permanent guard");
    observed.snapshot().unwrap();
}

#[test]
fn eight_real_clients_can_each_complete_twenty_serialized_guarded_replacements() {
    use std::sync::{Arc, Barrier};
    let f = Fixture::new(Some(b"initial\n"));
    let barrier = Arc::new(Barrier::new(8));
    let clients = (0..8)
        .map(|client| {
            let barrier = Arc::clone(&barrier);
            let path = f.target.clone();
            std::thread::spawn(move || -> std::result::Result<(), String> {
                barrier.wait();
                for round in 0..20 {
                    let mut session = ProjectLockSession::acquire(
                        &path,
                        1024,
                        Instant::now() + Duration::from_secs(3),
                    )
                    .map_err(|e| format!("client={client} round={round} acquire: {e:#}"))?;
                    let image = session
                        .snapshot()
                        .map_err(|e| format!("client={client} round={round} read: {e:#}"))?;
                    session
                        .compare_and_replace(
                            &image,
                            format!("client={client} round={round}\n").as_bytes(),
                        )
                        .map_err(|e| format!("client={client} round={round} replace: {e:#}"))?;
                }
                Ok(())
            })
        })
        .collect::<Vec<_>>();
    // Join every client while the fixture remains alive, even on the first
    // failure, so parent disappearance cannot obscure the original failure.
    let outcomes = clients
        .into_iter()
        .map(|client| client.join().unwrap())
        .collect::<Vec<_>>();
    for result in outcomes {
        assert!(result.is_ok(), "{}", result.unwrap_err());
    }
}
