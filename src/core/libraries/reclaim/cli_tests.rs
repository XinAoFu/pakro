//! One real-main lifecycle over a small, owned, fully published fixture.
use uuid::Uuid;
use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::layouts::{prepare_layout, tests::wheel};
use crate::libraries::project::{
    identity::ProjectIdentity, repository::ProjectRepository, state::ExpectedProjectState,
};
use crate::packages::python_environment::model::{
    GenerationRecord, InputKind, InputReference, SelectionKey,
};
use pakro_store::control::lease::StableLock;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    identity: (u64, u64),
    mode: u32,
    len: u64,
    modified: (i64, i64),
    link: Option<PathBuf>,
    sha256: Option<[u8; 32]>,
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let kind = metadata.file_type();
        let entry = Entry {
            identity: (metadata.dev(), metadata.ino()),
            mode: metadata.mode(),
            len: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            link: kind.is_symlink().then(|| std::fs::read_link(path).unwrap()),
            sha256: kind
                .is_file()
                .then(|| Sha256::digest(std::fs::read(path).unwrap()).into()),
        };
        entries.insert(path.strip_prefix(root).unwrap().to_owned(), entry);
        if kind.is_dir() {
            for child in std::fs::read_dir(path).unwrap() {
                visit(root, &child.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn cli_command(binary: &Path, stage: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command.env_clear().current_dir(stage).args(args);
    for (name, value) in [
        ("HOME", stage.join("home")),
        ("PAKRO_DATA_DIR", stage.join("data")),
        ("PAKRO_CACHE_DIR", stage.join("cache")),
        ("PAKRO_CONFIG_DIR", stage.join("config")),
        ("PAKRO_STATE_DIR", stage.join("state")),
        ("PAKRO_GLOBAL_CONFIG_FILE", stage.join("absent-global.toml")),
        ("PAKRO_SYSTEM_CONFIG_FILE", stage.join("absent-system.toml")),
        ("PAKRO_SYSTEM_CONFIG_DIR", stage.join("absent-system")),
        ("PAKRO_CEILING_PATHS", stage.to_owned()),
        ("XDG_CONFIG_HOME", stage.join("config")),
        ("XDG_CACHE_HOME", stage.join("cache")),
        ("XDG_DATA_HOME", stage.join("xdg-data")),
        ("XDG_STATE_HOME", stage.join("state")),
    ] {
        command.env(name, value);
    }
    for (name, value) in [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("LC_ALL", "C"),
        ("PAKRO_AUTO_UPDATE", "0"),
        ("PAKRO_LOCKED", "0"),
        ("PAKRO_OVERRIDE_CONFIG_FILENAMES", "pakro.toml"),
        (
            "PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES",
            "absent-tool-versions",
        ),
    ] {
        command.env(name, value);
    }
    command
}

fn cli(binary: &Path, stage: &Path, args: &[&str], code: i32) -> Output {
    let output = cli_command(binary, stage, args).output().unwrap();
    println!(
        "P4_MAIN_COMMAND={args:?} EXIT={:?}\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(code),
        "real main failed for {args:?}: {output:?}"
    );
    output
}

fn catalog(context: &OperationContext) -> Vec<u8> {
    ProjectRepository::open_readonly(context)
        .unwrap()
        .model()
        .encode_catalog(Default::default())
        .unwrap()
}

fn statuses(output: &Output, status: &str, objects: &[ReclaimId]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    for id in objects {
        let canonical = format!("{status}: {}", id.label());
        assert!(
            stdout.lines().any(|line| line == canonical
                || (matches!(id, ReclaimId::Package(_))
                    && line == format!("{canonical} (demo==1.0)"))),
            "missing {status} for {}: {stdout}",
            id.label()
        );
    }
}

fn ready(context: &OperationContext, objects: &[ReclaimId], expected: bool) {
    let mut stores = object::Stores::new();
    for &id in objects {
        let store = object::store(context, &mut stores, id, false).unwrap();
        assert_eq!(
            store
                .snapshot(
                    pakro_store::control::ControlNamespace::Catalog,
                    &object::manifest_name(id)
                )
                .unwrap()
                .is_some(),
            expected,
            "{} Ready",
            id.label()
        );
    }
}

#[tokio::test]
async fn p8_exact_instance_preview_is_readonly_and_removes_an_unreferenced_package() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(120));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
    let (facts, archive) = wheel(
        &context, &language, "unused", [211; 32], vec![],
        vec![("unused.py", b"VALUE=1\n")], None,
    );
    let package = crate::libraries::package::tree::prepare_package_tree(
        &context, &language, facts, archive,
    ).unwrap();
    let id = ReclaimId::Package(package.manifest().key());
    let root = package.root().to_owned();
    drop((package, language));

    let before = tree(context.requested_data_root());
    let preview = crate::programs::uninstall::reclaim::instance_report(&context, &id.label(), true).unwrap();
    assert!(preview.complete);
    assert!(preview.entries.iter().any(|entry| entry.object == Some(id) && entry.status == ReclaimStatus::Eligible));
    assert_eq!(tree(context.requested_data_root()), before, "instance preview must not change library data");

    let removed = crate::programs::uninstall::reclaim::instance_report(&context, &id.label(), false).unwrap();
    assert!(removed.complete);
    assert!(removed.entries.iter().any(|entry| entry.object == Some(id) && entry.status == ReclaimStatus::Removed));
    assert!(!root.exists());
    assert!(crate::programs::uninstall::reclaim::instance_report(&context, &id.label(), true).is_err());
}

fn manifests(
    context: &OperationContext,
    objects: &[ReclaimId],
) -> BTreeMap<ReclaimId, ([u64; 2], Vec<u8>)> {
    let mut stores = object::Stores::new();
    objects
        .iter()
        .map(|&id| {
            let store = object::store(context, &mut stores, id, false).unwrap();
            let pin = store
                .snapshot(
                    pakro_store::control::ControlNamespace::Catalog,
                    &object::manifest_name(id),
                )
                .unwrap()
                .unwrap();
            (id, (object::pair_snapshot(&pin), pin.bytes().to_vec()))
        })
        .collect()
}

fn selections(
    context: &OperationContext,
    keys: &[SelectionKey; 2],
    active: [bool; 2],
    allocation: &str,
    original_transactions: &[Uuid; 2],
) {
    let repository = ProjectRepository::open_readonly(context).unwrap();
    for (index, key) in keys.iter().enumerate() {
        let stamp = repository.model().snapshot(key);
        assert_eq!(stamp.revision(), if active[index] { 1 } else { 2 });
        assert_eq!(stamp.generation(), active[index].then_some(allocation));
        assert_eq!(
            repository
                .model()
                .project_registration(key.project())
                .is_some(),
            active[index]
        );
        assert!(
            repository
                .model()
                .applied_transaction(original_transactions[index])
                .is_some()
        );
    }
}

#[tokio::test]
#[ignore = "requires PAKRO_TEST_MAIN_BINARY pointing to the freshly built real pakro executable"]
async fn p4_real_main_prune_respects_two_projects_and_repeated_prune() {
    let binary = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MAIN_BINARY")
            .expect("set PAKRO_TEST_MAIN_BINARY to the freshly built real pakro executable"),
    );
    assert!(binary.is_absolute());
    let metadata = std::fs::symlink_metadata(&binary).unwrap();
    assert!(
        metadata.file_type().is_file() && metadata.mode() & 0o111 != 0,
        "PAKRO_TEST_MAIN_BINARY must be a regular executable"
    );
    let binary = binary.canonicalize().unwrap();
    let binary_sha: [u8; 32] = Sha256::digest(std::fs::read(&binary).unwrap()).into();
    println!(
        "P4_REAL_MAIN={} SHA256={}",
        binary.display(),
        hex::encode(binary_sha)
    );

    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    std::fs::create_dir(stage.join("home")).unwrap();
    std::fs::write(
        stage.join(".reclaim-test-owner"),
        b"pakro-owned-main-reclaim-fixture-v1",
    )
    .unwrap();
    let initial = operation_context(&stage, true, Duration::from_secs(30));
    let mut policy = *initial.policy();
    policy.writes.project_selection = true;
    let context =
        crate::programs::capture_operation_context(crate::programs::OperationContextInputs {
            settings: initial.settings(),
            policy,
            cancellation: tokio_util::sync::CancellationToken::new(),
            data_root: initial.requested_data_root().to_owned(),
            cache_root: initial.requested_cache_root().to_owned(),
            python_sources: None,
            python_target_description: None,
        })
        .unwrap();
    let archive = archive("3.15.0");
    let language = prepare(&install_spec(&archive), &context, &archive)
        .await
        .unwrap();
    let source = wheel(
        &context,
        &language,
        "demo",
        [181; 32],
        vec![],
        vec![("demo.py", b"VALUE=1\n")],
        None,
    );
    let layout = prepare_layout(&context, &language, [182; 32], vec![source]).unwrap();
    let objects = [
        ReclaimId::Language(language.key().fingerprint()),
        ReclaimId::Package([181; 32]),
        ReclaimId::Layout(layout.key()),
    ];
    let payloads = [
        language.root().to_owned(),
        layout.packages()[0].root().to_owned(),
        layout.prefix().to_owned(),
    ];
    let allocation = layout
        .prefix()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let projects = [Uuid::from_u128(901), Uuid::from_u128(902)];
    let project_paths = [stage.join("project-a"), stage.join("project-b")];
    for project in &project_paths {
        std::fs::create_dir(project).unwrap();
        std::fs::write(
            project.join("pakro.toml"),
            b"[tools]\npython = \"3.15.0\"\n",
        )
        .unwrap();
        std::fs::write(
            project.join("user-content"),
            b"project files must survive forget and reclaim\n",
        )
        .unwrap();
    }
    let project_files = [tree(&project_paths[0]), tree(&project_paths[1])];
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let inputs = vec![
        InputReference::new(
            InputKind::Language,
            pakro_store::Digest::Sha256(layout.language()),
        ),
        InputReference::new(InputKind::Package, pakro_store::Digest::Sha256([181; 32])),
    ];
    let generation = GenerationRecord::new_shared(
        &allocation,
        language.key().target(),
        layout.graph(),
        [183; 32],
        layout.key(),
        inputs,
        repository.model().limits(),
    )
    .unwrap();
    repository
        .model_mut()
        .register_shared_generation(generation)
        .unwrap();
    let keys = projects.map(|project| {
        SelectionKey::new(
            project,
            "default",
            language.key().target(),
            repository.model().limits(),
        )
        .unwrap()
    });
    let transactions = [Uuid::now_v7(), Uuid::now_v7()];
    for index in 0..2 {
        drop(
            StableLock::create_new(
                writer.build_handle(),
                &format!("project-{}.lock", projects[index]),
                object::uid(),
            )
            .unwrap(),
        );
        repository
            .model_mut()
            .register_project(
                projects[index],
                project_paths[index].to_str().unwrap(),
                &"c".repeat(64),
            )
            .unwrap();
        let expected = repository.model().snapshot(&keys[index]);
        repository
            .model_mut()
            .apply_selection(
                &keys[index],
                &expected,
                Some(&allocation),
                transactions[index],
            )
            .unwrap();
    }
    repository.commit_model().unwrap();
    let b_before = ExpectedProjectState::from_model(
        repository.model(),
        ProjectIdentity::from_uuid(projects[1]),
    );
    drop((writer, repository, layout, language));
    // The actual main must encounter catalog roots, rather than test-owned SH
    // leases which could make every candidate merely busy.
    ready(&context, &objects, true);
    selections(&context, &keys, [true, true], &allocation, &transactions);
    let stable_locks = tree(context.requested_data_root())
        .into_iter()
        .filter(|(path, _)| path.to_string_lossy().contains("/.leases/build/"))
        .collect::<BTreeMap<_, _>>();
    let initial_tree = tree(context.requested_data_root());
    let initial_catalog = catalog(&context);
    for args in [&["prune", "--dry-run"][..], &["prune"][..]] {
        let output = cli(&binary, &stage, args, 0);
        statuses(&output, "protected", &objects);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("operation: "));
        assert_eq!(tree(context.requested_data_root()), initial_tree);
        assert_eq!(catalog(&context), initial_catalog);
    }
    let language_instance = objects[0].label();
    let refused = cli(
        &binary,
        &stage,
        &[
            "uninstall",
            "python@3.15.0",
            "--instance",
            &language_instance,
        ],
        1,
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("still referenced"));
    assert_eq!(tree(context.requested_data_root()), initial_tree);
    assert_eq!(catalog(&context), initial_catalog);

    for index in 0..2 {
        let before = tree(context.requested_data_root());
        let before_catalog = catalog(&context);
        let before_manifests = manifests(&context, &objects);
        let before_payloads = payloads.each_ref().map(|path| tree(path));
        let id = projects[index].to_string();
        cli(&binary, &stage, &["project", "forget", &id], 0);
        assert_ne!(catalog(&context), before_catalog);
        assert_ne!(tree(context.requested_data_root()), before);
        ready(&context, &objects, true);
        assert!(payloads.iter().all(|path| path.is_dir()));
        assert_eq!(manifests(&context, &objects), before_manifests);
        assert_eq!(payloads.each_ref().map(|path| tree(path)), before_payloads);
        selections(
            &context,
            &keys,
            [false, index == 0],
            &allocation,
            &transactions,
        );
        if index == 0 {
            b_before
                .ensure_current(ProjectRepository::open_readonly(&context).unwrap().model())
                .unwrap();
        }
        let before_preview = tree(context.requested_data_root());
        let output = cli(&binary, &stage, &["prune", "--dry-run"], 0);
        statuses(
            &output,
            if index == 0 {
                "protected"
            } else {
                "would remove"
            },
            &objects,
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("operation: "));
        assert_eq!(tree(context.requested_data_root()), before_preview);
    }

    let before_remove = tree(context.requested_data_root());
    let before_remove_catalog = catalog(&context);
    ready(&context, &objects, true);
    assert!(payloads.iter().all(|path| path.is_dir()));
    let removed = cli(&binary, &stage, &["prune"], 0);
    statuses(&removed, "removed", &objects);
    assert_ne!(tree(context.requested_data_root()), before_remove);
    assert_ne!(
        catalog(&context),
        before_remove_catalog,
        "physical completion retires the old generation"
    );
    ready(&context, &objects, false);
    assert!(payloads.iter().all(|path| !path.exists()));
    selections(&context, &keys, [false, false], &allocation, &transactions);
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .generation(&allocation)
            .is_none()
    );
    assert!(super::pending(&context).unwrap().is_empty());
    let completed = tree(context.requested_data_root());
    let completed_catalog = catalog(&context);
    for args in [vec!["prune", "--dry-run"], vec!["prune"], vec!["prune"]] {
        let output = cli(&binary, &stage, &args, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("No managed content needs reclamation"));
        assert_eq!(tree(context.requested_data_root()), completed);
        assert_eq!(catalog(&context), completed_catalog);
        ready(&context, &objects, false);
    }
    let final_tree = tree(context.requested_data_root());
    for (path, entry) in stable_locks {
        assert_eq!(
            final_tree.get(&path),
            Some(&entry),
            "original stable protection changed: {}",
            path.display()
        );
    }
    assert_eq!(tree(&project_paths[0]), project_files[0]);
    assert_eq!(tree(&project_paths[1]), project_files[1]);
    let final_binary_sha: [u8; 32] = Sha256::digest(std::fs::read(&binary).unwrap()).into();
    assert_eq!(final_binary_sha, binary_sha);
    println!("P4_REAL_MAIN_LIFECYCLE_PASSED");
}

