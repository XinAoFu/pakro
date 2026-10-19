use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::time::Duration;

const RAW_ONE: &str = "[packages.\"python@3.13\"]\ndemo='1.0'\n";
const RAW_TWO: &str = "[packages.\"python@3.13\"]\ndemo='2.0'\n";
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn load(path: &Path) -> NativePythonConfigFile {
    NativePythonConfigFile::open(path, Default::default(), deadline()).unwrap()
}
fn version(owner: &NativePythonConfigFile) -> String {
    let data = owner.checked_package_requests().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].runtime_name, "python");
    assert_eq!(data[0].runtime_version, "3.13");
    assert_eq!(data[0].name, "demo");
    data[0].version.clone()
}

#[test]
fn actual_file_uses_the_existing_parser_and_checked_requests_without_writing_a_guard() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    PakroToml::from_str(RAW_ONE, &path)
        .expect("existing parser accepts the real fixture; this is only a data prelude");
    let before = fs::symlink_metadata(&path).unwrap();
    let owner = load(&path);
    assert_eq!(version(&owner), "1.0");
    owner.for_python().unwrap().recheck_for(&owner).unwrap();
    let after = fs::symlink_metadata(&path).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.mtime(),
            before.ctime()
        ),
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.mtime(),
            after.ctime()
        )
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert!(!directory.path().join(".pakro.lock.guard").exists());
}

#[test]
fn a_new_file_receipt_cannot_label_old_cached_or_proposed_parsed_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    let old_data = PakroToml::from_str(RAW_ONE, &path).unwrap();
    let cloned_data = old_data.clone();
    let proposed_data = PakroToml::for_history_preflight(RAW_ONE, &path).unwrap();
    let old_requests = |data: &PakroToml| {
        crate::packages::requests_from_config(&data.packages.lock().unwrap()).unwrap()
    };
    assert_eq!(old_requests(&cloned_data)[0].version, "1.0");
    assert_eq!(old_requests(&proposed_data)[0].version, "1.0");
    fs::write(&path, RAW_TWO).unwrap();
    let observed = load(&path);
    assert_eq!(
        version(&observed),
        "2.0",
        "the proof must parse its own held bytes, never adopt an old Arc or proposed data"
    );
    observed
        .for_python()
        .unwrap()
        .recheck_for(&observed)
        .unwrap();
}

#[test]
fn replacement_inode_and_same_target_alias_changes_revoke_the_original_source() {
    for alias in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual.toml");
        fs::write(&actual, RAW_ONE).unwrap();
        let path = if alias {
            let path = directory.path().join("pakro.toml");
            symlink("actual.toml", &path).unwrap();
            path
        } else {
            actual.clone()
        };
        let owner = load(&path);
        let receipt = owner.for_python().unwrap();
        if alias {
            let next = directory.path().join("next-link");
            symlink("actual.toml", &next).unwrap();
            fs::rename(next, &path).unwrap();
            assert_eq!(
                fs::canonicalize(&path).unwrap(),
                fs::canonicalize(&actual).unwrap()
            );
        } else {
            let next = directory.path().join("next-file");
            fs::write(&next, RAW_ONE).unwrap();
            fs::rename(next, &path).unwrap();
        }
        assert!(matches!(
            receipt.recheck_for(&owner),
            Err(NativeConfigError::Source(ReadOnlyFileError::Changed))
        ));
    }
}

#[test]
fn two_actual_owners_with_identical_bytes_cannot_exchange_a_borrowed_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    let first = load(&path);
    let another = load(&path);
    let receipt = first.for_python().unwrap();
    receipt.recheck_for(&first).unwrap();
    assert_eq!(
        receipt.recheck_for(&another),
        Err(NativeConfigError::WrongOwner)
    );
}

#[test]
fn utf8_toml_path_byte_deadline_and_package_errors_fail_without_raw_source_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("PRIVATE_SENTINEL.toml");
    fs::write(&path, RAW_ONE).unwrap();
    assert_eq!(version(&load(&path)), "1.0");
    let mut limits = ReadOnlyFileLimits::default();
    limits.max_bytes = RAW_ONE.len();
    assert_eq!(
        version(&NativePythonConfigFile::open(&path, limits, deadline()).unwrap()),
        "1.0"
    );
    limits.max_bytes -= 1;
    assert!(NativePythonConfigFile::open(&path, limits, deadline()).is_err());
    limits = Default::default();
    limits.max_path_bytes = 1;
    assert!(NativePythonConfigFile::open(&path, limits, deadline()).is_err());
    assert!(
        NativePythonConfigFile::open(
            &path,
            Default::default(),
            Instant::now() - Duration::from_secs(1)
        )
        .is_err()
    );
    for bytes in [
        b"\xffPRIVATE_SENTINEL".as_slice(),
        b"[broken PRIVATE_SENTINEL".as_slice(),
    ] {
        fs::write(&path, bytes).unwrap();
        let error =
            NativePythonConfigFile::open(&path, Default::default(), deadline()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE_SENTINEL"));
    }
    fs::write(&path, "[packages.\"invalid-group\"]\ndemo='1'\n").unwrap();
    let invalid_packages = load(&path);
    assert!(
        matches!(
            invalid_packages.checked_package_requests(),
            Err(NativeConfigError::InvalidPackages)
        ),
        "invalid legacy package declarations cannot become empty roots"
    );
}

