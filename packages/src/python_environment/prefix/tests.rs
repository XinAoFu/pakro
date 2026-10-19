use super::*;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

const FIXED: &str = "g-0123456789ab7def8123456789abcdef";

struct Fixture {
    _owner: tempfile::TempDir,
    parent: PathBuf,
    store: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let parent = owner.path().canonicalize().unwrap();
        assert!(parent.as_os_str().as_encoded_bytes().is_ascii());
        Self {
            store: parent.join("environments"),
            parent,
            _owner: owner,
        }
    }

    fn open(&self) -> EnvironmentStore {
        EnvironmentStore::open(&self.store, uid(), PrefixLimits::default()).unwrap()
    }

    fn allocate(&self) -> UnpublishedPrefix {
        let prefix = self.open().allocate().unwrap();
        assert!(
            prefix.final_path().is_dir(),
            "allocator did not create its final directory"
        );
        prefix
    }
}

fn uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn write_file(prefix: &mut UnpublishedPrefix, relative: &str, bytes: &[u8], mode: u32) {
    let mut writer = prefix
        .create_file_exclusive(Path::new(relative), mode)
        .unwrap();
    writer.write_all(bytes).unwrap();
    let receipt = writer.finish().unwrap();
    assert_eq!(receipt.relative_path(), Path::new(relative));
    assert_eq!(receipt.size(), bytes.len() as u64);
    assert_eq!(receipt.mode(), mode);
}

#[test]
fn owned_ascii_store_creates_an_exclusive_final_directory_before_layout() {
    let fixture = Fixture::new();
    let store = fixture.open();
    assert_eq!(store.root_path(), fixture.store);
    assert!(fixture.store.is_dir());
    let prefix = store.allocate().unwrap();
    assert!(prefix.final_path().starts_with(&fixture.store));
    assert!(
        prefix
            .final_path()
            .as_os_str()
            .as_encoded_bytes()
            .is_ascii()
    );
    assert!(prefix.final_path().is_dir());
    assert_eq!(mode(prefix.final_path()), 0o700);
    prefix.check_binding().unwrap();
}

#[test]
fn allocations_are_unique_and_never_relocate_the_final_prefix() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let first = store.allocate().unwrap();
    let second = store.allocate().unwrap();
    assert_ne!(first.final_path(), second.final_path());
    assert_ne!(first.allocation_id(), second.allocation_id());
    assert!(first.final_path().is_dir() && second.final_path().is_dir());
}

#[test]
fn existing_directory_file_and_symlink_are_never_adopted_or_overwritten() {
    for kind in ["directory", "file", "symlink"] {
        let fixture = Fixture::new();
        let store = fixture.open();
        let target = fixture.store.join(FIXED);
        let external = fixture.parent.join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep"), b"external").unwrap();
        match kind {
            "directory" => {
                fs::create_dir(&target).unwrap();
                fs::write(target.join("keep"), b"old").unwrap();
            }
            "file" => fs::write(&target, b"old").unwrap(),
            _ => symlink(&external, &target).unwrap(),
        }
        assert!(store.allocate_named(FIXED).is_err(), "{kind}");
        assert_eq!(fs::read(external.join("keep")).unwrap(), b"external");
        match kind {
            "directory" => assert_eq!(fs::read(target.join("keep")).unwrap(), b"old"),
            "file" => assert_eq!(fs::read(target).unwrap(), b"old"),
            _ => assert_eq!(fs::read_link(target).unwrap(), external),
        }
    }
}

#[test]
fn non_ascii_environment_root_and_wrong_uid_fail_without_creating_a_store() {
    let fixture = Fixture::new();
    let unicode = fixture.parent.join("中文环境根");
    assert!(EnvironmentStore::open(&unicode, uid(), PrefixLimits::default()).is_err());
    assert!(!unicode.exists());
    assert!(
        EnvironmentStore::open(
            &fixture.store,
            uid().wrapping_add(1),
            PrefixLimits::default()
        )
        .is_err()
    );
    assert!(!fixture.store.exists());
}

#[test]
fn unicode_source_location_does_not_determine_the_ascii_generation_path() {
    let fixture = Fixture::new();
    let source = fixture.parent.join("中文 source project");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("app.py"), b"source").unwrap();
    let prefix = fixture.allocate();
    assert!(
        prefix
            .final_path()
            .as_os_str()
            .as_encoded_bytes()
            .is_ascii()
    );
    assert!(!prefix.final_path().starts_with(&source));
    assert_eq!(fs::read(source.join("app.py")).unwrap(), b"source");
}

