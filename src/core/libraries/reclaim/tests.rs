use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::layouts::{prepare_layout, tests::wheel};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

const RACE_CHILD: &str = "libraries::reclaim::tests::p4_reclaim_competing_child";
fn context(stage: &Path) -> OperationContext {
    operation_context(stage, true, Duration::from_secs(30))
}
async fn fixture(stage: &Path) -> (OperationContext, Vec<Vec<ReclaimId>>, PathBuf) {
    std::fs::write(
        stage.join(".reclaim-test-owner"),
        b"pakro-owned-reclaim-fixture-v1",
    )
    .unwrap();
    let context = context(stage);
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let language_id = ReclaimId::Language(language.key().fingerprint());
    let source = wheel(
        &context,
        &language,
        "demo",
        [171; 32],
        vec![],
        vec![("demo.py", b"VALUE=1\n")],
        None,
    );
    let layout = prepare_layout(&context, &language, [172; 32], vec![source]).unwrap();
    let path = layout.packages()[0].root().to_owned();
    let groups = vec![
        vec![
            ReclaimId::Layout(layout.key()),
            ReclaimId::Package([171; 32]),
        ],
        vec![language_id],
    ];
    drop(layout);
    drop(language);
    (context, groups, path)
}

#[tokio::test(flavor = "current_thread")]
async fn p6_reclaim_inventory_reuses_held_fds_and_keeps_full_boundary_checks() {
    use super::object::{OwnedObject, Store, describe, manifest_name, store_binding_checks, store_name};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, package_path) = fixture(&stage).await;
    for id in [groups[0][1], groups[1][0]] {
        let store = Store::open(&context, store_name(id), false).unwrap().unwrap();
        let record = describe(&context, &store, id).unwrap();
        assert!(record.inventory.entries().len() > 2);
        let held = OwnedObject::acquire(&context, store.clone(), record).unwrap().unwrap();
        // All measured work is synchronous on this thread, after asynchronous
        // fixture publication. Member/chunk callbacks must not reopen control.
        let before = store_binding_checks();
        held.verify(&context).unwrap();
        assert_eq!(store_binding_checks(), before + 2,
            "complete store checks belong at inventory entry/exit, not every member or read block");

        if matches!(id, ReclaimId::Package(_)) {
            let control = store.path.join(".control");
            let displaced = store.path.join(".control-displaced");
            let mut swapped = false;
            let result = held.verify_with_after_walk(&context, || {
                std::fs::rename(&control, &displaced)?;
                std::fs::create_dir(&control)?;
                std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700))?;
                swapped = true;
                Ok(())
            });
            assert!(swapped);
            std::fs::remove_dir(&control).unwrap();
            std::fs::rename(&displaced, &control).unwrap();
            assert!(result.is_err(), "the full exit gate must reject a replaced control namespace");

            let manifest = control.join("catalog").join(manifest_name(id));
            let original = std::fs::read(&manifest).unwrap();
            let inode = std::fs::metadata(&manifest).unwrap().ino();
            let mut changed = original.clone(); changed.push(b' ');
            let result = held.verify_with_after_walk(&context, || {
                std::fs::write(&manifest, &changed)?;
                Ok(())
            });
            assert_eq!(std::fs::metadata(&manifest).unwrap().ino(), inode);
            std::fs::write(&manifest, original).unwrap();
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("reclaim original manifest changed"));

            let member = package_path.join("site-packages/demo.py");
            let inode = std::fs::metadata(&member).unwrap().ino();
            std::fs::write(&member, b"VALUE=2\n").unwrap();
            assert_eq!(std::fs::metadata(&member).unwrap().ino(), inode);
            assert!(held.verify(&context).is_err(),
                "held-FD reuse must still hash and reject same-inode changed payload bytes");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn p6_reclaim_parent_index_is_local_and_keeps_actual_directory_checks() {
    use super::object::{OwnedObject, Store, describe, store_name};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, package_path) = fixture(&stage).await;
    {
        let id = groups[0][1];
        let store = Store::open(&context, store_name(id), false).unwrap().unwrap();
        let record = describe(&context, &store, id).unwrap();
        let held = OwnedObject::acquire(&context, store, record).unwrap().unwrap();
        held.verify(&context).unwrap();
        let entries = held.record.inventory.entries().iter()
            .map(|entry| (entry.relative.as_path(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let relative = Path::new("site-packages/demo.py");
        let mut parents = super::tree::ParentCache::new(&held).unwrap();
        parents.parent(&entries, relative).unwrap();
        let opens = parents.opens;
        assert!(opens > 0);
        parents.parent(&entries, relative).unwrap();
        assert_eq!(parents.opens, opens, "the same parent should reuse its open descriptor");

        let directory = package_path.join("site-packages");
        let original = package_path.join("site-packages-original");
        let mode = std::fs::metadata(&directory).unwrap().permissions();
        std::fs::rename(&directory, &original).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, mode).unwrap();
        let error = parents.parent(&entries, relative).unwrap_err();
        std::fs::remove_dir(&directory).unwrap();
        std::fs::rename(&original, &directory).unwrap();
        assert!(format!("{error:#}").contains("reclaim parent directory was replaced"),
            "a local metadata lookup must not authorize a substituted directory: {error:#}");
    }

    // The deletion path is synchronous and stays on this thread after the
    // asynchronous fixture setup, so this measures every original object.
    let before = super::tree::parent_index_builds();
    let result = execute(&context, ReclaimMode::Prune, &groups, false).unwrap();
    assert!(result.complete, "{result:?}");
    assert_eq!(super::tree::parent_index_builds() - before,
        groups.iter().map(Vec::len).sum::<usize>(),
        "each original object must build its parent lookup once, not once per member");
    assert!(!package_path.exists());
}

#[test]
#[ignore = "invoked only by the owning two-process maintenance parent"]
fn p4_reclaim_competing_child() {
    let stage =
        PathBuf::from(std::env::var_os("PAKRO_RECLAIM_TEST_ROOT").expect("owned fixture root"));
    assert_eq!(stage.canonicalize().unwrap(), stage);
    assert_eq!(
        std::fs::read(stage.join(".reclaim-test-owner")).unwrap(),
        b"pakro-owned-reclaim-fixture-v1"
    );
    let groups: Vec<Vec<ReclaimId>> =
        serde_json::from_str(&std::env::var("PAKRO_RECLAIM_TEST_GROUPS").unwrap()).unwrap();
    let result = execute(&context(&stage), ReclaimMode::Prune, &groups, false).unwrap();
    if std::env::var("PAKRO_RECLAIM_TEST_ROLE").unwrap() == "first" {
        assert!(result.complete, "{result:?}");
        assert!(
            result
                .entries
                .iter()
                .all(|row| row.status == ReclaimStatus::Removed)
        );
    } else {
        assert!(
            !result.complete,
            "{result:?}"
        );
        assert!(
            result
                .entries
                .iter()
                .all(|row| row.status == ReclaimStatus::Busy),
            "{result:?}"
        );
        assert!(
            pending(&context(&stage)).unwrap().is_empty(),
            "the competing request must not create deletion markers"
        );
    }
}

#[tokio::test]
async fn p4_two_independent_reclaims_do_not_create_mutually_protecting_operations() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, path) = fixture(&stage).await;
    drop(
        crate::libraries::project::repository::ProjectRepository::open_reclaim_journal_writer(
            &context,
        )
        .unwrap(),
    );
    let command = |role: &str| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", RACE_CHILD, "--ignored", "--nocapture"])
            .env("PAKRO_RECLAIM_TEST_ROOT", &stage)
            .env(
                "PAKRO_RECLAIM_TEST_GROUPS",
                serde_json::to_string(&groups).unwrap(),
            )
            .env("PAKRO_RECLAIM_TEST_ROLE", role);
        command
    };
    let mut first = command("first")
        .env("PAKRO_TEST_RECLAIM_HOLD_GATE", &stage)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !stage.join("coordinator-held").exists() {
        if let Some(status) = first.try_wait().unwrap() {
            panic!("first collector exited before coordination: {status}")
        }
        if std::time::Instant::now() >= deadline {
            first.kill().unwrap();
            first.wait().unwrap();
            panic!("first collector did not reach its owned coordination barrier")
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let second = command("second").output().unwrap();
    std::fs::write(
        stage.join("coordinator-release"),
        b"continue original collector",
    )
    .unwrap();
    let first = first.wait_with_output().unwrap();
    assert!(
        second.status.success(),
        "second:\n{}\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        first.status.success(),
        "first:\n{}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!path.exists());
    assert!(pending(&context).unwrap().is_empty());
}

#[tokio::test]
async fn p4_readonly_store_upgrade_preserves_the_original_physical_library() {
    for replacement in 0..3 {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, _, _) = fixture(&stage).await;
        let id = ReclaimId::Package([171; 32]);
        let mut stores = super::object::Stores::new();
        let original = super::object::store(&context, &mut stores, id, false).unwrap();
        assert!(original.writer().is_err());
        let root_identity = super::object::pair(&original.root).unwrap();
        let build_identity = super::object::pair(&original.build).unwrap();
        if replacement == 0 {
            let upgraded = super::object::store(&context, &mut stores, id, true).unwrap();
            assert!(!upgraded.writer().unwrap().initialized_here());
            assert_eq!(super::object::pair(&upgraded.root).unwrap(), root_identity);
            assert_eq!(
                super::object::pair(&upgraded.build).unwrap(),
                build_identity
            );
            original.check().unwrap();
            upgraded.check().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                &upgraded,
                &super::object::store(&context, &mut stores, id, false).unwrap()
            ));
            continue;
        }
        let root = context.requested_data_root().join("packages");
        std::fs::rename(&root, stage.join("original-packages")).unwrap();
        if replacement == 1 {
            let error = match super::object::store(&context, &mut stores, id, true) {
                Ok(_) => panic!("a missing original library cannot acquire a writer"),
                Err(error) => error,
            };
            assert_eq!(
                super::object::failure_status(&error),
                ReclaimStatus::Unknown
            );
            assert!(!root.exists());
        } else {
            let foreign = pakro_store::control::StoreWriter::open_writer(
                &root,
                super::object::uid(),
                Default::default(),
                context.policy().deadline,
            )
            .unwrap();
            std::fs::write(
                root.join("foreign-sentinel"),
                b"keep this separate physical library",
            )
            .unwrap();
            let marker = std::fs::read(root.join(".control/.identity")).unwrap();
            let foreign_identity = super::object::pair(foreign.root_handle()).unwrap();
            let error = match super::object::store(&context, &mut stores, id, true) {
                Ok(_) => {
                    panic!("a replacement library cannot upgrade the original read capability")
                }
                Err(error) => error,
            };
            assert_eq!(
                super::object::failure_status(&error),
                ReclaimStatus::Unknown
            );
            assert_eq!(
                super::object::pair(foreign.root_handle()).unwrap(),
                foreign_identity
            );
            assert_eq!(
                std::fs::read(root.join(".control/.identity")).unwrap(),
                marker
            );
            assert_eq!(
                std::fs::read(root.join("foreign-sentinel")).unwrap(),
                b"keep this separate physical library"
            );
            assert_eq!(
                std::fs::read_dir(root.join(".leases/build"))
                    .unwrap()
                    .count(),
                0
            );
        }
        assert!(std::sync::Arc::ptr_eq(
            &original,
            stores.get("packages").unwrap()
        ));
        assert!(original.writer().is_err());
    }
}

