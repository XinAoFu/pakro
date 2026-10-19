use super::*;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use sha2::Sha256;

use super::super::{EnvironmentStore, PrefixLimits};

struct Fixture {
    prefix: UnpublishedPrefix,
    files: Vec<PrefixExpectedFile>,
    directories: Vec<PrefixExpectedDirectory>,
    _owner: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::open(
            &owner.path().canonicalize().unwrap().join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let prefix = store.allocate().unwrap();
        prefix.check_binding().unwrap();
        Self {
            prefix,
            files: vec![],
            directories: vec![],
            _owner: owner,
        }
    }
    fn directory(&mut self, relative: &str) {
        let path = Path::new(relative);
        self.prefix.create_dir_all(path).unwrap();
        let mut partial = PathBuf::new();
        for component in path.components() {
            partial.push(component);
            if !self
                .directories
                .iter()
                .any(|entry| entry.relative == partial)
            {
                self.directories.push(PrefixExpectedDirectory {
                    relative: partial.clone(),
                    mode: 0o755,
                });
            }
        }
    }
    fn file(&mut self, relative: &str, bytes: &[u8], mode: u32) {
        if let Some(parent) = Path::new(relative)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            self.directory(parent.to_str().unwrap());
        }
        let mut output = self
            .prefix
            .create_file_exclusive(Path::new(relative), mode)
            .unwrap();
        output.write_all(bytes).unwrap();
        let receipt = output.finish().unwrap();
        assert_eq!(receipt.size(), bytes.len() as u64);
        assert_eq!(receipt.mode(), mode);
        self.prefix.check_binding().unwrap();
        self.files.push(PrefixExpectedFile {
            relative: relative.into(),
            sha256: Sha256::digest(bytes).into(),
            size: bytes.len() as u64,
            mode,
        });
    }
    fn populated() -> Self {
        let mut fixture = Self::new();
        fixture.directory("empty");
        fixture.file("bin/python", b"independent runtime executable\0", 0o755);
        fixture.file("lib/中文,package.py", b"value = 42\n", 0o644);
        fixture
    }
    fn verify(&self) -> Result<PrefixInventoryReceipt> {
        verify_prefix_inventory(
            &self.prefix,
            &self.files,
            &self.directories,
            PrefixInventoryLimits::default(),
        )
    }
}

fn error(result: Result<PrefixInventoryReceipt>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(error.to_string().contains(diagnostic), "{error:#}");
}

#[test]
fn complete_inventory_preserves_unicode_and_empty_directories_without_writing() {
    let fixture = Fixture::populated();
    let path = fixture.prefix.final_path().join("lib/中文,package.py");
    let before = fs::metadata(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let receipt = fixture.verify().unwrap();
    assert_eq!(receipt.allocation_id(), fixture.prefix.allocation_id());
    assert_eq!(receipt.final_prefix(), fixture.prefix.final_path());
    assert_eq!(receipt.file_count(), fixture.files.len());
    assert_eq!(receipt.directory_count(), fixture.directories.len());
    assert_eq!(
        receipt.total_bytes(),
        fixture.files.iter().map(|file| file.size).sum::<u64>()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.mode()),
        (after.dev(), after.ino(), after.mode())
    );
    assert_eq!(before.mtime(), after.mtime());
    fixture.prefix.check_binding().unwrap();
}

#[test]
fn inventory_digest_is_order_independent_and_commits_to_observed_facts() {
    let mut fixture = Fixture::populated();
    let first = fixture.verify().unwrap();
    fixture.files.reverse();
    fixture.directories.reverse();
    let reordered = fixture.verify().unwrap();
    assert_eq!(first.inventory_sha256(), reordered.inventory_sha256());
    let mut other = Fixture::new();
    other.file("different", b"other verified bytes", 0o644);
    let different = other.verify().unwrap();
    assert_ne!(first.inventory_sha256(), different.inventory_sha256());
}

