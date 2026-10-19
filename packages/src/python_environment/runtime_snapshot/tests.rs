use super::*;
use crate::packages::python_environment::prefix::verify::{
    PrefixInventoryLimits, verify_prefix_inventory,
};
use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    store: EnvironmentStore,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let source = base.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("lib/pkg")).unwrap();
        fs::write(source.join("bin/python"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::write(source.join("lib/pkg/core.py"), b"VALUE=42\n").unwrap();
        fs::write(source.join("lib/config.txt"), b"config\n").unwrap();
        fs::set_permissions(source.join("bin/python"), fs::Permissions::from_mode(0o775)).unwrap();
        fs::set_permissions(
            source.join("lib/pkg/core.py"),
            fs::Permissions::from_mode(0o664),
        )
        .unwrap();
        fs::set_permissions(
            source.join("lib/config.txt"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::set_permissions(source.join("lib/pkg"), fs::Permissions::from_mode(0o700)).unwrap();
        let store = EnvironmentStore::open(
            &base.join("snapshots"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        Self {
            root,
            source,
            store,
        }
    }
    fn copy(&self) -> Result<OwnedRuntimeSnapshot> {
        self.copy_with(Path::new("bin/python"), RuntimeSnapshotLimits::default())
    }
    fn copy_with(
        &self,
        executable: &Path,
        limits: RuntimeSnapshotLimits,
    ) -> Result<OwnedRuntimeSnapshot> {
        snapshot_runtime(
            &self.source,
            executable,
            self.store.allocate().unwrap(),
            limits,
        )
    }
}

fn pair(path: &Path) -> [u64; 2] {
    let metadata = fs::symlink_metadata(path).unwrap();
    [metadata.dev(), metadata.ino()]
}

#[test]
fn complete_tree_is_copied_to_independent_inodes_with_the_full_v1_manifest() {
    let fixture = Fixture::new();
    let source_mode = fs::metadata(fixture.source.join("bin/python"))
        .unwrap()
        .mode();
    let snapshot = fixture
        .copy()
        .expect("complete real runtime snapshot is required");
    assert_eq!(snapshot.files().len(), 3);
    assert_eq!(snapshot.directories().len(), 3);
    assert_eq!(snapshot.entrypoint(), snapshot.root().join("bin/python"));
    assert_eq!(
        fs::metadata(snapshot.root()).unwrap().mode() & 0o7777,
        0o700
    );
    for directory in snapshot.directories() {
        assert_eq!(directory.mode, 0o755);
        assert_eq!(
            fs::metadata(snapshot.root().join(&directory.relative))
                .unwrap()
                .mode()
                & 0o7777,
            0o755
        );
    }
    for file in snapshot.files() {
        let source = fixture.source.join(&file.relative);
        let copied = snapshot.root().join(&file.relative);
        assert_eq!(fs::read(&source).unwrap(), fs::read(&copied).unwrap());
        assert_ne!(pair(&source), pair(&copied));
        assert_eq!(fs::metadata(copied).unwrap().nlink(), 1);
    }
    assert_eq!(
        fs::metadata(snapshot.entrypoint()).unwrap().mode() & 0o7777,
        0o755
    );
    assert_eq!(
        fs::metadata(snapshot.root().join("lib/pkg/core.py"))
            .unwrap()
            .mode()
            & 0o7777,
        0o644
    );
    assert_eq!(
        fs::metadata(fixture.source.join("bin/python"))
            .unwrap()
            .mode(),
        source_mode
    );
    let receipt = verify_prefix_inventory(
        &snapshot.prefix,
        snapshot.files(),
        snapshot.directories(),
        PrefixInventoryLimits::default(),
    )
    .unwrap();
    assert_eq!(receipt.inventory_sha256(), snapshot.inventory_sha256());
    assert_eq!(receipt.file_count(), 3);
}

#[test]
fn internal_symlinks_become_independent_regular_outputs_at_their_logical_paths() {
    let fixture = Fixture::new();
    symlink("python", fixture.source.join("bin/alias")).unwrap();
    symlink(
        fixture.source.join("bin/python"),
        fixture.source.join("bin/absolute-alias"),
    )
    .unwrap();
    symlink("lib/pkg", fixture.source.join("other-stdlib")).unwrap();
    symlink("../config.txt", fixture.source.join("lib/pkg/data-alias")).unwrap();
    let snapshot = fixture
        .copy_with(Path::new("bin/alias"), RuntimeSnapshotLimits::default())
        .unwrap();
    assert_eq!(snapshot.entrypoint(), snapshot.root().join("bin/alias"));
    for relative in [
        "bin/alias",
        "bin/absolute-alias",
        "other-stdlib/core.py",
        "other-stdlib/data-alias",
    ] {
        assert!(
            fs::symlink_metadata(snapshot.root().join(relative))
                .unwrap()
                .is_file()
        );
    }
    assert!(
        fs::symlink_metadata(snapshot.root().join("other-stdlib"))
            .unwrap()
            .is_dir()
    );
    assert_ne!(
        pair(&snapshot.root().join("bin/python")),
        pair(snapshot.entrypoint())
    );
    assert_ne!(
        pair(&snapshot.root().join("lib/pkg/core.py")),
        pair(&snapshot.root().join("other-stdlib/core.py"))
    );
    assert_eq!(
        fs::read(snapshot.root().join("other-stdlib/data-alias")).unwrap(),
        b"config\n"
    );
    assert!(
        fs::symlink_metadata(fixture.source.join("bin/alias"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn actual_active_directory_cycles_and_pure_symlink_cycles_are_bounded_errors() {
    for variant in ["directory", "symlink"] {
        let fixture = Fixture::new();
        if variant == "directory" {
            symlink("..", fixture.source.join("lib/pkg/cycle")).unwrap();
        } else {
            symlink("loop-b", fixture.source.join("loop-a")).unwrap();
            symlink("loop-a", fixture.source.join("loop-b")).unwrap();
        }
        let start = Instant::now();
        assert!(fixture.copy().is_err(), "accepted {variant} cycle");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn external_file_and_directory_symlinks_are_rejected_without_external_writes() {
    for variant in ["file", "directory"] {
        let fixture = Fixture::new();
        let outside = fixture.root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("valuable"), b"external original").unwrap();
        let original = pair(&outside.join("valuable"));
        symlink(
            if variant == "file" {
                outside.join("valuable")
            } else {
                outside.clone()
            },
            fixture.source.join("external"),
        )
        .unwrap();
        assert!(fixture.copy().is_err(), "accepted external {variant}");
        assert_eq!(
            fs::read(outside.join("valuable")).unwrap(),
            b"external original"
        );
        assert_eq!(pair(&outside.join("valuable")), original);
    }
}

#[test]
fn fifo_sources_are_rejected_without_blocking() {
    let fixture = Fixture::new();
    nix::unistd::mkfifo(
        &fixture.source.join("pipe"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    let start = Instant::now();
    assert!(fixture.copy().is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn privileged_files_and_unreadable_or_privileged_directories_are_rejected() {
    for (relative, mode) in [
        ("bin/python", 0o4755),
        ("lib/pkg", 0o2755),
        ("lib/pkg", 0o300),
        ("lib/pkg", 0o600),
    ] {
        let fixture = Fixture::new();
        let path = fixture.source.join(relative);
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let result = fixture.copy();
        let unchanged = fs::metadata(&path).unwrap().mode() & 0o7777;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err(), "accepted {relative} mode {mode:o}");
        assert_eq!(unchanged, mode);
    }
}

#[test]
fn all_limits_are_checked_before_a_successful_snapshot_can_escape() {
    for limits in [
        RuntimeSnapshotLimits {
            max_entries: 5,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_depth: 2,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_path_bytes: 4,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_total_path_bytes: 5,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_file_bytes: 2,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_total_bytes: 3,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_entries: 0,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_depth: 0,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_path_bytes: 0,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_total_path_bytes: 0,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_file_bytes: 0,
            ..RuntimeSnapshotLimits::default()
        },
        RuntimeSnapshotLimits {
            max_total_bytes: 0,
            ..RuntimeSnapshotLimits::default()
        },
    ] {
        let fixture = Fixture::new();
        assert!(
            fixture.copy_with(Path::new("bin/python"), limits).is_err(),
            "accepted {limits:?}"
        );
        assert_eq!(
            fs::read(fixture.source.join("lib/pkg/core.py")).unwrap(),
            b"VALUE=42\n"
        );
    }
}

#[test]
fn entrypoint_must_be_a_contained_relative_regular_executable() {
    let fixture = Fixture::new();
    for relative in [
        "",
        ".",
        "../bin/python",
        "/bin/python",
        "missing",
        "bin",
        "lib/config.txt",
    ] {
        assert!(
            fixture
                .copy_with(Path::new(relative), RuntimeSnapshotLimits::default())
                .is_err(),
            "accepted {relative}"
        );
    }
}

#[test]
fn copied_bytes_survive_the_complete_original_tree_being_removed() {
    let fixture = Fixture::new();
    let snapshot = fixture.copy().unwrap();
    let old = fixture.root.path().join("old-source");
    fs::rename(&fixture.source, &old).unwrap();
    fs::remove_dir_all(old).unwrap();
    assert_eq!(
        fs::read(snapshot.root().join("lib/pkg/core.py")).unwrap(),
        b"VALUE=42\n"
    );
    assert_eq!(
        fs::read(snapshot.entrypoint()).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    verify_prefix_inventory(
        &snapshot.prefix,
        snapshot.files(),
        snapshot.directories(),
        PrefixInventoryLimits::default(),
    )
    .unwrap();
}

#[test]
fn dropping_the_owned_snapshot_runs_real_prefix_cleanup_without_touching_source() {
    let fixture = Fixture::new();
    let snapshot = fixture.copy().unwrap();
    let destination = snapshot.root().to_owned();
    drop(snapshot);
    assert!(!destination.exists());
    assert_eq!(
        fs::read(fixture.source.join("lib/pkg/core.py")).unwrap(),
        b"VALUE=42\n"
    );
}

#[test]
fn content_tree_digest_is_stable_across_distinct_local_snapshot_instances() {
    use sha2::{Digest as _, Sha256};

    let fixture = Fixture::new();
    let first = fixture.copy().unwrap();
    let second = fixture.copy().unwrap();
    assert_ne!(first.root(), second.root());
    assert_ne!(pair(first.root()), pair(second.root()));
    assert_ne!(first.inventory_sha256(), second.inventory_sha256());

    // This hand-built recipe is independent of the production manifest hasher.
    let mut expected = Sha256::new();
    expected.update(b"pakro-runtime-snapshot-content\0");
    expected.update(1u32.to_le_bytes());
    expected.update(10u64.to_le_bytes());
    expected.update(b"bin/python");
    for (path, kind, mode, bytes) in [
        (b"bin".as_slice(), b'd', 0o755u32, None),
        (
            b"bin/python".as_slice(),
            b'f',
            0o755,
            Some(b"#!/bin/sh\nexit 0\n".as_slice()),
        ),
        (b"lib".as_slice(), b'd', 0o755, None),
        (
            b"lib/config.txt".as_slice(),
            b'f',
            0o644,
            Some(b"config\n".as_slice()),
        ),
        (b"lib/pkg".as_slice(), b'd', 0o755, None),
        (
            b"lib/pkg/core.py".as_slice(),
            b'f',
            0o644,
            Some(b"VALUE=42\n".as_slice()),
        ),
    ] {
        expected.update((path.len() as u64).to_le_bytes());
        expected.update(path);
        expected.update([kind]);
        expected.update(mode.to_le_bytes());
        expected.update(bytes.map_or(0, |bytes| bytes.len() as u64).to_le_bytes());
        expected.update(bytes.map_or([0; 32], |bytes| Sha256::digest(bytes).into()));
    }
    let expected: [u8; 32] = expected.finalize().into();
    assert_eq!(first.content_tree_sha256(), &expected);
    assert_eq!(second.content_tree_sha256(), &expected);
}

#[test]
fn source_root_replacement_and_file_mutation_after_preflight_are_rejected() {
    for variant in ["root", "file"] {
        let fixture = Fixture::new();
        let result = snapshot_with_source_hook(
            &fixture.source,
            Path::new("bin/python"),
            fixture.store.allocate().unwrap(),
            RuntimeSnapshotLimits::default(),
            || {
                if variant == "root" {
                    fs::rename(&fixture.source, fixture.root.path().join("saved-source"))?;
                    fs::create_dir(&fixture.source)?;
                } else {
                    fs::write(fixture.source.join("lib/pkg/core.py"), b"VALUE=43\n")?;
                }
                Ok(())
            },
        );
        assert!(result.is_err(), "accepted changed {variant}");
    }
}

#[test]
fn destination_must_be_an_empty_real_prefix() {
    let fixture = Fixture::new();
    let mut destination = fixture.store.allocate().unwrap();
    destination.create_dir_all(Path::new("existing")).unwrap();
    assert!(
        snapshot_runtime(
            &fixture.source,
            Path::new("bin/python"),
            destination,
            RuntimeSnapshotLimits::default()
        )
        .is_err()
    );
    assert_eq!(
        fs::read(fixture.source.join("lib/pkg/core.py")).unwrap(),
        b"VALUE=42\n"
    );
}

#[test]
fn source_must_not_contain_the_destination_allocation() {
    let fixture = Fixture::new();
    assert!(
        snapshot_runtime(
            &fixture.root.path().canonicalize().unwrap(),
            Path::new("source/bin/python"),
            fixture.store.allocate().unwrap(),
            RuntimeSnapshotLimits::default()
        )
        .is_err()
    );
    assert_eq!(
        fs::read(fixture.source.join("lib/pkg/core.py")).unwrap(),
        b"VALUE=42\n"
    );
}

#[test]
fn physical_source_ancestor_is_rejected_before_copy_through_a_case_alias() {
    let fixture = Fixture::new();
    let alias = fixture.source.with_file_name("SOURCE");
    if !alias.exists() {
        return; // This filesystem has no corresponding case alias.
    }
    assert_eq!(pair(&alias), pair(&fixture.source));
    eprintln!("case-alias physical overlap oracle reached");
    let inner = EnvironmentStore::open(
        &fixture.source.join("nested-store"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let original = pair(&fixture.source.join("lib/pkg/core.py"));
    let error = snapshot_with_source_hook(
        &alias,
        Path::new("bin/python"),
        inner.allocate().unwrap(),
        RuntimeSnapshotLimits::default(),
        || eyre::bail!("copy boundary reached before rejecting physical overlap"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("roots overlap"), "{error:#}");
    assert_eq!(pair(&fixture.source.join("lib/pkg/core.py")), original);
    assert_eq!(
        fs::read(fixture.source.join("lib/pkg/core.py")).unwrap(),
        b"VALUE=42\n"
    );
}

#[test]
fn an_internal_absolute_symlink_stays_internal_when_the_source_root_uses_a_case_alias() {
    let fixture = Fixture::new();
    let alias = fixture.source.with_file_name("SOURCE");
    if !alias.exists() {
        return;
    }
    assert_eq!(pair(&alias), pair(&fixture.source));
    eprintln!("case-alias internal-link oracle reached");
    symlink(
        fixture.source.join("bin/python"),
        fixture.source.join("bin/absolute-alias"),
    )
    .unwrap();
    let snapshot = snapshot_runtime(
        &alias,
        Path::new("bin/absolute-alias"),
        fixture.store.allocate().unwrap(),
        RuntimeSnapshotLimits::default(),
    )
    .expect("physical internal link must survive source case alias");
    assert_eq!(
        fs::read(snapshot.entrypoint()).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
}

#[test]
fn executable_path_budgets_are_rejected_before_walking_unsafe_source_entries() {
    for (executable, limits) in [
        (
            PathBuf::from("x".repeat(513)),
            RuntimeSnapshotLimits {
                max_path_bytes: 512,
                ..RuntimeSnapshotLimits::default()
            },
        ),
        (
            PathBuf::from("one/two/three"),
            RuntimeSnapshotLimits {
                max_depth: 2,
                ..RuntimeSnapshotLimits::default()
            },
        ),
    ] {
        let fixture = Fixture::new();
        nix::unistd::mkfifo(
            &fixture.source.join("unsafe"),
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .unwrap();
        let error = fixture.copy_with(&executable, limits).unwrap_err();
        assert!(
            error.to_string().contains("executable path or depth limit"),
            "{error:#}"
        );
    }
}

fn live(source: &Path, executable: &Path) {
    let owner = tempfile::tempdir().unwrap();
    let store = EnvironmentStore::open(
        &owner.path().canonicalize().unwrap().join("snapshots"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let snapshot = snapshot_runtime(
        source,
        executable,
        store.allocate().unwrap(),
        RuntimeSnapshotLimits::default(),
    )
    .unwrap();
    let output = std::process::Command::new(snapshot.entrypoint())
        .args(["-B", "-S", "-c", "import json,os,sys;print(json.dumps({'executable':sys.executable,'stdlib':os.__file__,'version':sys.version}))"])
        .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", owner.path()).env("LC_ALL", "C.UTF-8")
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let facts: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(Path::new(facts["executable"].as_str().unwrap()).starts_with(snapshot.root()));
    assert!(Path::new(facts["stdlib"].as_str().unwrap()).starts_with(snapshot.root()));
}

#[test]
#[ignore = "explicit real CLT complete runtime copy and startup"]
fn real_clt_runtime_snapshot() {
    live(
        Path::new(
            "/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9",
        ),
        Path::new("bin/python3.9"),
    );
}

#[test]
#[ignore = "explicit real owned311 complete runtime copy and startup"]
fn real_owned311_runtime_snapshot() {
    live(
        Path::new("/private/tmp/pakro-d3-owned-cpython311-2b8nnsx3"),
        Path::new("bin/python3.11"),
    );
}

#[test]
#[ignore = "explicit real PyPy complete runtime copy and startup"]
fn real_pypy_runtime_snapshot() {
    live(
        Path::new("/private/tmp/pakro-python-live/data/installs/python/pypy3.12-8.0.1"),
        Path::new("bin/pypy3.12"),
    );
}

#[test]
fn readonly_recheck_checks_nonentrypoint_bytes_without_repair() {
    let fixture = Fixture::new();
    let snapshot = fixture.copy().expect("true full snapshot prerequisite");
    snapshot.recheck(PrefixInventoryLimits::default()).unwrap();
    let path = snapshot.root().join("lib/config.txt");
    fs::write(&path, b"changed after snapshot\n").unwrap();
    assert!(snapshot.recheck(PrefixInventoryLimits::default()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"changed after snapshot\n");
    assert_eq!(
        fs::read(fixture.source.join("lib/config.txt")).unwrap(),
        b"config\n"
    );
}

#[test]
fn readonly_recheck_honors_tighter_budgets_without_losing_the_capability() {
    let fixture = Fixture::new();
    let snapshot = fixture.copy().expect("true full snapshot prerequisite");
    assert!(
        snapshot
            .recheck(PrefixInventoryLimits {
                max_files: 0,
                ..Default::default()
            })
            .is_err()
    );
    snapshot.recheck(PrefixInventoryLimits::default()).unwrap();
}