#[tokio::test]
async fn p4_final_payload_removal_rejects_a_replacement_directory() {
    use super::object::{OwnedObject, Store, describe, store_name};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, _) = fixture(&stage).await;
    let id = groups[1][0];
    let store = Store::open(&context, store_name(id), true).unwrap().unwrap();
    let record = describe(&context, &store, id).unwrap();
    let mut held = OwnedObject::acquire(&context, store, record).unwrap().unwrap();
    held.verify(&context).unwrap();
    // Isolate the final payload removal after the member loop has finished.
    // The published manifest remains untouched; this is only the local test input.
    let mut inventory = serde_json::to_value(&held.record.inventory).unwrap();
    inventory["entries"] = serde_json::json!([]);
    held.record.inventory = serde_json::from_value(inventory).unwrap();
    let payload = held.record.root_path(&held.store);
    let original = stage.join("original-payload");
    std::fs::rename(&payload, &original).unwrap();
    std::fs::create_dir(&payload).unwrap();
    let replacement = std::fs::metadata(&payload).unwrap().ino();
    let error = super::tree::remove(&held, &context).unwrap_err();
    assert!(format!("{error:#}").contains(payload.to_str().unwrap()), "{error:#}");
    assert_eq!(std::fs::metadata(&payload).unwrap().ino(), replacement);
    assert!(original.join("bin/python").is_file());
}