#[test]
fn symlink_store_or_ancestor_is_rejected_without_touching_the_external_directory() {
    for direct in [false, true] {
        let fixture = Fixture::new();
        let external = fixture.parent.join("outside");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep"), b"outside").unwrap();
        let alias = fixture.parent.join("alias");
        symlink(&external, &alias).unwrap();
        let requested = if direct {
            alias
        } else {
            alias.join("environments")
        };
        assert!(EnvironmentStore::open(&requested, uid(), PrefixLimits::default()).is_err());
        assert_eq!(fs::read(external.join("keep")).unwrap(), b"outside");
        assert!(!external.join("environments").exists());
    }
}

#[test]
fn writable_nonsticky_parent_is_rejected_but_sticky_trusted_ancestor_is_supported() {
    let fixture = Fixture::new();
    let shared = fixture.parent.join("shared-parent");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    let root = shared.join("environments");
    assert!(EnvironmentStore::open(&root, uid(), PrefixLimits::default()).is_err());
    assert!(!root.exists());
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
    let store = EnvironmentStore::open(&root, uid(), PrefixLimits::default()).unwrap();
    let prefix = store.allocate().unwrap();
    assert!(prefix.final_path().is_dir());
}

#[test]
fn relative_escape_and_privileged_file_modes_are_rejected_before_writing() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    for path in [
        "../outside",
        "/outside",
        "nested/../../outside",
        "bad\0name",
    ] {
        assert!(prefix.create_dir_all(Path::new(path)).is_err(), "{path:?}");
        assert!(
            prefix
                .create_file_exclusive(Path::new(path), 0o644)
                .is_err(),
            "{path:?}"
        );
    }
    assert!(
        prefix
            .create_file_exclusive(Path::new("setuid"), 0o4755)
            .is_err()
    );
    assert!(!fixture.parent.join("outside").exists());
}

#[test]
fn directory_creation_is_owned_idempotent_and_mode_exact() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    for _ in 0..2 {
        prefix
            .create_dir_all(Path::new("lib/site/namespace"))
            .unwrap();
    }
    for relative in ["lib", "lib/site", "lib/site/namespace"] {
        let path = prefix.final_path().join(relative);
        assert!(path.is_dir(), "missing created directory: {relative}");
        assert_eq!(mode(&path), 0o755);
    }
    prefix.check_binding().unwrap();
}

#[test]
fn partial_directory_creation_exceeding_entry_quota_taints_the_allocation() {
    let fixture = Fixture::new();
    let store = EnvironmentStore::open(
        &fixture.store,
        uid(),
        PrefixLimits {
            max_entries: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    assert!(prefix.final_path().is_dir());
    assert!(prefix.create_dir_all(Path::new("first/second")).is_err());
    assert!(prefix.final_path().join("first").is_dir());
    assert!(!prefix.final_path().join("first/second").exists());
    assert!(
        prefix.check_binding().is_err(),
        "partially created output remained ready after its entry quota failed"
    );
}

#[test]
fn directories_and_exclusive_files_have_exact_final_bytes_modes_and_receipts() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    prefix
        .create_dir_all(Path::new("lib/site/namespace"))
        .unwrap();
    prefix
        .create_dir_all(Path::new("lib/site/namespace"))
        .unwrap();
    write_file(
        &mut prefix,
        "lib/site/namespace/中文,\"resource\n.py",
        b"VALUE = 42\n",
        0o644,
    );
    let path = prefix
        .final_path()
        .join("lib/site/namespace/中文,\"resource\n.py");
    assert_eq!(fs::read(&path).unwrap(), b"VALUE = 42\n");
    assert_eq!(mode(&path), 0o644);
    assert_eq!(mode(&prefix.final_path().join("lib/site/namespace")), 0o755);
    assert!(
        prefix
            .create_file_exclusive(Path::new("lib/site/namespace/中文,\"resource\n.py"), 0o644)
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), b"VALUE = 42\n");
}

#[test]
fn parent_and_leaf_symlinks_cannot_redirect_a_prefix_write() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    let outside = fixture.parent.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"external").unwrap();
    symlink(&outside, prefix.final_path().join("linked-parent")).unwrap();
    symlink(
        outside.join("keep"),
        prefix.final_path().join("linked-file"),
    )
    .unwrap();
    assert!(
        prefix
            .create_dir_all(Path::new("linked-parent/new"))
            .is_err()
    );
    assert!(
        prefix
            .create_file_exclusive(Path::new("linked-parent/new"), 0o644)
            .is_err()
    );
    assert!(
        prefix
            .create_file_exclusive(Path::new("linked-file"), 0o644)
            .is_err()
    );
    assert!(!outside.join("new").exists());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"external");
}

