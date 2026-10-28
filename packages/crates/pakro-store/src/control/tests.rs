use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{fs, time::Duration};

fn uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("store");
    (temp, path)
}
fn writer(path: &Path) -> StoreWriter {
    StoreWriter::open_writer(path, uid(), RootLimits::default(), deadline()).unwrap()
}
fn readonly(path: &Path) -> Result<Option<ReadStore>> {
    ReadStore::open_readonly(path, uid(), RootLimits::default(), deadline())
}
fn inventory(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        if path.is_dir() {
            out.push((path.to_owned(), Vec::new()));
            for entry in fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), out)
            }
        } else {
            out.push((path.to_owned(), fs::read(path).unwrap()))
        }
    }
    let mut out = Vec::new();
    visit(path, &mut out);
    out.sort();
    out
}

#[test]
fn r05_read_empty_root_creates_nothing() {
    let (temp, path) = fixture();
    let absent = path.join("nested/store");
    assert!(readonly(&absent).unwrap().is_none());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(readonly(&path).unwrap().is_none());
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}

#[test]
fn r05_initialized_read_keeps_every_entry_unchanged() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    store
        .control(ControlLimits::default())
        .unwrap()
        .replace(ControlNamespace::Catalog, "state.json", None, b"catalog")
        .unwrap();
    drop(store);
    let before = inventory(&path);
    let read = readonly(&path).unwrap().unwrap();
    let control = read.control(ControlLimits::default()).unwrap();
    assert_eq!(
        control
            .read(ControlNamespace::Catalog, "state.json")
            .unwrap()
            .unwrap()
            .bytes(),
        b"catalog"
    );
    assert!(
        control
            .read(ControlNamespace::Journals, "absent.json")
            .unwrap()
            .is_none()
    );
    assert_eq!(inventory(&path), before);
}

#[test]
fn r05_existing_state_without_lock_is_corruption_without_repair() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    fs::remove_file(path.join(".allocation.lock")).unwrap();
    assert!(readonly(&path).is_err());
    assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
    assert!(!path.join(".allocation.lock").exists());
}

#[test]
fn r05_existing_state_without_namespace_is_corruption_without_repair() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    fs::remove_dir(path.join(".control/catalog")).unwrap();
    assert!(readonly(&path).is_err());
    assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
    assert!(!path.join(".control/catalog").exists());
}

#[test]
fn r05_catalog_lock_replacement_is_rejected_after_reopen() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    let lock = path.join(".allocation.lock");
    fs::rename(&lock, path.join("saved.lock")).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(readonly(&path).is_err());
    assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
}

#[test]
fn r05_bounded_read_rejects_extra_bytes() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    store
        .control(ControlLimits::default())
        .unwrap()
        .replace(ControlNamespace::Catalog, "state", None, b"12345")
        .unwrap();
    let read = readonly(&path).unwrap().unwrap();
    let control = read.control(ControlLimits::default()).unwrap();
    assert!(
        control
            .read_bounded(ControlNamespace::Catalog, "state", 4)
            .is_err()
    );
    assert_eq!(
        control
            .read_bounded(ControlNamespace::Catalog, "state", 5)
            .unwrap()
            .unwrap()
            .bytes(),
        b"12345"
    );
}

#[test]
fn r05_compare_swap_rejects_stale_snapshot() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let mut control = store.control(ControlLimits::default()).unwrap();
    let first = control
        .replace(ControlNamespace::Catalog, "state", None, b"first")
        .unwrap();
    let second = control
        .replace(ControlNamespace::Catalog, "state", Some(&first), b"second")
        .unwrap();
    assert!(
        control
            .replace(ControlNamespace::Catalog, "state", Some(&first), b"third")
            .is_err()
    );
    assert!(control.remove(&first).is_err());
    assert_eq!(
        control
            .read(ControlNamespace::Catalog, "state")
            .unwrap()
            .unwrap()
            .bytes(),
        b"second"
    );
    control.remove(&second).unwrap();
    assert!(
        control
            .read(ControlNamespace::Catalog, "state")
            .unwrap()
            .is_none()
    );
}