#[tokio::test]
async fn p4_unrecorded_fifo_and_socket_are_preserved_and_reported_by_path() {
    for socket in [false, true] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, groups, package) = fixture(&stage).await;
        let unexpected = package.join(if socket { "unexpected.sock" } else { "unexpected.fifo" });
        let listener = if socket {
            let address = stage.join("socket");
            let listener = std::os::unix::net::UnixListener::bind(&address).unwrap();
            std::fs::rename(address, &unexpected).unwrap();
            Some(listener)
        } else {
            nix::unistd::mkfifo(&unexpected, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
            None
        };
        let identity = std::fs::symlink_metadata(&unexpected).unwrap().ino();
        let report = execute(&context, ReclaimMode::Prune, &groups, false).unwrap();
        assert!(!report.complete, "{report:?}");
        assert!(report.entries.iter().flat_map(|entry| &entry.reasons)
            .any(|reason| reason.contains("unsupported type") && reason.contains(unexpected.to_str().unwrap())), "{report:?}");
        assert_eq!(std::fs::symlink_metadata(&unexpected).unwrap().ino(), identity);
        assert_eq!(std::fs::read(package.join("site-packages/demo.py")).unwrap(), b"VALUE=1\n");
        assert!(pending(&context).unwrap().is_empty());
        drop(listener);
    }
}

