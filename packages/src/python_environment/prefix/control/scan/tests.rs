use super::*;
use std::fs;
use std::os::unix::fs::symlink;

struct Fixture {
    _root: tempfile::TempDir,
    store: EnvironmentStore,
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
        Self { _root: root, store }
    }
    fn scan(&self, limits: NamespaceScanLimits) -> Result<EnvironmentNamespace> {
        self.store
            .with_control(ControlLimits::default(), |control| {
                control.scan_environment_namespace(limits)
            })
    }
    fn directory(&self, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.store.root_path().join(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}

#[test]
fn an_empty_store_has_no_generations_and_scanning_does_not_create_quarantine() {
    let fixture = Fixture::new();
    let result = fixture.scan(NamespaceScanLimits::default()).unwrap();
    assert!(result.generations.is_empty() && result.quarantined.is_empty());
    assert!(!fixture.store.root_path().join(".quarantine").exists());
}

#[test]
fn actual_generation_and_quarantine_roots_are_listed_with_exact_inodes() {
    let fixture = Fixture::new();
    let first = fixture.store.allocate().unwrap();
    let second = fixture.store.allocate().unwrap();
    let quarantine = fixture.directory(".quarantine");
    let qname = "q-00000000000000000000000000000001";
    let captured = quarantine.join(qname);
    fs::rename(first.final_path(), &captured).unwrap();
    let observed = fixture.scan(NamespaceScanLimits::default()).unwrap();
    let metadata = fs::metadata(second.final_path()).unwrap();
    assert_eq!(
        observed.generations,
        [NamespaceDirectory {
            name: second.allocation_id().into(),
            identity: [metadata.dev(), metadata.ino()]
        }]
    );
    let metadata = fs::metadata(&captured).unwrap();
    assert_eq!(
        observed.quarantined,
        [NamespaceDirectory {
            name: qname.into(),
            identity: [metadata.dev(), metadata.ino()]
        }]
    );
    assert!(captured.is_dir());
}

#[test]
fn a_generation_without_catalog_or_journal_is_reported_instead_of_hidden() {
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let observed = fixture.scan(NamespaceScanLimits::default()).unwrap();
    assert_eq!(
        observed.generations.len(),
        1,
        "allocation/journal crash-gap root was hidden"
    );
    assert_eq!(observed.generations[0].name, prefix.allocation_id());
}

#[test]
fn unknown_root_or_quarantine_entries_fail_closed_and_are_retained() {
    for relative in [
        "unexpected",
        ".tmp-orphan",
        "g-not-an-id",
        ".quarantine/unknown",
    ] {
        let fixture = Fixture::new();
        if relative.starts_with(".quarantine/") {
            fixture.directory(".quarantine");
        }
        let path = fixture.store.root_path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"retained evidence").unwrap();
        assert!(
            fixture.scan(NamespaceScanLimits::default()).is_err(),
            "ignored {relative}"
        );
        assert_eq!(fs::read(path).unwrap(), b"retained evidence");
    }
}

#[test]
fn generation_symlinks_files_and_unsafe_modes_are_not_directory_facts() {
    use std::os::unix::fs::PermissionsExt;
    for variant in [
        "symlink",
        "file",
        "mode",
        "privileged",
        "quarantine-symlink",
    ] {
        let fixture = Fixture::new();
        let name = "g-00000000000000000000000000000001";
        match variant {
            "symlink" => symlink(
                fixture.store.root_path(),
                fixture.store.root_path().join(name),
            )
            .unwrap(),
            "file" => fs::write(fixture.store.root_path().join(name), b"not a directory").unwrap(),
            "mode" => {
                let path = fixture.directory(name);
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "privileged" => {
                let path = fixture.directory(name);
                fs::set_permissions(path, fs::Permissions::from_mode(0o2700)).unwrap();
            }
            _ => symlink(
                fixture.store.root_path(),
                fixture.store.root_path().join(".quarantine"),
            )
            .unwrap(),
        }
        assert!(
            fixture.scan(NamespaceScanLimits::default()).is_err(),
            "accepted {variant}"
        );
    }
}

#[test]
fn namespace_entry_and_name_byte_budgets_include_metadata_and_quarantine() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let prefix = fixture.store.allocate().unwrap();
    let quarantine = fixture.directory(".quarantine");
    let captured = quarantine.join("q-00000000000000000000000000000001");
    fs::create_dir(&captured).unwrap();
    fs::set_permissions(&captured, fs::Permissions::from_mode(0o700)).unwrap();
    let _ = fixture.scan(NamespaceScanLimits::default()).unwrap();
    let mut entries = 0;
    let mut bytes = 0;
    for entry in fs::read_dir(fixture.store.root_path()).unwrap() {
        let entry = entry.unwrap();
        entries += 1;
        bytes += entry.file_name().as_encoded_bytes().len();
    }
    entries += 1;
    bytes += captured.file_name().unwrap().as_encoded_bytes().len();
    assert!(
        fixture
            .scan(NamespaceScanLimits {
                max_entries: entries,
                max_total_name_bytes: bytes
            })
            .is_ok()
    );
    for limits in [
        NamespaceScanLimits {
            max_entries: entries - 1,
            max_total_name_bytes: bytes,
        },
        NamespaceScanLimits {
            max_entries: entries,
            max_total_name_bytes: bytes - 1,
        },
        NamespaceScanLimits {
            max_entries: 0,
            max_total_name_bytes: bytes,
        },
        NamespaceScanLimits {
            max_entries: entries,
            max_total_name_bytes: 0,
        },
    ] {
        assert!(
            fixture.scan(limits).is_err(),
            "namespace budget was ignored"
        );
    }
    drop(prefix);
}

#[test]
fn a_replaced_control_namespace_cannot_be_used_to_scan_generations() {
    let fixture = Fixture::new();
    let error = fixture
        .store
        .with_control(ControlLimits::default(), |control| {
            let path = fixture.store.root_path().join(".control/ownership");
            fs::rename(&path, path.with_extension("saved")).unwrap();
            fs::create_dir(&path).unwrap();
            assert!(
                control
                    .scan_environment_namespace(NamespaceScanLimits::default())
                    .is_err()
            );
            Err::<(), _>(eyre::eyre!("injected replacement retained"))
        })
        .unwrap_err();
    assert!(error.to_string().contains("injected replacement retained"));
}