#[test]
fn p6_replace_rechecks_changed_bytes_and_inodes_inside_original_scope() {
    use std::os::unix::fs::MetadataExt;
    for change in ["bytes", "inode", "namespace"] {
        let (_temp, path) = fixture();
        let store = writer(&path);
        let mut control = store.control(ControlLimits::default()).unwrap();
        let original = control.replace(ControlNamespace::Catalog, "state", None, b"first").unwrap();
        let state = path.join(".control/catalog/state");
        let original_inode = fs::metadata(&state).unwrap().ino();
        let changed_path = path.clone();
        super::write::BEFORE_REPLACE_SECOND_CAS.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let state = changed_path.join(".control/catalog/state");
                if change == "bytes" {
                    fs::write(&state, b"other")?;
                    assert_eq!(fs::metadata(&state)?.ino(), original_inode);
                } else if change == "inode" {
                    fs::rename(&state, changed_path.join(".control/catalog/original-state"))?;
                    fs::write(&state, b"first")?;
                    fs::set_permissions(&state, fs::Permissions::from_mode(0o600))?;
                    assert_ne!(fs::metadata(&state)?.ino(), original_inode);
                } else {
                    fs::rename(changed_path.join(".control/catalog"), changed_path.join(".control/original-catalog"))?;
                    fs::create_dir(changed_path.join(".control/catalog"))?;
                    fs::set_permissions(changed_path.join(".control/catalog"), fs::Permissions::from_mode(0o700))?;
                }
                Ok(())
            }));
        });
        let error = control.replace(ControlNamespace::Catalog, "state", Some(&original), b"second").unwrap_err();
        if change == "namespace" {
            assert!(format!("{error:#}").contains("environment entry identity changed"),
                "change={change}: {error:#}");
            fs::remove_dir(path.join(".control/catalog")).unwrap();
            fs::rename(path.join(".control/original-catalog"), path.join(".control/catalog")).unwrap();
        } else {
            assert!(format!("{error:#}").contains("environment control metadata changed since its snapshot"),
                "change={change}: {error:#}");
        }
        assert_eq!(fs::read(&state).unwrap(), if change == "bytes" { b"other" } else { b"first" });
        assert!(fs::read_dir(path.join(".control/catalog")).unwrap().all(|entry|
            !entry.unwrap().file_name().as_encoded_bytes().starts_with(b".tmp-")),
            "change={change}: failed replacement retained its owned temporary file");
        store.check_binding().unwrap();
    }
}

#[test]
fn f1_recovery_prepared_cas_publishes_the_predeclared_inode() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let mut control = store.control(ControlLimits::default()).unwrap();
    let old = control
        .replace(ControlNamespace::Catalog, "ready", None, b"old")
        .unwrap();
    let staged = control
        .replace(ControlNamespace::Journals, "prepared", None, b"restored")
        .unwrap();
    let published = control
        .replace_from_journal("ready", &old, &staged)
        .unwrap();
    assert_eq!(published.file_identity(), staged.file_identity());
    assert_eq!(published.bytes(), b"restored");
    assert!(
        control
            .read(ControlNamespace::Journals, "prepared")
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .replace_from_journal("ready", &old, &staged)
            .is_err()
    );
    assert_eq!(
        control
            .read(ControlNamespace::Catalog, "ready")
            .unwrap()
            .unwrap()
            .bytes(),
        b"restored"
    );
}

#[test]
fn f1_recovery_prepared_cas_rejects_same_bytes_target_replacement() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let mut control = store.control(ControlLimits::default()).unwrap();
    let old = control
        .replace(ControlNamespace::Catalog, "ready", None, b"old")
        .unwrap();
    let staged = control
        .replace(ControlNamespace::Journals, "prepared", None, b"restored")
        .unwrap();
    let foreign = path.join(".control/catalog/foreign");
    fs::write(&foreign, b"old").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&foreign, path.join(".control/catalog/ready")).unwrap();
    assert!(
        control
            .replace_from_journal("ready", &old, &staged)
            .is_err()
    );
    assert_eq!(
        control
            .read(ControlNamespace::Journals, "prepared")
            .unwrap()
            .unwrap()
            .file_identity(),
        staged.file_identity()
    );
    assert_eq!(
        fs::read(path.join(".control/catalog/ready")).unwrap(),
        b"old"
    );
}