#[test]
fn mutation_after_the_actual_parser_rejects_the_receipt_and_preserves_external_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    PakroToml::from_str(RAW_ONE, &path).expect("existing parser fixture prelude only");
    let mut parsed = 0;
    let result =
        NativePythonConfigFile::open_with_finish(&path, Default::default(), deadline(), || {
            parsed += 1;
            fs::write(&path, RAW_TWO).unwrap();
        });
    assert_eq!(
        parsed, 1,
        "late target requires the actual existing parser to finish"
    );
    assert!(matches!(
        result,
        Err(NativeConfigError::Source(ReadOnlyFileError::Changed))
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), RAW_TWO);
}

#[test]
fn changed_source_rejects_new_requests_and_new_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    let owner = load(&path);
    fs::write(&path, RAW_TWO).unwrap();
    assert_eq!(
        owner.checked_package_requests(),
        Err(NativeConfigError::Source(ReadOnlyFileError::Changed))
    );
    assert!(matches!(
        owner.for_python(),
        Err(NativeConfigError::Source(ReadOnlyFileError::Changed))
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), RAW_TWO);
}

#[test]
fn requests_and_receipts_keep_the_original_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    let expires = Instant::now() + Duration::from_secs(2);
    let owner = NativePythonConfigFile::open(&path, Default::default(), expires).unwrap();
    let receipt = owner.for_python().unwrap();
    assert_eq!(version(&owner), "1.0");
    std::thread::sleep(expires.saturating_duration_since(Instant::now()));
    assert_eq!(
        owner.checked_package_requests(),
        Err(NativeConfigError::Source(ReadOnlyFileError::Expired))
    );
    assert!(matches!(
        owner.for_python(),
        Err(NativeConfigError::Source(ReadOnlyFileError::Expired))
    ));
    assert_eq!(
        receipt.recheck_for(&owner),
        Err(NativeConfigError::Source(ReadOnlyFileError::Expired))
    );
}

#[test]
fn parser_completion_after_deadline_cannot_create_an_owner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    fs::write(&path, RAW_ONE).unwrap();
    let expires = Instant::now() + Duration::from_secs(2);
    let mut parsed = false;
    let result =
        NativePythonConfigFile::open_with_finish(&path, Default::default(), expires, || {
            parsed = true;
            std::thread::sleep(expires.saturating_duration_since(Instant::now()));
        });
    assert!(parsed);
    assert!(matches!(
        result,
        Err(NativeConfigError::Source(ReadOnlyFileError::Expired))
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), RAW_ONE);
}

#[test]
fn checked_projection_preserves_package_errors_and_returns_independent_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pakro.toml");
    for raw in [
        "[packages.\"invalid-group\"]\ndemo='1'\n",
        "[packages.\"@3.13\"]\ndemo='1'\n",
        "[packages.\"python@\"]\ndemo='1'\n",
        "[packages.\"python@3.13\"]\n''='1'\n",
        "[packages.\"python@3.13\"]\ndemo=''\n",
    ] {
        fs::write(&path, raw).unwrap();
        assert_eq!(
            load(&path).checked_package_requests(),
            Err(NativeConfigError::InvalidPackages)
        );
    }
    for raw in [
        "packages=[]\n",
        "[packages]\n'python@3.13'='1'\n",
        "[packages.\"python@3.13\"]\ndemo=1\n",
    ] {
        fs::write(&path, raw).unwrap();
        assert!(matches!(
            NativePythonConfigFile::open(&path, Default::default(), deadline()),
            Err(NativeConfigError::InvalidDocument)
        ));
    }
    fs::write(&path, RAW_ONE).unwrap();
    let owner = load(&path);
    let mut requests = owner.checked_package_requests().unwrap();
    requests[0].version = "modified-copy".into();
    requests.clear();
    assert_eq!(version(&owner), "1.0");
    fs::write(&path, "[tools]\npython='3.13'\n").unwrap();
    assert!(load(&path).checked_package_requests().unwrap().is_empty());
}

#[test]
fn non_utf8_paths_are_rejected_before_the_legacy_parser() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().unwrap();
    let parent = directory
        .path()
        .join(OsString::from_vec(b"PRIVATE_\xff".to_vec()));
    let path = parent.join("pakro.toml");
    let mut parsed = false;
    let error =
        NativePythonConfigFile::open_with_finish(&path, Default::default(), deadline(), || {
            parsed = true
        })
        .unwrap_err();
    assert_eq!(error, NativeConfigError::InvalidDocument);
    assert!(!parsed);
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

// macOS rejects non-UTF-8 directory names at mkdir, before the owner can run.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_alias_targets_are_rejected_before_the_legacy_parser() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().unwrap();
    let parent = directory
        .path()
        .join(OsString::from_vec(b"PRIVATE_\xff".to_vec()));
    fs::create_dir(&parent).unwrap();
    let actual = parent.join("pakro.toml");
    fs::write(&actual, RAW_ONE).unwrap();
    let alias = directory.path().join("pakro.toml");
    symlink(&actual, &alias).unwrap();
    for path in [&actual, &alias] {
        let mut parsed = false;
        let error =
            NativePythonConfigFile::open_with_finish(path, Default::default(), deadline(), || {
                parsed = true
            })
            .unwrap_err();
        assert_eq!(error, NativeConfigError::InvalidDocument);
        assert!(!parsed);
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    assert_eq!(fs::read_to_string(&actual).unwrap(), RAW_ONE);
}
