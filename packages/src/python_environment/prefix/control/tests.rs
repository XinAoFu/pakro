use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

#[test]
fn r05_application_readonly_open_creates_no_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("missing");
    let opened = EnvironmentStore::open_readonly(
        &root,
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .unwrap();
    assert!(opened.is_none());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn r05_application_missing_catalog_lock_is_never_recreated() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |_| Ok(()))
        .unwrap();
    let lock = fixture.store.root_path().join(".allocation.lock");
    fs::remove_file(&lock).unwrap();
    assert!(
        fixture
            .store
            .with_control(ControlLimits::default(), |_| Ok(()))
            .is_err()
    );
    assert!(!lock.exists());
}

#[test]
fn r05_application_catalog_scope_starts_after_the_object_phase() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let independent = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(fixture.store.root_path().join(".allocation.lock"))?;
            let probe = Flock::lock(independent, FlockArg::LockExclusiveNonblock).map_err(
                |(_, error)| eyre::eyre!("catalog was locked before object phase: {error}"),
            )?;
            drop(probe);
            control.read(ControlNamespace::Catalog, "missing.json")?;
            let independent = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(fixture.store.root_path().join(".allocation.lock"))?;
            assert!(Flock::lock(independent, FlockArg::LockExclusiveNonblock).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn r05_application_object_admission_is_rejected_after_catalog_read() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let identity = prefix.generation_identity().unwrap();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.read(ControlNamespace::Catalog, "missing.json")?;
            assert!(
                control
                    .try_generation_lease(&identity, lease::GenerationLeaseMode::Live)
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn r05_application_writer_keeps_the_original_deadline() {
    let fixture = Fixture::new();
    let root = fixture.store.root_path();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let store = EnvironmentStore::open_writer(
        root,
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
        deadline,
    )
    .unwrap();
    std::thread::sleep(
        deadline.saturating_duration_since(std::time::Instant::now())
            + std::time::Duration::from_millis(10),
    );
    assert!(store.allocate().is_err());
    assert!(
        store
            .with_control(ControlLimits::default(), |_| Ok(()))
            .is_err()
    );
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("g-")
    }));
}

#[test]
fn r05_existing_allocation_does_not_get_a_recreated_object_lock() {
    let fixture = Fixture::new();
    let name = "g-00000000000000000000000000000001";
    fs::create_dir(fixture.store.root_path().join(name)).unwrap();
    let lock = fixture
        .store
        .root_path()
        .join(".leases/build")
        .join(format!("{name}.lock"));
    assert!(!lock.exists());
    assert!(fixture.store.allocate_named(name).is_err());
    assert!(
        !lock.exists(),
        "an existing allocation must not acquire replacement protection"
    );
}

