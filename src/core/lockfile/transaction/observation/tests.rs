use super::*;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::sync::mpsc;
use std::time::Duration;

const ORIGINAL: &[u8] = b"[project]\nname='demo'\nversion='1.0'\n";
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn open(path: &Path) -> ReadOnlyFileObservation {
    ReadOnlyFileObservation::open(path, Default::default(), deadline()).unwrap()
}
fn write(path: &Path) {
    fs::write(path, ORIGINAL).unwrap();
}
fn stamp(path: &Path) -> (u64, u64, u32, u64, i64, i64, i64, i64) {
    let meta = fs::symlink_metadata(path).unwrap();
    (
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.nlink(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

#[test]
fn readonly_and_hardlinked_config_creates_no_guard_or_other_entries() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("readonly");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("pyproject.toml");
    write(&path);
    fs::hard_link(&path, root.path().join("other-link")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let before_file = stamp(&path);
    let before_parent = stamp(&parent);
    let observation = open(&path);
    assert_eq!(observation.bytes(), ORIGINAL);
    let digest: [u8; 32] = Sha256::digest(ORIGINAL).into();
    assert_eq!(observation.sha256(), &digest);
    observation.recheck().unwrap();
    assert_eq!(stamp(&path), before_file);
    assert_eq!(stamp(&parent), before_parent);
    let entries = fs::read_dir(&parent)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!parent.join(".pakro.lock.guard").exists());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn in_place_edits_invalidate_the_observation_but_preserve_its_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    write(&path);
    let observation = open(&path);
    let old_inode = fs::metadata(&path).unwrap().ino();
    fs::write(&path, b"[project]\nname='changed'\nversion='2.0'\n").unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), old_inode);
    assert_eq!(observation.recheck(), Err(ReadOnlyFileError::Changed));
    assert_eq!(observation.bytes(), ORIGINAL);
}

#[test]
fn same_bytes_under_a_replacement_inode_are_not_the_original_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    write(&path);
    let observation = open(&path);
    let old_inode = fs::metadata(&path).unwrap().ino();
    let replacement = root.path().join("replacement");
    write(&replacement);
    assert_ne!(fs::metadata(&replacement).unwrap().ino(), old_inode);
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(observation.recheck(), Err(ReadOnlyFileError::Changed));
}

#[test]
fn leaf_and_intermediate_aliases_are_each_pinned_even_for_the_same_target() {
    for change_intermediate in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("actual.toml");
        write(&target);
        let intermediate = root.path().join("middle");
        symlink("actual.toml", &intermediate).unwrap();
        let alias = root.path().join("pyproject.toml");
        symlink("middle", &alias).unwrap();
        let observation = open(&alias);
        observation.recheck().unwrap();
        let changed = if change_intermediate {
            &intermediate
        } else {
            &alias
        };
        let old = fs::symlink_metadata(changed).unwrap().ino();
        let next = root.path().join("replacement-link");
        symlink(
            if change_intermediate {
                "actual.toml"
            } else {
                "middle"
            },
            &next,
        )
        .unwrap();
        assert_ne!(fs::symlink_metadata(&next).unwrap().ino(), old);
        fs::rename(&next, changed).unwrap();
        assert_eq!(
            fs::canonicalize(&alias).unwrap(),
            fs::canonicalize(&target).unwrap()
        );
        assert_eq!(observation.recheck(), Err(ReadOnlyFileError::Changed));
    }
}

#[test]
fn parent_alias_and_an_ordinary_ancestor_cannot_be_substituted() {
    for substitute_ordinary_ancestor in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        write(&actual.join("pyproject.toml"));
        let named = root.path().join("named");
        let path = if substitute_ordinary_ancestor {
            actual.join("pyproject.toml")
        } else {
            symlink("actual", &named).unwrap();
            named.join("pyproject.toml")
        };
        let observation = open(&path);
        observation.recheck().unwrap();
        let original_file = fs::metadata(&path).unwrap().ino();
        if substitute_ordinary_ancestor {
            let moved = root.path().join("retained");
            fs::rename(&actual, &moved).unwrap();
            symlink("retained", &actual).unwrap();
        } else {
            let replacement = root.path().join("new-parent-link");
            symlink("actual", &replacement).unwrap();
            assert_ne!(
                fs::symlink_metadata(&replacement).unwrap().ino(),
                fs::symlink_metadata(&named).unwrap().ino()
            );
            fs::rename(replacement, &named).unwrap();
        }
        assert_eq!(fs::metadata(&path).unwrap().ino(), original_file);
        assert_eq!(observation.recheck(), Err(ReadOnlyFileError::Changed));
    }
}