#[test]
fn replacing_a_directory_after_creation_is_detected_before_following_it() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    prefix.create_dir_all(Path::new("owned")).unwrap();
    let held = prefix.final_path().join("original-owned");
    fs::rename(prefix.final_path().join("owned"), held).unwrap();
    let outside = fixture.parent.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, prefix.final_path().join("owned")).unwrap();
    assert!(
        prefix
            .create_file_exclusive(Path::new("owned/escaped"), 0o644)
            .is_err()
    );
    assert!(!outside.join("escaped").exists());
}

#[test]
fn a_replaced_generation_entry_is_never_cleaned_as_if_it_were_owned() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    write_file(&mut prefix, "mine", b"owned", 0o644);
    let original = prefix.final_path().to_path_buf();
    let held = fixture.store.join("held-original-generation");
    fs::rename(&original, &held).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("other-owner"), b"must survive").unwrap();
    assert!(prefix.check_binding().is_err());
    assert!(prefix.discard().is_err());
    drop(prefix);
    assert_eq!(
        fs::read(original.join("other-owner")).unwrap(),
        b"must survive"
    );
    assert_eq!(fs::read(held.join("mine")).unwrap(), b"owned");
}

#[test]
fn empty_generation_explicit_discard_and_drop_remove_only_their_entries() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let mut first = store.allocate().unwrap();
    let second = store.allocate().unwrap();
    let first_path = first.final_path().to_path_buf();
    let second_path = second.final_path().to_path_buf();
    assert!(first_path.is_dir() && second_path.is_dir());
    first.discard().unwrap();
    first.discard().unwrap();
    assert!(!first_path.exists());
    assert!(second_path.is_dir());
    drop(first);
    assert!(second_path.is_dir());
    drop(second);
    assert!(!second_path.exists());
}

#[test]
fn discard_and_drop_clean_only_their_own_unpublished_generation() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let mut first = store.allocate().unwrap();
    let mut second = store.allocate().unwrap();
    assert!(first.final_path().is_dir() && second.final_path().is_dir());
    write_file(&mut first, "first", b"first", 0o644);
    write_file(&mut second, "second", b"second", 0o644);
    let first_path = first.final_path().to_path_buf();
    let second_path = second.final_path().to_path_buf();
    first.discard().unwrap();
    first.discard().unwrap();
    assert!(!first_path.exists());
    assert_eq!(fs::read(second_path.join("second")).unwrap(), b"second");
    drop(first);
    drop(second);
    assert!(!second_path.exists());
}

#[test]
fn mark_failed_rejects_ready_outputs_and_later_writes_but_preserves_discard() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    write_file(
        &mut prefix,
        "complete",
        b"finished before caller error",
        0o644,
    );
    prefix.check_binding().unwrap();
    let final_path = prefix.final_path().to_path_buf();
    prefix.mark_failed();
    assert!(
        prefix.check_binding().is_err(),
        "caller failure left a ready allocation"
    );
    assert!(
        prefix
            .create_file_exclusive(Path::new("later"), 0o644)
            .is_err()
    );
    assert!(prefix.create_dir_all(Path::new("later-directory")).is_err());
    assert_eq!(
        fs::read(final_path.join("complete")).unwrap(),
        b"finished before caller error"
    );
    assert_eq!(mode(&final_path.join("complete")), 0o644);
    assert!(!final_path.join("later").exists());
    assert!(!final_path.join("later-directory").exists());
    prefix.discard().unwrap();
    assert!(!final_path.exists());
}

#[test]
fn unfinished_file_taints_the_allocation_instead_of_returning_completion_facts() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    {
        let mut writer = prefix
            .create_file_exclusive(Path::new("partial"), 0o644)
            .unwrap();
        writer.write_all(b"partial").unwrap();
    }
    assert!(
        prefix.check_binding().is_err(),
        "unfinished output was accepted as ready"
    );
    prefix.discard().unwrap();
}

#[test]
fn build_lease_is_held_on_the_exact_allocated_generation() {
    use nix::fcntl::{Flock, FlockArg};
    let fixture = Fixture::new();
    let prefix = fixture.allocate();
    let path = fixture
        .store
        .join(".leases/build")
        .join(format!("{}.lock", prefix.allocation_id()));
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(Flock::lock(lease, FlockArg::LockExclusiveNonblock).is_err());
}