#[test]
fn r05_payload_write_stops_at_the_original_deadline() {
    let fixture = Fixture::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let store = EnvironmentStore::open_writer(
        fixture.store.root_path(),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
        deadline,
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    let path = prefix.final_path().join("bounded");
    let mut writer = prefix
        .create_file_exclusive(Path::new("bounded"), 0o600)
        .unwrap();
    std::thread::sleep(
        deadline.saturating_duration_since(std::time::Instant::now())
            + std::time::Duration::from_millis(10),
    );
    assert!(writer.write_all(b"must not be written").is_err());
    drop(writer);
    assert_eq!(fs::read(path).unwrap(), b"");
}

fn replace_build_lock(fixture: &Fixture, allocation: &str) -> PathBuf {
    let path = fixture
        .store
        .root_path()
        .join(".leases/build")
        .join(format!("{allocation}.lock"));
    let saved = fixture
        ._root
        .path()
        .join(format!("{allocation}.original-lock"));
    fs::rename(&path, &saved).unwrap();
    fs::write(&path, b"replacement-lock").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn r05_replaced_build_lock_rejects_prefix_creation_and_preserves_discarded_content() {
    let fixture = Fixture::new();
    let mut prefix = fixture.store.allocate().unwrap();
    let generation = prefix.final_path().to_path_buf();
    let mut file = prefix
        .create_file_exclusive(Path::new("keep"), 0o600)
        .unwrap();
    file.write_all(b"owned-content").unwrap();
    file.finish().unwrap();
    let lock = replace_build_lock(&fixture, prefix.allocation_id());
    assert!(
        prefix
            .create_file_exclusive(Path::new("new"), 0o600)
            .is_err()
    );
    assert!(!generation.join("new").exists());
    assert!(prefix.discard().is_err());
    assert_eq!(fs::read(generation.join("keep")).unwrap(), b"owned-content");
    drop(prefix);
    assert_eq!(fs::read(generation.join("keep")).unwrap(), b"owned-content");
    assert_eq!(fs::read(lock).unwrap(), b"replacement-lock");
}

#[test]
fn r05_payload_writer_rechecks_the_build_lock_on_each_write() {
    let fixture = Fixture::new();
    let mut prefix = fixture.store.allocate().unwrap();
    let allocation = prefix.allocation_id().to_owned();
    let path = prefix.final_path().join("keep");
    let mut file = prefix
        .create_file_exclusive(Path::new("keep"), 0o600)
        .unwrap();
    file.write_all(b"before").unwrap();
    replace_build_lock(&fixture, &allocation);
    assert!(file.write_all(b"unauthorized-after").is_err());
    assert!(file.finish().is_err());
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert!(prefix.discard().is_err());
    assert_eq!(fs::read(&path).unwrap(), b"before");
}

#[test]
fn r05_discard_rechecks_build_lock_after_capture_checkpoint() {
    let fixture = Fixture::new();
    let mut prefix = fixture.store.allocate().unwrap();
    let generation = prefix.final_path().to_path_buf();
    let allocation = prefix.allocation_id().to_owned();
    let mut file = prefix
        .create_file_exclusive(Path::new("keep"), 0o600)
        .unwrap();
    file.write_all(b"owned-content").unwrap();
    file.finish().unwrap();
    assert!(
        prefix
            .discard_with_capture_hook(|| {
                replace_build_lock(&fixture, &allocation);
                Ok(())
            })
            .is_err()
    );
    assert_eq!(fs::read(generation.join("keep")).unwrap(), b"owned-content");
}

#[test]
fn r05_allocator_rechecks_build_lock_after_waiting_for_catalog() {
    let fixture = Fixture::new();
    let name = "g-00000000000000000000000000000002";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let worker_store = EnvironmentStore::open_writer(
        fixture.store.root_path(),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
        deadline,
    )
    .unwrap();
    let catalog = fixture
        .store
        .kernel
        .catalog_lock()
        .acquire(LeaseMode::Exclusive, deadline)
        .unwrap();
    let worker = std::thread::spawn(move || worker_store.allocate_named(name).is_err());
    let lock_path = fixture
        .store
        .root_path()
        .join(".leases/build")
        .join(format!("{name}.lock"));
    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut locked = false;
    while std::time::Instant::now() < ready_deadline {
        if let Ok(probe) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
        {
            match Flock::lock(probe, FlockArg::LockExclusiveNonblock) {
                Ok(probe) => drop(probe),
                Err((_, error)) if error == nix::errno::Errno::EWOULDBLOCK => {
                    locked = true;
                    break;
                }
                Err((_, error)) => panic!("unexpected build lock probe failure: {error}"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if locked {
        replace_build_lock(&fixture, name);
    }
    drop(catalog);
    let rejected = worker.join().unwrap();
    assert!(locked, "worker did not reach the actual held object lock");
    assert!(
        rejected,
        "allocation returned a prefix under a replaced lock inode"
    );
    assert!(!fixture.store.root_path().join(name).exists());
}

#[test]
fn store_identity_reports_the_anchored_store_inode() {
    let fixture = Fixture::new();
    let metadata = fs::metadata(fixture.store.root_path()).unwrap();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            assert_eq!(control.store_identity()?, (metadata.dev(), metadata.ino()));
            Ok(())
        })
        .unwrap();
}

#[test]
fn namespace_listing_returns_sorted_existing_names_without_reading_payloads() {
    let fixture = Fixture::new();
    fixture.seed(
        "z.json",
        b"opaque invalid JSON is not interpreted by listing",
    );
    fixture.seed("a.json", b"also opaque");
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            assert_eq!(
                control.list_names(ControlNamespace::Catalog, 2, 12)?,
                ["a.json", "z.json"]
            );
            assert!(
                control
                    .list_names(ControlNamespace::Journals, 1, 1)?
                    .is_empty()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn listing_charges_entry_and_name_budgets_before_collecting() {
    let fixture = Fixture::new();
    fixture.seed("a.json", b"a");
    fixture.seed("b.json", b"b");
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            assert!(
                control
                    .list_names(ControlNamespace::Catalog, 1, 100)
                    .is_err()
            );
            assert!(
                control
                    .list_names(ControlNamespace::Catalog, 2, 11)
                    .is_err()
            );
            assert!(
                control
                    .list_names(ControlNamespace::Catalog, 0, 100)
                    .is_err()
            );
            assert!(control.list_names(ControlNamespace::Catalog, 2, 0).is_err());
            assert_eq!(
                control.list_names(ControlNamespace::Catalog, 2, 12)?,
                ["a.json", "b.json"]
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn listing_rejects_unsafe_leaf_types_links_and_permissions() {
    for variant in [
        "symlink",
        "fifo",
        "directory",
        "hardlink",
        "public",
        "privileged",
    ] {
        let fixture = Fixture::new();
        let path = fixture.seed("entry.json", b"private bytes");
        match variant {
            "symlink" => {
                fs::remove_file(&path).unwrap();
                symlink("missing", &path).unwrap();
            }
            "fifo" => {
                fs::remove_file(&path).unwrap();
                nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap();
            }
            "directory" => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            "hardlink" => {
                fs::hard_link(&path, fixture.store.root_path().join("external-alias")).unwrap()
            }
            "public" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            _ => fs::set_permissions(&path, fs::Permissions::from_mode(0o4600)).unwrap(),
        }
        let started = std::time::Instant::now();
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                assert!(
                    control
                        .list_names(ControlNamespace::Catalog, 10, 1000)
                        .is_err(),
                    "accepted {variant}"
                );
                Ok(())
            })
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}

#[test]
fn incomplete_temporary_and_invalid_names_are_retained_as_errors() {
    for name in [".tmp-incomplete", "bad name", "中文.json"] {
        let fixture = Fixture::new();
        fixture.seed_namespace();
        let path = fixture.path("catalog", name);
        fs::write(&path, b"recovery evidence").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                assert!(
                    control
                        .list_names(ControlNamespace::Catalog, 10, 1000)
                        .is_err(),
                    "silently ignored {name}"
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"recovery evidence");
    }
}

#[test]
fn listing_checks_the_current_namespace_directory_identity() {
    let fixture = Fixture::new();
    fixture.seed("entry.json", b"evidence");
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let path = fixture.path("catalog", "");
            fs::rename(&path, fixture.path("saved", "")).unwrap();
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                control
                    .list_names(ControlNamespace::Catalog, 10, 1000)
                    .is_err()
            );
            Ok(())
        })
        .unwrap_err(); // The enclosing session also detects this replacement.
    assert!(error.to_string().contains("identity changed"), "{error:#}");
}

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("environments");
        let store = EnvironmentStore::open(
            &path,
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        Self { _root: root, store }
    }
    fn path(&self, namespace: &str, name: &str) -> PathBuf {
        self.store
            .root_path()
            .join(".control")
            .join(namespace)
            .join(name)
    }
    fn seed_namespace(&self) {
        for namespace in ["", "catalog", "journals", "seals"] {
            let path = self.store.root_path().join(".control").join(namespace);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    fn seed(&self, name: &str, bytes: &[u8]) -> PathBuf {
        self.seed_namespace();
        let path = self.path("catalog", name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}

#[test]
fn control_namespaces_are_real_owned_private_directories() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |_| Ok(()))
        .unwrap();
    for namespace in ["", "catalog", "journals", "seals"] {
        let metadata = fs::symlink_metadata(fixture.path(namespace, "")).unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
    }
}

#[test]
fn control_metadata_phase_holds_the_same_actual_allocation_flock() {
    let fixture = Fixture::new();
    drop(fixture.store.allocate().unwrap());
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            control.read(ControlNamespace::Catalog, "missing.json")?;
            let independent = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(fixture.store.root_path().join(".allocation.lock"))?;
            assert!(
                Flock::lock(independent, FlockArg::LockExclusiveNonblock).is_err(),
                "control session did not acquire the real shared writer lock"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn missing_control_metadata_is_none_without_a_dummy_file() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            assert!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
    assert!(!fixture.path("catalog", "state.json").exists());
}

#[test]
fn initial_atomic_replace_has_exact_bytes_hash_mode_and_a_private_snapshot() {
    use sha2::{Digest as _, Sha256};
    let fixture = Fixture::new();
    let bytes = b"{\"secret-test-content\":42}\n";
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let written = control.replace(ControlNamespace::Catalog, "state.json", None, bytes)?;
            assert_eq!(written.bytes(), bytes);
            assert_eq!(
                written.sha256().as_slice(),
                Sha256::digest(bytes).as_slice()
            );
            assert!(!format!("{written:?}").contains("secret-test-content"));
            assert_eq!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .unwrap()
                    .bytes(),
                bytes
            );
            Ok(())
        })
        .unwrap();
    let path = fixture.path("catalog", "state.json");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let metadata = fs::symlink_metadata(path).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    let names: Vec<_> = fs::read_dir(fixture.path("catalog", ""))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
}

#[test]
fn invalid_names_and_zero_budgets_fail_before_metadata_creation() {
    let fixture = Fixture::new();
    for limits in [
        ControlLimits {
            max_file_bytes: 0,
            ..Default::default()
        },
        ControlLimits {
            max_name_bytes: 0,
            ..Default::default()
        },
    ] {
        let mut called = false;
        assert!(
            fixture
                .store
                .with_control(limits, |_| {
                    called = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!called);
    }
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            for name in [
                "",
                ".",
                "..",
                "../escape",
                "/escape",
                "中文",
                "bad\0name",
                ".tmp-forged",
            ] {
                assert!(
                    control.read(ControlNamespace::Catalog, name).is_err(),
                    "{name:?}"
                );
                assert!(
                    control
                        .replace(ControlNamespace::Catalog, name, None, b"bad")
                        .is_err()
                );
            }
            Ok(())
        })
        .unwrap();
    assert!(!fixture.store.root_path().join("escape").exists());
}

#[test]
fn control_directory_aliases_are_not_followed() {
    let fixture = Fixture::new();
    let outside = fixture._root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"outside").unwrap();
    fs::rename(
        fixture.store.root_path().join(".control"),
        fixture._root.path().join("held-control"),
    )
    .unwrap();
    symlink(&outside, fixture.store.root_path().join(".control")).unwrap();
    assert!(
        fixture
            .store
            .with_control(ControlLimits::default(), |_| Ok(()))
            .is_err()
    );
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside");
    assert!(!outside.join("catalog").exists());
}

#[test]
fn reads_reject_symlink_fifo_hardlink_and_unsafe_mode_without_blocking() {
    for kind in ["symlink", "fifo", "hardlink", "mode"] {
        let fixture = Fixture::new();
        fixture.seed_namespace();
        let outside = fixture._root.path().join("outside");
        fs::write(&outside, b"do-not-read-or-write").unwrap();
        let path = fixture.path("catalog", "state.json");
        match kind {
            "symlink" => symlink(&outside, &path).unwrap(),
            "fifo" => nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap(),
            "hardlink" => fs::hard_link(&outside, &path).unwrap(),
            _ => {
                fs::write(&path, b"unsafe").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
            }
        }
        assert!(
            fixture
                .store
                .with_control(ControlLimits::default(), |control| control
                    .read(ControlNamespace::Catalog, "state.json"))
                .is_err(),
            "{kind}"
        );
        assert_eq!(fs::read(outside).unwrap(), b"do-not-read-or-write");
    }
}

#[test]
fn bounded_reads_accept_exact_bytes_and_reject_an_extra_byte() {
    let fixture = Fixture::new();
    let path = fixture.seed("state.json", b"1234");
    let limits = ControlLimits {
        max_file_bytes: 4,
        ..Default::default()
    };
    fixture
        .store
        .with_control(limits, |control| {
            assert_eq!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .unwrap()
                    .bytes(),
                b"1234"
            );
            Ok(())
        })
        .unwrap();
    fs::write(path, b"12345").unwrap();
    assert!(
        fixture
            .store
            .with_control(limits, |control| control
                .read(ControlNamespace::Catalog, "state.json"))
            .is_err()
    );
}

#[test]
fn compare_and_swap_preserves_the_last_writer_and_missing_expectation_is_exclusive() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let first = control.replace(ControlNamespace::Catalog, "state.json", None, b"first")?;
            assert!(
                control
                    .replace(ControlNamespace::Catalog, "state.json", None, b"wrong")
                    .is_err()
            );
            let second = control.replace(
                ControlNamespace::Catalog,
                "state.json",
                Some(&first),
                b"second",
            )?;
            assert!(
                control
                    .replace(
                        ControlNamespace::Catalog,
                        "state.json",
                        Some(&first),
                        b"stale"
                    )
                    .is_err()
            );
            assert_eq!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .unwrap()
                    .bytes(),
                second.bytes()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn snapshots_cannot_be_reused_for_another_store_namespace_or_name() {
    let first = Fixture::new();
    let second = Fixture::new();
    let snapshot = first
        .store
        .with_control(ControlLimits::default(), |control| {
            control.replace(ControlNamespace::Catalog, "state.json", None, b"first")
        })
        .unwrap();
    first
        .store
        .with_control(ControlLimits::default(), |control| {
            assert!(
                control
                    .replace(
                        ControlNamespace::Journals,
                        "state.json",
                        Some(&snapshot),
                        b"bad"
                    )
                    .is_err()
            );
            assert!(
                control
                    .replace(
                        ControlNamespace::Catalog,
                        "other.json",
                        Some(&snapshot),
                        b"bad"
                    )
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    second
        .store
        .with_control(ControlLimits::default(), |control| {
            assert!(
                control
                    .replace(
                        ControlNamespace::Catalog,
                        "state.json",
                        Some(&snapshot),
                        b"bad"
                    )
                    .is_err()
            );
            assert!(control.remove(&snapshot).is_err());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fs::read(first.path("catalog", "state.json")).unwrap(),
        b"first"
    );
    assert!(!second.path("catalog", "state.json").exists());
}

#[test]
fn removal_requires_the_current_snapshot_and_is_durable_metadata_only() {
    let fixture = Fixture::new();
    fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let old = control.replace(ControlNamespace::Catalog, "state.json", None, b"first")?;
            let current = control.replace(
                ControlNamespace::Catalog,
                "state.json",
                Some(&old),
                b"second",
            )?;
            assert!(control.remove(&old).is_err());
            assert_eq!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .unwrap()
                    .bytes(),
                b"second"
            );
            control.remove(&current)?;
            assert!(
                control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .is_none()
            );
            assert!(control.remove(&current).is_err());
            Ok(())
        })
        .unwrap();
    assert!(fixture.store.root_path().is_dir());
}

#[test]
fn changed_content_or_replaced_inode_cannot_reuse_a_snapshot() {
    for replacement in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.seed("state.json", b"first");
        let snapshot = fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                Ok(control
                    .read(ControlNamespace::Catalog, "state.json")?
                    .unwrap())
            })
            .unwrap();
        if replacement {
            fs::remove_file(&path).unwrap();
        }
        fs::write(&path, b"third-party").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fixture
            .store
            .with_control(ControlLimits::default(), |control| {
                assert!(
                    control
                        .replace(
                            ControlNamespace::Catalog,
                            "state.json",
                            Some(&snapshot),
                            b"bad"
                        )
                        .is_err()
                );
                assert!(control.remove(&snapshot).is_err());
                Ok(())
            })
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), b"third-party");
    }
}