#[test]
fn missing_directory_and_fifo_inputs_fail_without_waiting_for_a_writer() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        ReadOnlyFileObservation::open(&root.path().join("missing"), Default::default(), deadline())
            .is_err()
    );
    assert!(ReadOnlyFileObservation::open(root.path(), Default::default(), deadline()).is_err());
    let fifo = root.path().join("pipe");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let worker_path = fifo.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = ReadOnlyFileObservation::open(&worker_path, Default::default(), deadline());
        tx.send(result.is_err()).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_millis(500));
    if result.is_err() {
        // Release a broken blocking FIFO open so a failure cannot hang the suite.
        let rescue = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        drop(rescue);
        let _ = rx.recv_timeout(Duration::from_secs(2));
        panic!("readonly FIFO open did not complete without a writer");
    }
    assert!(result.unwrap());
    worker.join().unwrap();
}

#[test]
fn byte_path_component_symlink_and_deadline_limits_are_real_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    write(&path);
    open(&path).recheck().unwrap();
    let mut limits = ReadOnlyFileLimits::default();
    limits.max_bytes = ORIGINAL.len();
    assert_eq!(
        ReadOnlyFileObservation::open(&path, limits, deadline())
            .unwrap()
            .bytes(),
        ORIGINAL
    );
    limits.max_bytes -= 1;
    assert!(matches!(
        ReadOnlyFileObservation::open(&path, limits, deadline()),
        Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Bytes))
    ));
    limits = Default::default();
    limits.max_path_bytes = 1;
    assert!(matches!(
        ReadOnlyFileObservation::open(&path, limits, deadline()),
        Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::PathBytes))
    ));
    limits = Default::default();
    limits.max_components = 1;
    assert!(matches!(
        ReadOnlyFileObservation::open(&path, limits, deadline()),
        Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Components))
    ));
    let first = root.path().join("first");
    let second = root.path().join("second");
    symlink("second", &first).unwrap();
    symlink("pyproject.toml", &second).unwrap();
    limits = Default::default();
    limits.max_symlink_hops = 1;
    assert!(matches!(
        ReadOnlyFileObservation::open(&first, limits, deadline()),
        Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Symlinks))
    ));
    assert!(matches!(
        ReadOnlyFileObservation::open(
            &path,
            Default::default(),
            Instant::now() - Duration::from_secs(1)
        ),
        Err(ReadOnlyFileError::Expired)
    ));
    for zero in 0..4 {
        limits = Default::default();
        match zero {
            0 => limits.max_bytes = 0,
            1 => limits.max_path_bytes = 0,
            2 => limits.max_components = 0,
            _ => limits.max_symlink_hops = 0,
        }
        assert!(matches!(
            ReadOnlyFileObservation::open(&path, limits, deadline()),
            Err(ReadOnlyFileError::InvalidLimits)
        ));
    }
}

#[test]
fn a_real_capture_mutation_is_rejected_and_diagnostics_do_not_echo_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("secret-user-token.toml");
    write(&path);
    let observation = open(&path);
    let formatted = format!("{observation:?}");
    assert!(!formatted.contains("secret-user-token"));
    assert!(!formatted.contains("demo"));
    let mut reached = 0;
    let result =
        ReadOnlyFileObservation::open_with_hook(&path, Default::default(), deadline(), || {
            reached += 1;
            fs::write(
                &path,
                b"[project]\nname='secret-replacement'\nversion='2'\n",
            )
            .unwrap();
        });
    assert_eq!(reached, 1);
    assert!(matches!(result, Err(ReadOnlyFileError::Changed)));
    let error = result.unwrap_err();
    assert!(!format!("{error:?} {error}").contains("secret"));
}