#[tokio::test]
async fn p4_unknown_files_and_replaced_paths_stop_deletion_without_touching_outside_files() {
    for case in ["unknown", "root-symlink", "root-directory", "directory-symlink", "directory", "file-symlink"] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, groups, package) = fixture(&stage).await;
        let outside = stage.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("keep.py");
        std::fs::write(&outside_file, b"outside contents").unwrap();
        let original = stage.join("original-content");
        let changed = match case {
            "unknown" => {
                let path = package.join("unrecorded.txt");
                std::fs::write(&path, b"unrecorded contents").unwrap();
                path
            }
            "root-symlink" | "root-directory" => {
                std::fs::rename(&package, &original).unwrap();
                if case == "root-symlink" {
                    std::os::unix::fs::symlink(&outside, &package).unwrap();
                } else {
                    std::fs::create_dir(&package).unwrap();
                    std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o700)).unwrap();
                }
                package.clone()
            }
            "directory-symlink" | "directory" => {
                let path = package.join("site-packages");
                std::fs::rename(&path, &original).unwrap();
                if case == "directory-symlink" {
                    std::os::unix::fs::symlink(&outside, &path).unwrap();
                } else {
                    std::fs::create_dir(&path).unwrap();
                }
                path
            }
            "file-symlink" => {
                let path = package.join("site-packages/demo.py");
                std::fs::rename(&path, &original).unwrap();
                std::os::unix::fs::symlink(&outside_file, &path).unwrap();
                path
            }
            _ => unreachable!(),
        };
        let identity = std::fs::symlink_metadata(&changed).unwrap().ino();
        let report = execute(&context, ReclaimMode::Prune, &groups, false).unwrap();
        assert!(!report.complete, "{case}: {report:?}");
        assert!(report.entries.iter().flat_map(|entry| &entry.reasons)
            .any(|reason| reason.contains(changed.to_str().unwrap())), "{case}: {report:?}");
        assert_eq!(std::fs::symlink_metadata(&changed).unwrap().ino(), identity);
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside contents");
        if case == "unknown" { assert_eq!(std::fs::read(&changed).unwrap(), b"unrecorded contents"); }
        if case != "unknown" { assert!(original.exists()); }
        assert!(pending(&context).unwrap().is_empty());
    }
}

#[test]
#[ignore = "invoked only by the owning concurrent installation test"]
fn p4_installation_holds_shared_objects_child() {
    let stage = PathBuf::from(std::env::var_os("PAKRO_RECLAIM_TEST_ROOT").unwrap());
    assert_eq!(std::fs::read(stage.join(".reclaim-test-owner")).unwrap(), b"pakro-owned-reclaim-fixture-v1");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (context, _, path) = fixture(&stage).await;
        let bytes = archive("3.15.0");
        let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
        let source = wheel(&context, &language, "demo", [171; 32], vec![], vec![("demo.py", b"VALUE=1\n")], None);
        let layout = prepare_layout(&context, &language, [172; 32], vec![source]).unwrap();
        std::fs::write(stage.join("installation-held"), b"held").unwrap();
        while !stage.join("installation-release").exists() {
            context.checkpoint().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(path.is_dir());
        language.check_binding().unwrap();
        for package in layout.packages() { package.check_binding().unwrap(); }
    });
}

