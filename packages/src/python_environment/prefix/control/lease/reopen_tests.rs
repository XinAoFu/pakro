use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    store: EnvironmentStore,
    facts: GenerationIdentityFacts,
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
        let prefix = store.allocate().unwrap();
        let identity = prefix.generation_identity().unwrap();
        let facts = identity.facts().unwrap();
        drop(identity);
        // Temporarily remove the directory entry the old unpublished owner
        // knows. Its real Drop retains that unknown state and releases every
        // original descriptor and the kernel build EX, without deleting g.
        let generation = store.root_path().join(&facts.allocation);
        let saved = root.path().join("saved-generation");
        fs::rename(&generation, &saved).unwrap();
        drop(prefix);
        fs::rename(&saved, &generation).unwrap();
        Self { root, store, facts }
    }

    fn pin(&self, facts: &GenerationIdentityFacts) -> Result<GenerationIdentity> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.pin_generation_identity(facts)
            })
    }

    fn cleanup(&self, facts: &GenerationIdentityFacts) -> Result<Option<CleanupLease>> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.try_cleanup_lease(facts)
            })
    }

    fn generation(&self) -> PathBuf {
        self.store.root_path().join(&self.facts.allocation)
    }

    fn lock(&self) -> PathBuf {
        self.store
            .root_path()
            .join(".leases/build")
            .join(format!("{}.lock", self.facts.allocation))
    }

    fn acquire(
        &self,
        identity: &GenerationIdentity,
        mode: GenerationLeaseMode,
    ) -> Option<GenerationLease> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.try_generation_lease(identity, mode)
            })
            .unwrap()
    }
}

#[test]
fn reopened_identity_keeps_actual_inodes_after_all_old_generation_pins_drop() {
    let Fixture { root, store, facts } = Fixture::new();
    let path = store.root_path().to_owned();
    drop(store);
    let reopened = EnvironmentStore::open(
        &path,
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let identity = reopened
        .with_control(ControlLimits::default(), |control| {
            control.pin_generation_identity(&facts)
        })
        .expect("existing generation must reopen from persisted facts");
    assert_eq!(identity.facts().unwrap(), facts);
    let live = reopened
        .with_control(ControlLimits::default(), |control| {
            control.try_generation_lease(&identity, GenerationLeaseMode::Live)
        })
        .unwrap()
        .expect("released old build EX must not survive reopening");
    live.check_binding().unwrap();
    assert!(root.path().exists());
}

#[test]
fn reopening_identity_does_not_bypass_the_actual_held_build_lock() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let facts = prefix.generation_identity().unwrap().facts().unwrap();
    let identity = fixture
        .pin(&facts)
        .expect("pinning existing files does not acquire a lease");
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_none()
    );
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Collect)
            .is_none()
    );
    assert!(fixture.cleanup(&facts).unwrap().is_none());
}

#[test]
fn cleanup_reopens_existing_lock_after_the_generation_directory_is_gone() {
    let fixture = Fixture::new();
    fs::remove_dir(fixture.generation()).unwrap();
    let mut facts = fixture.facts.clone();
    facts.generation = [u64::MAX, u64::MAX];
    let cleanup = fixture
        .cleanup(&facts)
        .unwrap()
        .expect("missing g must not prevent cleanup EX");
    assert_eq!(cleanup.facts(), &facts);
    cleanup.check_binding().unwrap();
    assert!(fixture.cleanup(&facts).unwrap().is_none());
    drop(cleanup);
    assert!(fixture.cleanup(&facts).unwrap().is_some());
    assert!(
        fixture.lock().is_file(),
        "releasing cleanup must not unlink the permanent lock"
    );
    assert!(
        !fixture.generation().exists(),
        "cleanup lease must not recreate g"
    );
}

#[test]
fn cleanup_waits_for_actual_live_release_after_generation_capture() {
    let fixture = Fixture::new();
    let identity = fixture.pin(&fixture.facts).unwrap();
    let live = fixture
        .acquire(&identity, GenerationLeaseMode::Live)
        .unwrap();
    let quarantine = fixture.store.root_path().join("q-captured");
    fs::rename(fixture.generation(), &quarantine).unwrap();
    let start = Instant::now();
    assert!(fixture.cleanup(&fixture.facts).unwrap().is_none());
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(live);
    let cleanup = fixture.cleanup(&fixture.facts).unwrap().unwrap();
    cleanup.check_binding().unwrap();
    assert!(
        quarantine.is_dir(),
        "lease must not itself remove the captured root"
    );
}

#[test]
fn cleanup_excludes_a_live_lease_for_the_same_existing_generation() {
    let fixture = Fixture::new();
    let identity = fixture.pin(&fixture.facts).unwrap();
    let cleanup = fixture.cleanup(&fixture.facts).unwrap().unwrap();
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_none()
    );
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Collect)
            .is_none()
    );
    drop(cleanup);
    assert!(
        fixture
            .acquire(&identity, GenerationLeaseMode::Live)
            .is_some()
    );
}

#[test]
fn reopening_refuses_every_changed_persisted_identity_pair() {
    let fixture = Fixture::new();
    for field in 0..5 {
        for coordinate in 0..2 {
            let mut facts = fixture.facts.clone();
            let pair = match field {
                0 => &mut facts.store,
                1 => &mut facts.generation,
                2 => &mut facts.leases,
                3 => &mut facts.build,
                4 => &mut facts.lock,
                _ => unreachable!(),
            };
            pair[coordinate] ^= 1;
            assert!(
                fixture.pin(&facts).is_err(),
                "accepted changed identity pair {field}"
            );
            if field != 1 {
                assert!(
                    fixture.cleanup(&facts).is_err(),
                    "cleanup accepted changed identity pair {field}"
                );
            }
        }
    }
}

