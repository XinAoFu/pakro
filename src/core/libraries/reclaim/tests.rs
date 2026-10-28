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

const CHILD: &str = "libraries::reclaim::tests::p4_reclaim_crash_child";
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
        let cursor = super::journal::Cursor::default();
        let held = OwnedObject::acquire(&context, store.clone(), record, &cursor).unwrap().unwrap();
        // All measured work is synchronous on this thread, after asynchronous
        // fixture publication. Member/chunk callbacks must not reopen control.
        let before = store_binding_checks();
        held.verify(&context, &cursor).unwrap();
        assert_eq!(store_binding_checks(), before + 2,
            "complete store checks belong at inventory entry/exit, not every member or read block");

        if matches!(id, ReclaimId::Package(_)) {
            let control = store.path.join(".control");
            let displaced = store.path.join(".control-displaced");
            let mut swapped = false;
            let result = held.verify_with_after_walk(&context, &cursor, || {
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
            let result = held.verify_with_after_walk(&context, &cursor, || {
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
            assert!(held.verify(&context, &cursor).is_err(),
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
        let cursor = super::journal::Cursor::default();
        let held = OwnedObject::acquire(&context, store, record, &cursor).unwrap().unwrap();
        held.verify(&context, &cursor).unwrap();
        let entries = held.record.inventory.entries().iter()
            .map(|entry| (entry.relative.as_path(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let relative = Path::new("site-packages/demo.py");
        drop(super::tree::parent(&held, &entries, relative).unwrap());

        let directory = package_path.join("site-packages");
        let original = package_path.join("site-packages-original");
        let mode = std::fs::metadata(&directory).unwrap().permissions();
        std::fs::rename(&directory, &original).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, mode).unwrap();
        let error = super::tree::parent(&held, &entries, relative).unwrap_err();
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

fn crash(stage: &Path, groups: &[Vec<ReclaimId>], cut: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env("PAKRO_RECLAIM_TEST_ROOT", stage)
        .env(
            "PAKRO_RECLAIM_TEST_GROUPS",
            serde_json::to_string(groups).unwrap(),
        )
        .env("PAKRO_TEST_RECLAIM_CUTPOINT", cut)
        .env_remove("PAKRO_TEST_RECLAIM_SYNC_PROOF")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "cut={cut}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn operation(context: &OperationContext) -> uuid::Uuid {
    let pending = pending(context).unwrap();
    assert_eq!(pending.len(), 1);
    pending[0].operation_id
}

#[test]
#[ignore = "invoked only by the owning durable-boundary parent"]
fn p4_reclaim_crash_child() {
    let stage =
        PathBuf::from(std::env::var_os("PAKRO_RECLAIM_TEST_ROOT").expect("owned fixture root"));
    assert_eq!(stage.canonicalize().unwrap(), stage);
    assert_eq!(
        std::fs::read(stage.join(".reclaim-test-owner")).unwrap(),
        b"pakro-owned-reclaim-fixture-v1"
    );
    if let Ok(operation) = std::env::var("PAKRO_RECLAIM_TEST_RESUME") {
        let result = resume(
            &context(&stage),
            uuid::Uuid::parse_str(&operation).unwrap(),
            false,
        )
        .unwrap();
        assert!(
            result.complete && result.resumed,
            "fresh resume process: {result:?}"
        );
        assert!(
            result
                .entries
                .iter()
                .all(|row| row.status == ReclaimStatus::Removed),
            "{result:?}"
        );
        return;
    }
    let groups: Vec<Vec<ReclaimId>> =
        serde_json::from_str(&std::env::var("PAKRO_RECLAIM_TEST_GROUPS").unwrap()).unwrap();
    let result = execute(&context(&stage), ReclaimMode::Prune, &groups, false).unwrap();
    panic!("requested durable cutpoint was not reached: {result:?}");
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
            !result.complete && result.operation_id.is_none(),
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
            "the competing request must not create a second Prepared record"
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
    let operations = pending(&context).unwrap();
    assert_eq!(operations.len(), 1);
    assert!(operations[0].complete);
    assert!(
        resume(&context, operations[0].operation_id, false)
            .unwrap()
            .complete
    );
}

#[tokio::test]
async fn p4_real_exit_at_each_durable_boundary_resumes_only_original_objects() {
    for cut in [
        "preparing",
        "payload-written",
        "cursor-written",
        "state-written",
        "header-prepared",
        "prepared",
        "claim",
        "claims-ready",
        "revoke-intent",
        "ready-removed",
        "revoked",
        "capture-intent",
        "capture-renamed",
        "captured",
        "entry-intent",
        "entry-unlinked",
        "entry-removed",
        "payload-intent",
        "payload-unlinked",
        "allocation-intent",
        "allocation-unlinked",
        "allocation-removed",
        "retired-cleaned",
        "complete",
        "claim-cleaned",
        "completion-receipt",
    ] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, groups, path) = fixture(&stage).await;
        let sentinel = stage.join("unrelated-source");
        std::fs::write(&sentinel, b"never a library candidate").unwrap();
        crash(&stage, &groups, cut);
        let id = operation(&context);
        if ["payload-intent", "payload-unlinked"].contains(&cut) {
            let journals = context
                .requested_data_root()
                .join("projects/.control/journals");
            for index in 0..2 {
                let record: super::object::Record = serde_json::from_slice(
                    &std::fs::read(journals.join(format!("reclaim-object-{id}-{index}.json")))
                        .unwrap(),
                )
                .unwrap();
                let cursor: super::journal::Cursor = serde_json::from_slice(
                    &std::fs::read(journals.join(format!("reclaim-cursor-{id}-{index}.json")))
                        .unwrap(),
                )
                .unwrap();
                cursor
                    .validate(record.inventory.entries().len(), record.payload.is_some())
                    .unwrap();
                assert!(cursor.allocation_removed);
                let library = context
                    .requested_data_root()
                    .join(super::object::store_name(record.object));
                assert!(
                    !library.join(&record.allocation).exists()
                        && !library.join(&record.captured).exists()
                );
            }
            let language: super::object::Record = serde_json::from_slice(
                &std::fs::read(journals.join(format!("reclaim-object-{id}-2.json"))).unwrap(),
            )
            .unwrap();
            let language_cursor: super::journal::Cursor = serde_json::from_slice(
                &std::fs::read(journals.join(format!("reclaim-cursor-{id}-2.json"))).unwrap(),
            )
            .unwrap();
            assert!(
                language_cursor.revoked
                    && language_cursor.payload_pending
                    && !language_cursor.allocation_removed
            );
            let captured = context
                .requested_data_root()
                .join("languages")
                .join(&language.captured);
            assert_eq!(captured.join("payload").exists(), cut == "payload-intent");
            if cut == "payload-intent" {
                // An unknown external referrer must still block the remaining
                // language, even though its old upper group is already gone.
                let unknown = context.requested_data_root().join(format!(
                    "packages/.control/catalog/package-{}.json",
                    hex::encode([222u8; 32])
                ));
                let unknown_bytes = b"{unknown outgoing references";
                std::fs::write(&unknown, unknown_bytes).unwrap();
                std::fs::set_permissions(&unknown, std::fs::Permissions::from_mode(0o600)).unwrap();
                let inode = std::fs::metadata(&captured).unwrap().ino();
                let refused = resume(&context, id, false).unwrap();
                assert!(!refused.complete, "{refused:?}");
                for object in &groups[0] {
                    assert_eq!(
                        refused
                            .entries
                            .iter()
                            .filter(|row| row.object == Some(*object)
                                && row.status == ReclaimStatus::Removed)
                            .count(),
                        1,
                        "{refused:?}"
                    );
                }
                assert!(
                    refused
                        .entries
                        .iter()
                        .any(|row| row.object == Some(language.object)
                            && row.status == ReclaimStatus::Unknown),
                    "{refused:?}"
                );
                assert_eq!(std::fs::metadata(&captured).unwrap().ino(), inode);
                assert_eq!(std::fs::read(&unknown).unwrap(), unknown_bytes);
                assert_eq!(
                    std::fs::read(journals.join(format!("reclaim-cursor-{id}-2.json"))).unwrap(),
                    serde_json::to_vec(&language_cursor).unwrap()
                );
                std::fs::remove_file(unknown).unwrap();
            }
        }
        let result = resume(&context, id, false).unwrap();
        if [
            "preparing",
            "payload-written",
            "cursor-written",
            "state-written",
        ]
        .contains(&cut)
        {
            assert!(
                result.complete
                    && result
                        .entries
                        .iter()
                        .all(|row| row.status == ReclaimStatus::Retained),
                "cut={cut}: {result:?}"
            );
            assert!(path.exists());
            assert_eq!(
                crate::libraries::package::manifest::enumerate_published(&context)
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                resume(&context, id, false)
                    .unwrap()
                    .entries
                    .iter()
                    .all(|row| row.status == ReclaimStatus::Retained)
            );
            continue;
        }
        assert!(result.complete && result.resumed, "cut={cut}: {result:?}");
        let unique = result
            .entries
            .iter()
            .filter_map(|row| row.object)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            result.entries.len(),
            "each original object has one accurate result: {result:?}"
        );
        assert!(
            result
                .entries
                .iter()
                .all(|row| row.status == ReclaimStatus::Removed)
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(sentinel).unwrap(),
            b"never a library candidate"
        );
        assert!(
            crate::libraries::package::manifest::enumerate_published(&context)
                .unwrap()
                .is_empty()
        );
        assert!(
            crate::libraries::language::repository::LanguageRepository::open_readonly(&context)
                .unwrap()
                .enumerate_published()
                .unwrap()
                .is_empty()
        );
        assert!(
            context
                .requested_data_root()
                .join("languages/.leases/build")
                .is_dir()
        );
        assert!(
            context
                .requested_data_root()
                .join("packages/.leases/build")
                .is_dir()
        );
        let again = resume(&context, id, false).unwrap();
        assert!(again.complete);
    }
}

#[tokio::test]
async fn p4_resume_fsyncs_original_parents_after_syscall_before_sync_exit() {
    for (cut, phase, index) in [
        ("ready-unlink-unsynced", "ready-parent", 0),
        ("capture-rename-unsynced", "capture-parent", 0),
        ("entry-unlink-unsynced", "entry-parent", 0),
        ("payload-rmdir-unsynced", "payload-parent", 2),
        ("allocation-rmdir-unsynced", "allocation-parent", 0),
        ("claim-unlink-unsynced", "claim-parent", 0),
    ] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, groups, _) = fixture(&stage).await;
        crash(&stage, &groups, cut);
        let operation = operation(&context);
        let journals = context
            .requested_data_root()
            .join("projects/.control/journals");
        let record: super::object::Record = serde_json::from_slice(
            &std::fs::read(journals.join(format!("reclaim-object-{operation}-{index}.json")))
                .unwrap(),
        )
        .unwrap();
        let cursor: super::journal::Cursor = serde_json::from_slice(
            &std::fs::read(journals.join(format!("reclaim-cursor-{operation}-{index}.json")))
                .unwrap(),
        )
        .unwrap();
        let library = context
            .requested_data_root()
            .join(super::object::store_name(record.object));
        let captured = library.join(&record.captured);
        let metadata_pair = |path: &Path| {
            let value = std::fs::metadata(path).unwrap();
            [value.dev(), value.ino()]
        };
        let expected = match phase {
            "ready-parent" => {
                assert!(cursor.revoke_pending && !cursor.revoked);
                assert!(
                    !library
                        .join(".control/catalog")
                        .join(super::object::manifest_name(record.object))
                        .exists()
                );
                metadata_pair(&library.join(".control/catalog"))
            }
            "capture-parent" => {
                assert!(cursor.capture_pending && !cursor.captured);
                assert!(!library.join(&record.allocation).exists());
                assert_eq!(metadata_pair(&captured), record.allocation_identity);
                record.store
            }
            "entry-parent" => {
                assert!(cursor.pending);
                let mut entries = record.inventory.entries().iter().collect::<Vec<_>>();
                entries.sort_by(|a, b| {
                    b.relative
                        .components()
                        .count()
                        .cmp(&a.relative.components().count())
                        .then_with(|| a.relative.cmp(&b.relative))
                });
                let entry = entries[cursor.next];
                let root = if record.payload.is_some() {
                    captured.join("payload")
                } else {
                    captured.clone()
                };
                assert!(
                    std::fs::symlink_metadata(root.join(&entry.relative))
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                );
                let parent = entry.relative.parent().unwrap();
                if parent.as_os_str().is_empty() {
                    record.payload.unwrap_or(record.allocation_identity)
                } else {
                    record
                        .inventory
                        .entries()
                        .iter()
                        .find(|entry| entry.relative == parent)
                        .unwrap()
                        .identity
                }
            }
            "payload-parent" => {
                assert!(cursor.payload_pending && !cursor.payload_removed);
                assert!(!captured.join("payload").exists());
                record.allocation_identity
            }
            "allocation-parent" => {
                assert!(cursor.allocation_pending && !cursor.allocation_removed);
                assert!(!captured.exists());
                record.store
            }
            "claim-parent" => {
                assert!(
                    !library
                        .join(".control/journals")
                        .join(super::object::claim_name(record.object))
                        .exists()
                );
                let state: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(journals.join(format!("reclaim-state-{operation}.json")))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(state["complete"], true);
                assert_eq!(state["claims_cleaned"], false);
                metadata_pair(&library.join(".control/journals"))
            }
            _ => unreachable!(),
        };
        let progress_parent = metadata_pair(&journals);
        assert!(!stage.join(".reclaim-sync-proof").exists());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("PAKRO_RECLAIM_TEST_ROOT", &stage)
            .env("PAKRO_RECLAIM_TEST_RESUME", operation.to_string())
            .env("PAKRO_TEST_RECLAIM_SYNC_PROOF", "1")
            .env_remove("PAKRO_TEST_RECLAIM_CUTPOINT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cut={cut}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let proof = std::fs::read_to_string(stage.join(".reclaim-sync-proof")).unwrap();
        let events = proof
            .lines()
            .map(|line| {
                serde_json::from_str::<(String, Option<ReclaimId>, [u64; 2])>(line).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            events.iter().any(|(label, object, parent)| label == phase
                && *object == Some(record.object)
                && *parent == expected),
            "cut={cut}: missing successful original-parent fsync proof: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(label, object, parent)| label == "progress-parent"
                    && object.is_none()
                    && *parent == progress_parent),
            "restored progress namespace must also be durable: {events:?}"
        );
        assert!(
            pending(&context)
                .unwrap()
                .iter()
                .all(|operation| operation.complete)
        );
    }
}

#[tokio::test]
async fn p4_resume_refuses_same_inode_changed_bytes_unknown_members_and_bad_cursor() {
    for mode in [0, 1, 2] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let (context, groups, path) = fixture(&stage).await;
        crash(&stage, &groups, "prepared");
        let id = operation(&context);
        let catalog = context
            .requested_data_root()
            .join("projects/.control/catalog/projects");
        let before = std::fs::read(&catalog).unwrap();
        if mode == 0 {
            let file = path.join("site-packages/demo.py");
            let inode = std::fs::metadata(&file).unwrap().ino();
            std::fs::write(&file, b"VALUE=2\n").unwrap();
            assert_eq!(std::fs::metadata(file).unwrap().ino(), inode);
        } else if mode == 1 {
            std::fs::write(path.join("unknown-user-file"), b"keep unknown").unwrap();
        } else {
            let file = context.requested_data_root().join(format!(
                "projects/.control/journals/reclaim-cursor-{id}-0.json"
            ));
            let mut cursor: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            cursor["next"] = serde_json::json!(999999);
            std::fs::write(file, serde_json::to_vec(&cursor).unwrap()).unwrap();
        }
        let result = resume(&context, id, false).unwrap();
        assert!(!result.complete, "{result:?}");
        assert!(
            result
                .entries
                .iter()
                .any(|row| row.status == ReclaimStatus::Damaged
                    && row.reasons.iter().any(|reason| reason
                        .contains("reclaim remaining inventory changed or has unknown members")
                        || reason.contains("invalid reclaim entry cursor")
                        || reason.contains("reclaim entry progress precedes capture"))),
            "{result:?}"
        );
        assert_eq!(std::fs::read(catalog).unwrap(), before);
        assert!(path.exists());
        if mode == 1 {
            assert_eq!(
                std::fs::read(path.join("unknown-user-file")).unwrap(),
                b"keep unknown"
            );
        }
    }
}

#[tokio::test]
async fn p4_resume_does_not_initialize_missing_or_replaced_object_libraries() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, _) = fixture(&stage).await;
    crash(&stage, &groups, "prepared");
    let id = operation(&context);
    let root = context.requested_data_root().join("packages");
    let saved = stage.join("saved-packages");
    std::fs::rename(&root, &saved).unwrap();
    let result = resume(&context, id, false).unwrap();
    assert!(!result.complete);
    assert!(
        result
            .entries
            .iter()
            .any(|row| row.status == ReclaimStatus::Unknown),
        "{result:?}"
    );
    assert!(
        !root.exists(),
        "resume must not recreate a missing object library"
    );
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("foreign"), b"unrelated library root").unwrap();
    let result = resume(&context, id, false).unwrap();
    assert!(!result.complete);
    assert!(
        result
            .entries
            .iter()
            .any(|row| row.status == ReclaimStatus::Unknown),
        "{result:?}"
    );
    assert_eq!(
        std::fs::read(root.join("foreign")).unwrap(),
        b"unrelated library root"
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert!(saved.join(".control/catalog").exists());
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
async fn p4_claim_blocks_republication_and_completed_resume_preserves_a_new_instance() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (context, groups, _) = fixture(&stage).await;
    crash(&stage, &groups, "revoked");
    let id = operation(&context);
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let source = wheel(
        &context,
        &language,
        "demo",
        [171; 32],
        vec![],
        vec![("demo.py", b"VALUE=1\n")],
        None,
    );
    let error = match prepare_layout(&context, &language, [172; 32], vec![source]) {
        Ok(_) => panic!("a pending layout identity must not be republished"),
        Err(error) => error,
    };
    assert!(
        error.chain().any(|cause| cause.to_string()
            == "PendingRecovery: shared object has an unfinished deletion"),
        "{error:#}"
    );
    drop(language);
    assert!(resume(&context, id, false).unwrap().complete);
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let language_root = language.root().to_owned();
    let original = std::fs::metadata(&language_root).unwrap().ino();
    let result = resume(&context, id, false).unwrap();
    assert!(result.complete);
    language.check_binding().unwrap();
    assert_eq!(std::fs::metadata(language_root).unwrap().ino(), original);
}