#[test]
fn f1_recovery_prepared_cas_rejects_same_bytes_source_replacement() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let mut control = store.control(ControlLimits::default()).unwrap();
    let old = control
        .replace(ControlNamespace::Catalog, "ready", None, b"old")
        .unwrap();
    let staged = control
        .replace(ControlNamespace::Journals, "prepared", None, b"restored")
        .unwrap();
    let foreign = path.join(".control/journals/foreign");
    fs::write(&foreign, b"restored").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&foreign, path.join(".control/journals/prepared")).unwrap();
    assert!(
        control
            .replace_from_journal("ready", &old, &staged)
            .is_err()
    );
    assert_eq!(
        control
            .read(ControlNamespace::Catalog, "ready")
            .unwrap()
            .unwrap()
            .file_identity(),
        old.file_identity()
    );
    assert_eq!(
        fs::read(path.join(".control/journals/prepared")).unwrap(),
        b"restored"
    );
}

#[test]
fn f1_recovery_prepared_cas_rejects_foreign_namespace_and_store() {
    let (_first, first) = fixture();
    let (_second, second) = fixture();
    let a = writer(&first);
    let b = writer(&second);
    let mut control = a.control(ControlLimits::default()).unwrap();
    let old = control
        .replace(ControlNamespace::Catalog, "ready", None, b"old")
        .unwrap();
    let wrong_namespace = control
        .replace(ControlNamespace::Ownership, "prepared", None, b"new")
        .unwrap();
    assert!(
        control
            .replace_from_journal("ready", &old, &wrong_namespace)
            .is_err()
    );
    let foreign = b
        .control(ControlLimits::default())
        .unwrap()
        .replace(ControlNamespace::Journals, "prepared", None, b"new")
        .unwrap();
    assert!(
        control
            .replace_from_journal("ready", &old, &foreign)
            .is_err()
    );
    assert_eq!(
        control
            .read(ControlNamespace::Catalog, "ready")
            .unwrap()
            .unwrap()
            .file_identity(),
        old.file_identity()
    );
}

#[test]
fn r05_metadata_rejects_symlink_hardlink_fifo_and_public_mode() {
    for kind in ["symlink", "hardlink", "fifo", "mode"] {
        let (_temp, path) = fixture();
        let store = writer(&path);
        store
            .control(ControlLimits::default())
            .unwrap()
            .replace(ControlNamespace::Catalog, "state", None, b"one")
            .unwrap();
        let state = path.join(".control/catalog/state");
        match kind {
            "symlink" => {
                fs::rename(&state, path.join("original")).unwrap();
                symlink(path.join("original"), &state).unwrap();
            }
            "hardlink" => fs::hard_link(&state, path.join("alias")).unwrap(),
            "fifo" => {
                fs::remove_file(&state).unwrap();
                nix::unistd::mkfifo(&state, nix::sys::stat::Mode::from_bits_truncate(0o600))
                    .unwrap();
            }
            _ => fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap(),
        }
        let read = readonly(&path).unwrap().unwrap();
        assert!(
            read.control(ControlLimits::default())
                .unwrap()
                .read(ControlNamespace::Catalog, "state")
                .is_err(),
            "accepted {kind}"
        );
    }
}

#[test]
fn r05_authorized_root_rejects_symlink_and_parent_paths() {
    let (temp, path) = fixture();
    drop(writer(&path));
    let alias = temp.path().join("alias");
    symlink(&path, &alias).unwrap();
    assert!(readonly(&alias).is_err());
    assert!(readonly(&path.join("../store")).is_err());
    assert!(readonly(Path::new("relative")).is_err());
}