#[tokio::test]
#[ignore = "requires PAKRO_TEST_MAIN_BINARY and kills the actual prune/uninstall processes"]
async fn p4_real_main_killed_deletion_is_unavailable_and_next_prune_cleans_it() {
    use crate::libraries::package::tree::prepare_package_tree;
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_MAIN_BINARY").unwrap()).canonicalize().unwrap();
    for exact in [false, true] {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        std::fs::create_dir(stage.join("home")).unwrap();
        let context = operation_context(&stage, true, Duration::from_secs(240));
        let bytes = archive("3.15.0");
        let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
        let names = (0..800).map(|index| format!("bulk/member{index:05}.py")).collect::<Vec<_>>();
        let files = || names.iter().map(|name| (name.as_str(), b"VALUE=1\n".as_slice())).collect();
        let (facts, artifact) = wheel(&context, &language, "bulk", [221; 32], vec![], files(), None);
        let package = prepare_package_tree(&context, &language, facts, artifact).unwrap();
        let id = ReclaimId::Package(package.manifest().key());
        let instance = id.label();
        let path = package.root().to_owned();
        drop(package);
        drop(language);
        let stdout = stage.join("delete.stdout");
        let stderr = stage.join("delete.stderr");
        let args = if exact { vec!["uninstall", "--instance", &instance] } else { vec!["prune"] };
        let mut child = ChildGuard(cli_command(&binary, &stage, &args)
            .stdout(std::fs::File::create(&stdout).unwrap()).stderr(std::fs::File::create(&stderr).unwrap())
            .spawn().unwrap());
        let pid = Pid::from_raw(child.0.id() as i32);
        let first = path.join("site-packages").join(&names[0]);
        let last = path.join("site-packages").join(names.last().unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while first.exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "deletion exited before interruption: {}", std::fs::read_to_string(&stderr).unwrap());
            assert!(std::time::Instant::now() < deadline, "deletion did not start within 60 seconds");
            std::thread::sleep(Duration::from_millis(1));
        }
        kill(pid, Signal::SIGSTOP).unwrap();
        assert!(matches!(waitpid(pid, Some(WaitPidFlag::WUNTRACED)).unwrap(), WaitStatus::Stopped(_, Signal::SIGSTOP)));
        assert!(last.exists(), "deletion finished before the real interruption");
        kill(pid, Signal::SIGKILL).unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        assert!(kill(pid, None).is_err(), "killed pakro process still exists");
        println!("P4_REAL_KILL exact={exact} pid={} first_removed=true remaining=true\n{}\n{}", pid.as_raw(), std::fs::read_to_string(&stdout).unwrap(), std::fs::read_to_string(&stderr).unwrap());
        ready(&context, &[id], false);
        assert!(super::pending(&context).unwrap().iter().any(|pending| pending.objects.contains(&id)));
        let listing = cli(&binary, &stage, &["installs", "ls", "--json"], 0);
        let rows: serde_json::Value = serde_json::from_slice(&listing.stdout).unwrap();
        assert!(rows.as_array().unwrap().iter().all(|row| row["instance"].as_str() != Some(instance.as_str())));
        let preview = cli(&binary, &stage, &["prune", "--dry-run"], 0);
        statuses(&preview, "would remove", &[id]);
        assert!(String::from_utf8_lossy(&preview.stdout).contains("unavailable remainder"));
        assert!(last.exists());
        let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
        let (facts, artifact) = wheel(&context, &language, "bulk", [221; 32], vec![], files(), None);
        let error = match prepare_package_tree(&context, &language, facts, artifact) {
            Ok(_) => panic!("an interrupted object must not be selected or republished"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("run pakro prune"), "{error:#}");
        drop(language);
        cli(&binary, &stage, &["prune"], 0);
        assert!(!path.exists());
        assert!(super::pending(&context).unwrap().is_empty());
        let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
        let (facts, artifact) = wheel(&context, &language, "bulk", [221; 32], vec![], files(), None);
        let installed = prepare_package_tree(&context, &language, facts, artifact).unwrap();
        installed.check_binding().unwrap();
        assert!(installed.root().join("site-packages").join(&names[0]).is_file());
    }
}