#[test]
fn zero_allocation_limits_fail_before_creating_the_store() {
    for invalid in [
        PrefixLimits {
            max_components: 0,
            ..Default::default()
        },
        PrefixLimits {
            max_entries: 0,
            ..Default::default()
        },
        PrefixLimits {
            max_path_bytes: 0,
            ..Default::default()
        },
        PrefixLimits {
            max_total_bytes: 0,
            ..Default::default()
        },
        PrefixLimits {
            max_cleanup_entries: 0,
            ..Default::default()
        },
    ] {
        let fixture = Fixture::new();
        assert!(EnvironmentStore::open(&fixture.store, uid(), invalid).is_err());
        assert!(!fixture.store.exists());
    }
}

#[test]
fn file_resource_limits_fail_without_returning_completion_facts() {
    let fixture = Fixture::new();
    let store = EnvironmentStore::open(
        &fixture.store,
        uid(),
        PrefixLimits {
            max_entries: 1,
            max_total_bytes: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    assert!(prefix.final_path().is_dir());
    let mut writer = prefix
        .create_file_exclusive(Path::new("one"), 0o644)
        .unwrap();
    assert!(writer.write_all(b"too large").is_err());
    drop(writer);
    assert!(
        prefix
            .create_file_exclusive(Path::new("two"), 0o644)
            .is_err()
    );
    prefix.discard().unwrap();
}

#[test]
fn cleanup_failure_retains_an_owned_recovery_location() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    write_file(&mut prefix, "mine", b"owned", 0o644);
    // An unknown inode cannot become ours just because it appears below our
    // root. The safe policy is to retain the allocation for recovery.
    fs::write(
        prefix.final_path().join("untracked-foreign-entry"),
        b"not ours",
    )
    .unwrap();
    assert!(prefix.discard().is_err());
    let recovery = prefix
        .recovery_path()
        .expect("cleanup error omitted its retained location")
        .to_path_buf();
    assert!(recovery.is_dir());
    drop(prefix);
    assert_eq!(
        fs::read(recovery.join("untracked-foreign-entry")).unwrap(),
        b"not ours"
    );
}

#[test]
fn file_finish_rejects_a_replaced_leaf_without_writing_through_its_symlink() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    let file = prefix.final_path().join("output");
    let held = prefix.final_path().join("held-original-output");
    let outside = fixture.parent.join("external-output");
    fs::write(&outside, b"external must not be overwritten").unwrap();
    let mut writer = prefix
        .create_file_exclusive(Path::new("output"), 0o644)
        .unwrap();
    writer.write_all(b"our file bytes").unwrap();
    fs::rename(&file, held).unwrap();
    symlink(&outside, &file).unwrap();
    assert!(writer.finish().is_err());
    drop(prefix);
    assert_eq!(
        fs::read(outside).unwrap(),
        b"external must not be overwritten"
    );
}

#[test]
fn a_changed_candidate_captured_for_discard_is_restored_without_deleting_it() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate();
    write_file(&mut prefix, "mine", b"our original generation", 0o644);
    let original = prefix.final_path().to_path_buf();
    let held = fixture.store.join("held-original");
    let foreign = fixture.store.join("another-owner");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), b"another owner's generation").unwrap();
    let error = prefix
        .discard_with_capture_hook(|| {
            fs::rename(&original, &held)?;
            fs::rename(&foreign, &original)?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("identity") || error.to_string().contains("retained"),
        "{error:#}"
    );
    drop(prefix);
    assert_eq!(
        fs::read(original.join("keep")).unwrap(),
        b"another owner's generation"
    );
    assert_eq!(
        fs::read(held.join("mine")).unwrap(),
        b"our original generation"
    );
}

#[test]
fn noclobber_restoration_cannot_replace_even_an_empty_existing_directory() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let source = fixture.parent.join("source");
    let target = fixture.parent.join("occupied-empty-target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(source.join("keep"), b"retained candidate").unwrap();
    let target_before = fs::metadata(&target).unwrap();
    let parent = fs::File::open(&fixture.parent).unwrap();
    assert!(
        rename_noclobber(
            &parent,
            std::ffi::OsStr::new("source"),
            &parent,
            std::ffi::OsStr::new("occupied-empty-target")
        )
        .is_err()
    );
    assert_eq!(
        fs::read(source.join("keep")).unwrap(),
        b"retained candidate"
    );
    let target_after = fs::metadata(target).unwrap();
    assert_eq!(
        (target_after.dev(), target_after.ino()),
        (target_before.dev(), target_before.ino())
    );
}