#[test]
fn r05_common_root_supports_unicode_components() {
    let (temp, _) = fixture();
    let path = temp.path().canonicalize().unwrap().join("语言库");
    drop(writer(&path));
    assert!(readonly(&path).unwrap().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn r05_common_root_supports_non_utf8_components() {
    use std::os::unix::ffi::OsStrExt;
    let (temp, _) = fixture();
    let path = temp
        .path()
        .canonicalize()
        .unwrap()
        .join(std::ffi::OsStr::from_bytes(b"store-\xff"));
    drop(writer(&path));
    assert!(readonly(&path).unwrap().is_some());
}

#[test]
fn r05_nonempty_uninitialized_root_is_not_adopted() {
    let (_temp, path) = fixture();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(path.join("unowned"), b"preserve").unwrap();
    assert!(readonly(&path).is_err());
    assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
    assert!(!path.join(".allocation.lock").exists());
}

#[test]
fn r05_expired_deadline_never_initializes_root() {
    let (_temp, path) = fixture();
    let expired = Instant::now() - Duration::from_millis(1);
    assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), expired).is_err());
    assert!(!path.exists());
}

#[test]
fn r05_shared_reader_blocks_collector() {
    super::lease::tests::r05_shared_reader_blocks_collector();
}

#[test]
fn r05_lock_inode_replacement_is_rejected() {
    super::lease::tests::r05_lock_inode_replacement_is_rejected();
}

#[test]
fn r05_reader_waits_for_real_concurrent_initializer() {
    use std::io::{Read, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    const CHILD_ROOT: &str = "__PAKRO_R05_INIT_ROOT";
    const CHILD_READY: &str = "__PAKRO_R05_INIT_READY";
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        let path = PathBuf::from(path);
        let root = open_trusted_store(&path, uid(), true).unwrap();
        let ready = std::env::var_os(CHILD_READY).unwrap();
        let child_deadline = Instant::now() + Duration::from_secs(10);
        let anchor = initialize_anchor_with_hook(path, root, uid(), child_deadline, || {
            fs::write(&ready, b"created stable lock").unwrap();
            let mut release = [0];
            std::io::stdin().read_exact(&mut release)?;
            Ok(())
        })
        .unwrap();
        anchor.check_binding(child_deadline).unwrap();
        return;
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (temp, path) = fixture();
    let ready = temp.path().join("initializer.ready");
    let name = std::thread::current().name().unwrap().to_owned();
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD_ROOT, &path)
            .env(CHILD_READY, &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let limit = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "initializer exited before handshake"
        );
        assert!(Instant::now() < limit, "initializer handshake timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    let (send, receive) = mpsc::channel();
    let query_path = path.clone();
    let reader = std::thread::spawn(move || {
        send.send(readonly(&query_path)).unwrap();
    });
    assert!(
        matches!(
            receive.recv_timeout(Duration::from_millis(80)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "reader returned before the initializer published its identity marker"
    );
    child.0.stdin.as_mut().unwrap().write_all(b"go").unwrap();
    let store = receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .expect("initialized store");
    reader.join().unwrap();
    assert!(child.0.wait().unwrap().success());
    store.check_binding().unwrap();
    assert!(
        store
            .control(ControlLimits::default())
            .unwrap()
            .read(ControlNamespace::Catalog, "missing")
            .unwrap()
            .is_none()
    );
}

#[test]
fn r05_original_deadline_also_bounds_binding_checks() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    let original = Instant::now() + Duration::from_millis(200);
    let writable = StoreWriter::open_writer(&path, uid(), RootLimits::default(), original).unwrap();
    let readable = ReadStore::open_readonly(&path, uid(), RootLimits::default(), original)
        .unwrap()
        .unwrap();
    let read_scope = readable.control(ControlLimits::default()).unwrap();
    let write_scope = writable.control(ControlLimits::default()).unwrap();
    std::thread::sleep(
        original.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
    );
    assert!(
        writable.check_binding().is_err(),
        "writer refreshed the caller's deadline"
    );
    assert!(
        readable.check_binding().is_err(),
        "reader refreshed the caller's deadline"
    );
    assert!(
        read_scope.check_binding().is_err(),
        "read scope refreshed the caller's deadline"
    );
    assert!(
        write_scope.check_binding().is_err(),
        "write scope refreshed the caller's deadline"
    );
}

#[test]
fn r05_initializer_creates_stable_object_lock_directories() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    for directory in [".leases", ".leases/build"] {
        let metadata = fs::metadata(path.join(directory))
            .expect("initializer must create stable object lock directories");
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
    }
}

#[test]
fn r05_missing_object_lock_directories_are_never_recreated() {
    let (_temp, path) = fixture();
    drop(writer(&path));
    assert!(
        path.join(".leases/build").is_dir(),
        "initializer omitted object lock directories"
    );
    for directory in [".leases/build", ".leases"] {
        fs::remove_dir(path.join(directory)).unwrap();
        assert!(readonly(&path).is_err());
        assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
        assert!(!path.join(directory).exists());
    }
}

#[test]
fn r05_replaced_object_lock_directories_are_rejected() {
    for directory in [".leases/build", ".leases"] {
        let (_temp, path) = fixture();
        drop(writer(&path));
        let target = path.join(directory);
        let saved = target.with_extension("saved");
        fs::rename(&target, &saved)
            .expect("initializer must create stable object lock directories");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let before = inventory(&path);
        assert!(readonly(&path).is_err());
        assert!(StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline()).is_err());
        assert_eq!(
            inventory(&path),
            before,
            "corrupt store was implicitly repaired"
        );
    }
}