#[test]
fn output_and_name_limits_reject_before_replacing_existing_metadata() {
    let fixture = Fixture::new();
    let path = fixture.seed("state.json", b"old");
    let limits = ControlLimits {
        max_file_bytes: 3,
        max_name_bytes: 10,
    };
    fixture
        .store
        .with_control(limits, |control| {
            let old = control
                .read(ControlNamespace::Catalog, "state.json")?
                .unwrap();
            assert!(
                control
                    .replace(ControlNamespace::Catalog, "state.json", Some(&old), b"four")
                    .is_err()
            );
            assert!(
                control
                    .read(ControlNamespace::Catalog, "longer-name.json")
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"old");
}

#[test]
fn per_read_budgets_can_narrow_but_never_expand_the_control_session_limit() {
    let fixture = Fixture::new();
    let path = fixture.seed("state.json", b"1234");
    let limits = ControlLimits {
        max_file_bytes: 4,
        ..Default::default()
    };
    fixture.store.with_control(limits, |control| {
        let narrow_rejected = control.read_bounded(ControlNamespace::Catalog, "state.json", 3).is_err();
        let zero_rejected = control.read_bounded(ControlNamespace::Catalog, "state.json", 0).is_err();
        assert_eq!(control.read_bounded(ControlNamespace::Catalog, "state.json", 4)?.unwrap().bytes(), b"1234");
        fs::write(&path, b"12345")?;
        let wider_rejected = control.read_bounded(ControlNamespace::Catalog, "state.json", 8).is_err();
        assert!(narrow_rejected && zero_rejected && wider_rejected,
            "read budgets were not enforced: narrow={narrow_rejected} zero={zero_rejected} wider={wider_rejected}");
        Ok(())
    }).unwrap();
    assert_eq!(fs::read(path).unwrap(), b"12345");
}