#[test]
fn empty_inventory_still_validates_the_actual_root_and_allocation() {
    let fixture = Fixture::new();
    let receipt = fixture.verify().unwrap();
    assert_eq!(
        (
            receipt.file_count(),
            receipt.directory_count(),
            receipt.total_bytes()
        ),
        (0, 0, 0)
    );
    fs::set_permissions(
        fixture.prefix.final_path(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    error(fixture.verify(), "prefix inventory root");
}

#[test]
fn same_size_content_corruption_and_wrong_expected_hash_are_rejected() {
    let mut fixture = Fixture::populated();
    fixture.files[0].sha256[0] ^= 1;
    error(fixture.verify(), "prefix inventory digest");
    fixture.files[0].sha256[0] ^= 1;
    let target = fixture.prefix.final_path().join(&fixture.files[0].relative);
    fs::write(target, vec![b'X'; fixture.files[0].size as usize]).unwrap();
    error(fixture.verify(), "prefix inventory digest");
}

#[test]
fn actual_size_and_declared_size_cannot_be_substituted_for_eof() {
    for delta in [false, true] {
        let mut fixture = Fixture::populated();
        if delta {
            fixture.files[0].size += 1;
        } else {
            let target = fixture.prefix.final_path().join(&fixture.files[0].relative);
            fs::write(target, b"short").unwrap();
        }
        error(fixture.verify(), "prefix inventory size");
    }
}

#[test]
fn file_modes_privileged_bits_and_directory_modes_are_actual_facts() {
    for (relative, mode) in [
        ("bin/python", 0o644),
        ("bin/python", 0o4755),
        ("lib", 0o700),
    ] {
        let fixture = Fixture::populated();
        fs::set_permissions(
            fixture.prefix.final_path().join(relative),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        error(fixture.verify(), "prefix inventory mode");
    }
}

#[test]
fn an_external_hardlink_is_rejected_even_when_inode_bytes_and_mode_match() {
    let fixture = Fixture::populated();
    let target = fixture.prefix.final_path().join("lib/中文,package.py");
    let outside = fixture._owner.path().join("outside-hardlink");
    let before = fs::read(&target).unwrap();
    fs::hard_link(&target, &outside).unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().ino(),
        fs::metadata(&outside).unwrap().ino()
    );
    error(fixture.verify(), "prefix inventory links");
    assert_eq!(fs::read(outside).unwrap(), before);
}

#[test]
fn missing_unplanned_and_later_tracked_entries_are_rejected() {
    for action in 0..3 {
        let mut fixture = Fixture::populated();
        match action {
            0 => fs::remove_file(fixture.prefix.final_path().join("bin/python")).unwrap(),
            1 => fs::write(fixture.prefix.final_path().join("unexpected"), b"unowned").unwrap(),
            _ => {
                let expected_files = fixture.files.clone();
                fixture.file(
                    "later-output",
                    b"tracked but absent from expected inventory",
                    0o644,
                );
                fixture.files = expected_files;
            }
        }
        error(fixture.verify(), "prefix inventory entry");
    }
}

#[test]
fn omitted_empty_directory_is_not_silently_accepted() {
    let mut fixture = Fixture::populated();
    fixture
        .directories
        .retain(|entry| entry.relative != Path::new("empty"));
    error(fixture.verify(), "prefix inventory entry");
}

#[test]
fn symlink_leaf_and_parent_cannot_authenticate_an_outside_file() {
    for replace_parent in [false, true] {
        let fixture = Fixture::populated();
        let outside = fixture._owner.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let external = outside.join("python");
        let original = fs::read(fixture.prefix.final_path().join("bin/python")).unwrap();
        fs::write(&external, &original).unwrap();
        if replace_parent {
            fs::rename(
                fixture.prefix.final_path().join("bin"),
                fixture._owner.path().join("held-bin"),
            )
            .unwrap();
            symlink(&outside, fixture.prefix.final_path().join("bin")).unwrap();
        } else {
            fs::remove_file(fixture.prefix.final_path().join("bin/python")).unwrap();
            symlink(&external, fixture.prefix.final_path().join("bin/python")).unwrap();
        }
        error(fixture.verify(), "prefix inventory entry");
        assert_eq!(fs::read(external).unwrap(), original);
    }
}

#[test]
fn fifo_leaf_is_rejected_without_a_blocking_open() {
    let fixture = Fixture::populated();
    let target = fixture.prefix.final_path().join("bin/python");
    fs::remove_file(&target).unwrap();
    nix::unistd::mkfifo(&target, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    error(fixture.verify(), "prefix inventory entry");
}

#[test]
fn changed_namespace_and_failed_allocations_cannot_get_a_receipt() {
    for failed in [false, true] {
        let mut fixture = Fixture::populated();
        if failed {
            fixture.prefix.mark_failed();
        } else {
            fs::rename(
                fixture.prefix.final_path(),
                fixture._owner.path().join("held-allocation"),
            )
            .unwrap();
            fs::create_dir(fixture.prefix.final_path()).unwrap();
        }
        error(fixture.verify(), "prefix inventory allocation");
    }
}

#[test]
fn unsafe_or_duplicate_expected_paths_and_conflicting_kinds_are_rejected() {
    for variant in 0..8 {
        let mut fixture = Fixture::populated();
        match variant {
            0 => fixture.files[0].relative = PathBuf::from("/absolute"),
            1 => fixture.files[0].relative = PathBuf::from("../escape"),
            2 => fixture.files[0].relative = PathBuf::from("bin/./python"),
            3 => fixture.files[0].relative = PathBuf::from("bin/py\0thon"),
            4 => fixture.files.push(fixture.files[0].clone()),
            5 => fixture.directories.push(fixture.directories[0].clone()),
            6 => fixture.directories.push(PrefixExpectedDirectory {
                relative: "bin/python".into(),
                mode: 0o755,
            }),
            _ => fixture.files[0].mode = 0o4755,
        }
        error(fixture.verify(), "prefix inventory input");
    }
}

#[test]
fn expected_path_count_bytes_depth_and_content_limits_are_preflighted() {
    let fixture = Fixture::populated();
    for limits in [
        PrefixInventoryLimits {
            max_files: 0,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_directories: 0,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_files: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_directories: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_components: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_path_bytes: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_total_path_bytes: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_file_bytes: 1,
            ..Default::default()
        },
        PrefixInventoryLimits {
            max_total_bytes: 1,
            ..Default::default()
        },
    ] {
        error(
            verify_prefix_inventory(
                &fixture.prefix,
                &fixture.files,
                &fixture.directories,
                limits,
            ),
            "prefix inventory limit",
        );
    }
    let mut enormous = fixture.files.clone();
    enormous[0].size = u64::MAX;
    error(
        verify_prefix_inventory(
            &fixture.prefix,
            &enormous,
            &fixture.directories,
            PrefixInventoryLimits::default(),
        ),
        "prefix inventory limit",
    );
    fixture.prefix.check_binding().unwrap();
}

#[test]
fn rejected_inventory_does_not_mutate_or_taint_the_valid_allocation() {
    let fixture = Fixture::populated();
    let mut wrong = fixture.files.clone();
    wrong[0].sha256[0] ^= 1;
    error(
        verify_prefix_inventory(
            &fixture.prefix,
            &wrong,
            &fixture.directories,
            PrefixInventoryLimits::default(),
        ),
        "prefix inventory digest",
    );
    fixture.prefix.check_binding().unwrap();
    fixture.verify().unwrap();
}