#[test]
fn r05_control_rejects_same_named_object_lock_from_another_store() {
    let (_a, a_path) = fixture();
    let (_b, b_path) = fixture();
    let a = writer(&a_path);
    let b = writer(&b_path);
    let foreign = StableLock::create_new(b.build_handle(), "object.lock", uid()).unwrap();
    let scope = a.control(ControlLimits::default()).unwrap();
    assert!(
        scope
            .try_object("object", &foreign, LeaseMode::Shared)
            .is_err(),
        "foreign lock was accepted as protection for this store"
    );
    let read = readonly(&a_path).unwrap().unwrap();
    let read_scope = read.control(ControlLimits::default()).unwrap();
    assert!(
        read_scope
            .try_object("object", &foreign, LeaseMode::Shared)
            .is_err(),
        "readonly scope accepted another store's lock"
    );
}

#[test]
fn r05_control_object_key_must_match_lock_leaf() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let wrong = StableLock::create_new(store.build_handle(), "different.lock", uid()).unwrap();
    let scope = store.control(ControlLimits::default()).unwrap();
    assert!(
        scope
            .try_object("object", &wrong, LeaseMode::Shared)
            .is_err(),
        "one object's key used a different object's lock"
    );
}

#[test]
fn r05_control_correct_shared_object_blocks_collection() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let lock = StableLock::create_new(store.build_handle(), "object.lock", uid()).unwrap();
    let read = readonly(&path).unwrap().unwrap();
    let scope = read.control(ControlLimits::default()).unwrap();
    let held = scope
        .try_object("object", &lock, LeaseMode::Shared)
        .unwrap()
        .unwrap();
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_none()
    );
    scope.read(ControlNamespace::Catalog, "missing").unwrap();
    drop(scope);
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_none()
    );
    drop(held);
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, deadline())
            .unwrap()
            .is_some()
    );
}

#[test]
fn r05_control_project_protection_belongs_to_same_store() {
    let (_a, a_path) = fixture();
    let (_b, b_path) = fixture();
    let a = writer(&a_path);
    let b = writer(&b_path);
    let foreign = StableLock::create_new(b.build_handle(), "project.lock", uid()).unwrap();
    let local = StableLock::create_new(a.build_handle(), "project.lock", uid()).unwrap();
    let scope = a.control(ControlLimits::default()).unwrap();
    assert!(
        scope.acquire_project(&foreign).is_err(),
        "project protection came from another store"
    );
    let held = scope.acquire_project(&local).unwrap();
    held.check_binding().unwrap();
    scope.read(ControlNamespace::Catalog, "missing").unwrap();
}

