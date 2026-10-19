use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

struct Root {
    _owner: tempfile::TempDir,
    path: PathBuf,
    file: File,
}
impl Root {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let path = owner.path().canonicalize().unwrap().join("payload");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            file: File::open(&path).unwrap(),
            path,
            _owner: owner,
        }
    }
    fn file(&self, path: &str, bytes: &[u8]) {
        let path = self.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fn observe(&self) -> Result<TreeInventory> {
        observe_tree_inventory(self, PrefixInventoryLimits::default())
    }
}
impl InventoryRoot for Root {
    fn root_file(&self) -> &File {
        &self.file
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        "language-fixture"
    }
    fn expected_uid(&self) -> u32 {
        nix::unistd::geteuid().as_raw()
    }
    fn check_binding(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path)?;
        let pinned = self.file.metadata()?;
        ensure!(
            current.is_dir() && (current.dev(), current.ino()) == (pinned.dev(), pinned.ino()),
            "test root binding replaced"
        );
        Ok(())
    }
}

#[test]
fn r06_tree_inventory_records_complete_pinned_content_and_roundtrips() {
    let root = Root::new();
    root.file("bin/python", b"runtime");
    root.file("lib/中文.py", b"value=42\n");
    fs::create_dir(root.path.join("empty")).unwrap();
    let inventory = root.observe().unwrap();
    assert_eq!(inventory.entries().len(), 5);
    assert!(inventory.contains_file(Path::new("bin/python")));
    assert!(!inventory.contains_file(Path::new("empty")));
    assert_eq!(inventory.total_bytes(), 16);
    assert_eq!(
        inventory,
        serde_json::from_slice::<TreeInventory>(&serde_json::to_vec(&inventory).unwrap()).unwrap()
    );
    assert_eq!(inventory, root.observe().unwrap());
    fs::write(root.path.join("lib/中文.py"), b"value=43\n").unwrap();
    assert_ne!(inventory, root.observe().unwrap());
}

#[test]
fn r06_tree_inventory_tracks_contained_relative_link_chains_without_copying() {
    let root = Root::new();
    root.file("bin/python3.15", b"runtime");
    symlink("python3.15", root.path.join("bin/python3")).unwrap();
    symlink("python3", root.path.join("bin/python")).unwrap();
    symlink("../bin/python", root.path.join("back")).unwrap();
    // The root-level link escapes before re-entering and must not be accepted.
    assert!(root.observe().is_err());
    fs::remove_file(root.path.join("back")).unwrap();
    let observed = root.observe().unwrap();
    assert_eq!(observed.total_bytes(), 7);
    assert!(observed.entries().iter().any(|entry| entry.relative == Path::new("bin/python") && matches!(&entry.kind, TreeInventoryKind::Symlink { target } if target == Path::new("python3"))));
    assert_eq!(
        fs::read_link(root.path.join("bin/python")).unwrap(),
        Path::new("python3")
    );
}

#[test]
fn r06_tree_inventory_rejects_links_that_escape_dangle_cycle_or_exceed_budget() {
    for target in ["/bin/sh", "../outside", "missing", "link"] {
        let root = Root::new();
        root.file("bin/python", b"runtime");
        symlink(target, root.path.join("link")).unwrap();
        assert!(root.observe().is_err(), "target {target}");
    }
    let root = Root::new();
    root.file("file", b"safe");
    symlink("b", root.path.join("a")).unwrap();
    symlink("a", root.path.join("b")).unwrap();
    assert!(root.observe().is_err());
    fs::remove_file(root.path.join("b")).unwrap();
    symlink("file", root.path.join("b")).unwrap();
    assert!(root.observe().is_ok());
    assert!(
        observe_tree_inventory(
            &root,
            PrefixInventoryLimits {
                max_components: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn r06_tree_inventory_refuses_replaced_root_special_files_hardlinks_and_limits() {
    let root = Root::new();
    root.file("file", b"safe");
    assert!(root.observe().is_ok());
    fs::hard_link(root.path.join("file"), root.path.join("alias")).unwrap();
    assert!(root.observe().is_err());
    fs::remove_file(root.path.join("alias")).unwrap();
    assert!(
        observe_tree_inventory(
            &root,
            PrefixInventoryLimits {
                max_total_bytes: 3,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        observe_tree_inventory(
            &root,
            PrefixInventoryLimits {
                max_files: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
    let displaced = root.path.with_file_name("displaced");
    fs::rename(&root.path, &displaced).unwrap();
    fs::create_dir(&root.path).unwrap();
    assert!(root.observe().is_err());
}

#[test]
fn r06_tree_inventory_does_not_follow_symlink_directory_as_traversal() {
    let root = Root::new();
    root.file("real/stdlib.py", b"safe");
    symlink("real", root.path.join("alias")).unwrap();
    let observed = root.observe().unwrap();
    assert_eq!(observed.entries().len(), 3);
    assert_eq!(observed.total_bytes(), 4);
    assert!(!observed.contains_file(Path::new("alias/stdlib.py")));
}

#[test]
fn r06_tree_inventory_total_budget_is_checked_before_file_bytes_are_read() {
    let root = Root::new();
    root.file("over-budget", b"not allowed within total budget");
    let mut entered_read = false;
    let result = observe_tree_with_hook(
        &root,
        PrefixInventoryLimits {
            max_total_bytes: 1,
            ..Default::default()
        },
        None,
        &mut |_| {
            entered_read = true;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(
        !entered_read,
        "observer began hashing before checking the total byte budget"
    );
}

#[test]
fn r06_tree_symlink_trailing_directory_syntax_matches_actual_resolution() {
    let mut accepted = Vec::new();
    for (target, resolves) in [
        ("file/", false),
        ("file/.", false),
        ("directory/", true),
        ("directory/.", true),
    ] {
        let root = Root::new();
        root.file("file", b"ordinary file");
        fs::create_dir(root.path.join("directory")).unwrap();
        symlink(target, root.path.join("link")).unwrap();
        let metadata = fs::metadata(root.path.join("link"));
        let opened = File::open(root.path.join("link"));
        assert_eq!(
            metadata.is_ok(),
            resolves,
            "actual filesystem metadata resolution for {target}"
        );
        assert_eq!(
            opened.is_ok(),
            resolves,
            "actual filesystem open for {target}"
        );
        if resolves {
            assert!(metadata.unwrap().is_dir());
            assert!(opened.unwrap().metadata().unwrap().is_dir());
        } else {
            assert_eq!(
                metadata.unwrap_err().raw_os_error(),
                Some(nix::libc::ENOTDIR)
            );
            assert_eq!(opened.unwrap_err().raw_os_error(), Some(nix::libc::ENOTDIR));
        }
        accepted.push((target, root.observe().is_ok()));
    }
    assert_eq!(
        accepted,
        [
            ("file/", false),
            ("file/.", false),
            ("directory/", true),
            ("directory/.", true)
        ],
        "inventory must preserve a link target's requirement that its final component is a directory"
    );
}