#[tokio::test]
async fn p4_prune_during_installation_keeps_shared_objects_then_reclaims_them() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, path) = fixture(&stage).await;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "libraries::reclaim::tests::p4_installation_holds_shared_objects_child", "--ignored", "--nocapture"])
        .env("PAKRO_RECLAIM_TEST_ROOT", &stage)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !stage.join("installation-held").exists() {
        if let Some(status) = child.try_wait().unwrap() { panic!("installation exited before holding its objects: {status}"); }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap(); child.wait().unwrap(); panic!("installation did not acquire shared objects");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = execute(&context, ReclaimMode::Prune, &groups, false);
    std::fs::write(stage.join("installation-release"), b"release").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let report = result.unwrap();
    assert!(!report.complete && report.entries.iter().all(|entry| entry.status == ReclaimStatus::Busy), "{report:?}");
    assert!(path.is_dir());
    assert!(pending(&context).unwrap().is_empty());
    assert!(execute(&context, ReclaimMode::Prune, &groups, false).unwrap().complete);
    assert!(!path.exists());
}


pub(super) fn mark_event(
    context: &OperationContext,
    owner: &super::object::OwnedObject,
    phase: &str,
) -> eyre::Result<()> {
    use std::io::Write;
    let Ok(mode) = std::env::var("PAKRO_TEST_RECLAIM_MARK_PHASES") else { return Ok(()) };
    let stage = context.requested_data_root().parent().unwrap();
    eyre::ensure!(std::fs::read(stage.join(".reclaim-test-owner"))? == b"pakro-owned-reclaim-fixture-v1", "mark observation requires an owned test root");
    eyre::ensure!(matches!(mode.as_str(), "pause-before-sync" | "observe"), "unknown mark observation mode");
    let first = owner.record.inventory.entries().first().expect("nonempty test object");
    let present = std::fs::symlink_metadata(owner.record.root_path(&owner.store).join(&first.relative)).is_ok();
    let mut output = std::fs::OpenOptions::new().append(true).create(true).open(stage.join("mark-events.jsonl"))?;
    writeln!(output, "{}", serde_json::json!({"object":owner.record.object.label(),"phase":phase,"first_member_present":present}))?;
    if phase == "renamed" && mode == "pause-before-sync" {
        std::fs::write(stage.join("mark-paused"), owner.record.object.label())?;
        loop {
            context.checkpoint()?;
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

#[test]
#[ignore = "invoked only by the owning rename-before-sync interruption test"]
fn p4_mark_sync_child() {
    let stage = PathBuf::from(std::env::var_os("PAKRO_RECLAIM_TEST_ROOT").unwrap());
    assert_eq!(std::fs::read(stage.join(".reclaim-test-owner")).unwrap(), b"pakro-owned-reclaim-fixture-v1");
    let report = crate::programs::uninstall::prune::run(&context(&stage), false).unwrap();
    assert!(report.complete, "{report:?}");
}

#[tokio::test]
async fn p4_interruption_after_mark_rename_syncs_existing_mark_before_deleting() {
    use std::os::unix::process::ExitStatusExt;
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, package) = fixture(&stage).await;
    let command = |mode: &str| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "libraries::reclaim::tests::p4_mark_sync_child", "--ignored", "--nocapture"])
            .env("PAKRO_RECLAIM_TEST_ROOT", &stage).env("PAKRO_TEST_RECLAIM_MARK_PHASES", mode)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        command
    };
    let mut child = ChildGuard(command("pause-before-sync").spawn().unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !stage.join("mark-paused").exists() {
        assert!(child.0.try_wait().unwrap().is_none(), "mark child exited before rename");
        assert!(std::time::Instant::now() < deadline, "mark child did not reach rename");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.0.kill().unwrap();
    assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    let marked = std::fs::read_to_string(stage.join("mark-paused")).unwrap();
    assert_eq!(marked, groups[0][0].label());
    let interrupted = pending(&context).unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].objects, vec![groups[0][0]]);
    assert!(package.join("site-packages/demo.py").is_file());
    let before = std::fs::read_to_string(stage.join("mark-events.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(before.trim()).unwrap();
    assert_eq!(first["phase"], "renamed");
    assert_eq!(first["first_member_present"], true);
    let output = command("observe").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let events = std::fs::read_to_string(stage.join("mark-events.jsonl")).unwrap();
    let events = events.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    let synced = events.iter().filter(|event| event["object"] == marked && event["phase"] == "synced").collect::<Vec<_>>();
    assert_eq!(synced.len(), 1, "the existing mark must reach both directory syncs");
    assert_eq!(synced[0]["first_member_present"], true, "sync must complete before deleting the original members");
    assert!(pending(&context).unwrap().is_empty());
    assert!(!package.exists());
    eprintln!("P4_MARK_RENAME_INTERRUPTED existing_mark_synced_before_members=true events={events:?}");
}