#[test]
fn r05_absent_read_still_rechecks_namespace_binding() {
    let (_temp, path) = fixture();
    let store = writer(&path);
    let read = readonly(&path).unwrap().unwrap();
    let scope = read.control(ControlLimits::default()).unwrap();
    let result = scope.read_with_limit_and_hook(ControlNamespace::Catalog, "missing", 1024, || {
        fs::rename(
            path.join(".control/catalog"),
            path.join(".control/original-catalog"),
        )?;
        fs::create_dir(path.join(".control/catalog"))?;
        fs::set_permissions(
            path.join(".control/catalog"),
            fs::Permissions::from_mode(0o700),
        )?;
        Ok(())
    });
    assert!(result.is_err(), "missing entry hid a replaced namespace");
    drop(store);
}

#[test]
fn r05_root_walk_stops_before_creation_when_original_deadline_expires() {
    let (_temp, path) = fixture();
    let target = path.join("deadline-boundary/descendant");
    let original = Instant::now() + Duration::from_millis(200);
    let mut reached = false;
    let result = super::unix::open_trusted_store_with_hook(
        &target,
        uid(),
        true,
        Some(original),
        |component| {
            if component == std::ffi::OsStr::new("deadline-boundary") {
                reached = true;
                std::thread::sleep(
                    original.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
                );
            }
            Ok(())
        },
    );
    assert!(reached, "deadline hook was not reached");
    assert!(result.is_err());
    assert!(
        !path.join("deadline-boundary").exists(),
        "root walker kept creating directories after the original deadline"
    );
}

#[test]
fn r05_readonly_regular_open_has_no_write_access() {
    use nix::fcntl::{FcntlArg, fcntl};
    let (_temp, path) = fixture();
    let store = writer(&path);
    let file = open_existing_readonly_regular(store.root_handle(), LOCK_NAME, uid()).unwrap();
    let flags = fcntl(&file, FcntlArg::F_GETFL).unwrap();
    assert_eq!(
        flags & nix::libc::O_ACCMODE,
        nix::libc::O_RDONLY,
        "readonly opening requested write access"
    );
}

#[test]
fn f4_store_initialization_qualification_is_not_reissued() {
    use std::sync::{Arc, Barrier};
    let (_temp, path) = fixture();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let start = Arc::new(Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let start = start.clone();
            workers.push(scope.spawn(move || {
                start.wait();
                StoreWriter::open_writer(&path, uid(), RootLimits::default(), deadline())
                    .map(|store| store.initialized_here())
            }));
        }
        start.wait();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(true)))
            .count(),
        1,
        "exactly the stable-inode initializer gets the creation fact: {results:?}"
    );
    // A racing opener may conservatively fail before the identity marker is
    // complete. Neither that failure nor a successful reopen grants creation.
    assert!(results.iter().any(|result| !matches!(result, Ok(true))));
    assert!(!writer(&path).initialized_here());
    let before = inventory(&path);
    // Deterministically exercise the EEXIST fallback as well as the real
    // racing opens above: the losing initializer must not run the callback
    // from which StoreWriter derives its creation qualification.
    let called = std::cell::Cell::new(false);
    let root = open_trusted_store(&path, uid(), false).unwrap();
    let anchor = initialize_anchor_with_hook(path.clone(), root, uid(), deadline(), || {
        called.set(true);
        Ok(())
    })
    .unwrap();
    anchor.check_binding(deadline()).unwrap();
    assert!(
        !called.get(),
        "an initialization loser received creation qualification"
    );
    assert!(!writer(&path).initialized_here());
    assert_eq!(
        inventory(&path),
        before,
        "reopening changed initialized store contents"
    );
}