#[test]
fn reopening_rejects_unsafe_allocation_identifiers_before_opening_entries() {
    let fixture = Fixture::new();
    for allocation in [
        "".to_owned(),
        "g-0".to_owned(),
        format!("g-{}", "A".repeat(32)),
        format!("g-{}", "0".repeat(31)),
        format!("g-{}", "0".repeat(33)),
        "../environments".to_owned(),
        "/tmp/escape".to_owned(),
        format!("g-{}\0", "0".repeat(32)),
    ] {
        let mut facts = fixture.facts.clone();
        facts.allocation = allocation;
        assert!(fixture.pin(&facts).is_err());
        assert!(fixture.cleanup(&facts).is_err());
    }
    assert_eq!(
        fs::read_dir(fixture.store.root_path().join(".leases/build"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn reopening_missing_entries_never_creates_a_directory_or_lock() {
    for component in ["generation", ".leases", ".leases/build", "lock"] {
        let fixture = Fixture::new();
        let path = match component {
            "generation" => fixture.generation(),
            "lock" => fixture.lock(),
            _ => fixture.store.root_path().join(component),
        };
        let saved = fixture.root.path().join("saved-entry");
        fs::rename(&path, &saved).unwrap();
        assert!(
            fixture.pin(&fixture.facts).is_err(),
            "accepted missing {component}"
        );
        if component != "generation" {
            assert!(
                fixture.cleanup(&fixture.facts).is_err(),
                "cleanup accepted missing {component}"
            );
        }
        assert!(!path.exists(), "recreated missing {component}");
    }
}

#[test]
fn reopening_refuses_replacement_inodes_even_when_the_entries_are_safe() {
    for component in ["generation", ".leases", ".leases/build", "lock"] {
        let fixture = Fixture::new();
        let path = match component {
            "generation" => fixture.generation(),
            "lock" => fixture.lock(),
            _ => fixture.store.root_path().join(component),
        };
        let saved = fixture.root.path().join("saved-entry");
        fs::rename(&path, &saved).unwrap();
        if component == "lock" {
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(
            fixture.pin(&fixture.facts).is_err(),
            "accepted replacement {component}"
        );
        if component != "generation" {
            assert!(
                fixture.cleanup(&fixture.facts).is_err(),
                "cleanup accepted replacement {component}"
            );
        }
    }
}

#[test]
fn reopening_refuses_a_generation_from_another_store() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    assert!(other.pin(&fixture.facts).is_err());
    assert!(other.cleanup(&fixture.facts).is_err());
}

#[test]
fn reopening_refuses_unsafe_lock_entries_without_blocking() {
    for variant in [
        "symlink",
        "fifo",
        "directory",
        "hardlink",
        "public",
        "privileged",
    ] {
        let fixture = Fixture::new();
        let path = fixture.lock();
        match variant {
            "symlink" => {
                let saved = path.with_extension("saved");
                fs::rename(&path, &saved).unwrap();
                symlink(saved, &path).unwrap();
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
        assert!(fixture.pin(&fixture.facts).is_err(), "accepted {variant}");
        assert!(
            fixture.cleanup(&fixture.facts).is_err(),
            "cleanup accepted {variant}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn reopening_refuses_unsafe_generation_and_lease_directories() {
    for component in ["generation", ".leases", ".leases/build"] {
        for variant in ["symlink", "public", "privileged"] {
            let fixture = Fixture::new();
            let path = if component == "generation" {
                fixture.generation()
            } else {
                fixture.store.root_path().join(component)
            };
            match variant {
                "symlink" => {
                    let saved = fixture.root.path().join("saved-entry");
                    fs::rename(&path, &saved).unwrap();
                    symlink(saved, &path).unwrap();
                }
                "public" => fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap(),
                "privileged" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o4700)).unwrap()
                }
                _ => unreachable!(),
            }
            assert!(
                fixture.pin(&fixture.facts).is_err(),
                "accepted {component} {variant}"
            );
            if component != "generation" {
                assert!(
                    fixture.cleanup(&fixture.facts).is_err(),
                    "cleanup accepted {component} {variant}"
                );
            }
        }
    }
}

#[test]
fn held_cleanup_detects_replaced_store_and_lease_entries() {
    for component in ["store", ".leases", ".leases/build", "lock"] {
        let fixture = Fixture::new();
        let cleanup = fixture.cleanup(&fixture.facts).unwrap().unwrap();
        cleanup.check_binding().unwrap();
        let path = match component {
            "store" => fixture.store.root_path().to_owned(),
            "lock" => fixture.lock(),
            _ => fixture.store.root_path().join(component),
        };
        let saved = fixture.root.path().join("saved-entry");
        fs::rename(&path, &saved).unwrap();
        if component == "lock" {
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(
            cleanup.check_binding().is_err(),
            "missed replaced {component}"
        );
        fs::remove_file(&path)
            .or_else(|_| fs::remove_dir(&path))
            .unwrap();
        fs::rename(&saved, &path).unwrap();
        cleanup.check_binding().unwrap();
    }
}
