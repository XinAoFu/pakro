use super::*;
use crate::config::{FrozenConfigDiscovery, Settings};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::*;
use std::fs;
use std::time::{Duration, Instant};

#[path = "admission_tests.rs"]
mod admission;

const OLD: &[u8] = b"# native comment\nlockfile_version=4\n[future]\nvalue='preserved'\n";
const NEW: &[u8] = b"# native comment\nlockfile_version=4\n[future]\nvalue='preserved'\n[python]\n";

fn context(root: &Path, locked: bool) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: OperationPolicy {
            locked,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 64,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: !locked,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}
fn observe(project: &Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_path_buf()];
    observe_project(
        project,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60)),
    )
    .unwrap()
}
fn make_project(root: &Path, label: &str, old: bool) -> PathBuf {
    let project = root.join(label);
    fs::create_dir(&project).unwrap();
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.13.16'\n").unwrap();
    if old {
        fs::write(project.join("pakro.lock"), OLD).unwrap();
    }
    project
}
fn apply(context: &OperationContext, project: &Path, nonce: Uuid) -> PendingLockCommit {
    let observed = observe(project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(context).unwrap();
    let writer = repository.writer().unwrap();
    let lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
    let _guard = writer
        .control(ControlLimits::default())
        .unwrap()
        .acquire_project(&lock)
        .unwrap();
    PendingLockCommit::apply(
        context,
        &observed,
        writer.clone(),
        identity.as_uuid(),
        nonce,
        0,
        project_state(repository.model(), identity.as_uuid()),
        vec![pakro_store::Digest::Sha256([7; 32]).to_string()],
        &project.join("pakro.lock"),
        NEW,
    )
    .unwrap()
}
fn commit_catalog(context: &OperationContext, project: &Path, nonce: Uuid) {
    use crate::packages::python_environment::model::SelectionKey;
    let observed = observe(project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let mut repository = ProjectRepository::open_writer(context).unwrap();
    let key = SelectionKey::new(
        identity.as_uuid(),
        "default",
        [8; 32],
        repository.model().limits(),
    )
    .unwrap();
    let expected = repository.model().snapshot(&key);
    repository
        .model_mut()
        .register_project(
            identity.as_uuid(),
            project.to_str().unwrap(),
            &"0".repeat(64),
        )
        .unwrap();
    repository
        .model_mut()
        .apply_language_selection(
            &key,
            &expected,
            &pakro_store::Digest::Sha256([9; 32]).to_string(),
            nonce,
        )
        .unwrap();
    repository.commit_model().unwrap();
}

#[test]
fn p4_pending_projection_keeps_all_transactions_and_damage_is_not_empty() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let a = make_project(&root, "a", false);
    let b = make_project(&root, "b", false);
    let first = apply(&context, &a, Uuid::from_u128(501));
    let second = apply(&context, &b, Uuid::from_u128(502));
    let roots = pending_protections(&context).unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(
        roots
            .iter()
            .map(|root| root.transaction)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([Uuid::from_u128(501), Uuid::from_u128(502)])
    );
    assert!(roots.iter().all(|root| root.references == vec![[7; 32]]));
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), NEW);
    assert_eq!(fs::read(b.join("pakro.lock")).unwrap(), NEW);
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let mut control = writer.control(ControlLimits::default()).unwrap();
    let snapshot = control
        .read(ControlNamespace::Journals, &name(roots[0].project))
        .unwrap()
        .unwrap();
    control
        .replace(
            ControlNamespace::Journals,
            &name(roots[0].project),
            Some(&snapshot),
            b"damaged pending reference facts",
        )
        .unwrap();
    drop(control);
    assert!(pending_protections(&context).is_err());
    assert!(pending_reference_chains(&context, [6; 32]).is_err());
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), NEW);
    assert_eq!(fs::read(b.join("pakro.lock")).unwrap(), NEW);
    drop((first, second));
}

#[test]
fn p2_document_before_catalog_is_pending_and_rollback_restores_exact_bytes() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let other = make_project(&root, "other", false);
    let context = context(&root, false);
    let pending = apply(&context, &project, Uuid::now_v7());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    assert!(ensure_project_consistent(&context, &observe(&project)).is_err());
    ensure_project_consistent(&context, &observe(&other)).unwrap();
    assert_eq!(
        pending_reference_chains(&context, [7; 32]).unwrap().len(),
        1
    );
    assert!(
        pending_reference_chains(&context, [6; 32])
            .unwrap()
            .is_empty()
    );
    drop(pending);
    assert!(recover_project_lock(&context, &observe(&project)).unwrap());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
    ensure_project_consistent(&context, &observe(&project)).unwrap();
    assert!(!recover_project_lock(&context, &observe(&project)).unwrap());
    assert!(
        pending_reference_chains(&context, [7; 32])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn p2_new_lock_before_catalog_is_removed_on_owned_recovery() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", false);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    recover_project_lock(&context, &observe(&project)).unwrap();
    assert!(!project.join("pakro.lock").exists());
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .registered_projects()
            .next()
            .is_none()
    );
}

#[test]
fn p2_catalog_commit_recovery_keeps_the_published_lock() {
    use std::os::unix::fs::MetadataExt;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    let nonce = Uuid::now_v7();
    let pending = apply(&context, &project, nonce);
    commit_catalog(&context, &project, nonce);
    let inode = fs::metadata(project.join("pakro.lock")).unwrap().ino();
    drop(pending);
    recover_project_lock(&context, &observe(&project)).unwrap();
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    assert_eq!(
        fs::metadata(project.join("pakro.lock")).unwrap().ino(),
        inode
    );
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .applied_transaction(nonce)
            .is_some()
    );
}

#[test]
fn p2_external_lock_edit_is_preserved_and_remains_pending() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let external = b"# external writer\nlockfile_version=4\n";
    fs::write(project.join("pakro.lock"), external).unwrap();
    assert!(recover_project_lock(&context, &observe(&project)).is_err());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), external);
    assert_eq!(
        pending_reference_chains(&context, [7; 32]).unwrap().len(),
        1
    );
}

#[test]
fn p2_later_project_revision_prevents_rollback_without_rewinding_catalog() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let successor = Uuid::now_v7();
    commit_catalog(&context, &project, successor);
    assert!(recover_project_lock(&context, &observe(&project)).is_err());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .applied_transaction(successor)
            .is_some()
    );
}

#[test]
fn p2_unrelated_catalog_commit_survives_owned_rollback() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let other = make_project(&root, "other", false);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let other_nonce = Uuid::now_v7();
    commit_catalog(&context, &other, other_nonce);
    recover_project_lock(&context, &observe(&project)).unwrap();
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .applied_transaction(other_nonce)
            .is_some()
    );
}

#[test]
fn p2_locked_request_never_rolls_back_lock_bytes_or_inode() {
    use std::os::unix::fs::MetadataExt;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let resolve = context(&root, false);
    drop(apply(&resolve, &project, Uuid::now_v7()));
    let inode = fs::metadata(project.join("pakro.lock")).unwrap().ino();
    let locked = context(&root, true);
    assert!(recover_project_lock(&locked, &observe(&project)).is_err());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    assert_eq!(
        fs::metadata(project.join("pakro.lock")).unwrap().ino(),
        inode
    );
}

#[test]
fn p2_changed_original_inode_is_rejected_before_journal_or_publication() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    fs::write(project.join("replacement"), OLD).unwrap();
    fs::rename(project.join("replacement"), project.join("pakro.lock")).unwrap();
    assert!(
        PendingLockCommit::apply(
            &context,
            &observed,
            repository.writer().unwrap(),
            identity.as_uuid(),
            Uuid::now_v7(),
            0,
            project_state(repository.model(), identity.as_uuid()),
            vec![],
            &project.join("pakro.lock"),
            NEW
        )
        .is_err()
    );
    ensure_project_consistent(&context, &observe(&project)).unwrap();
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
}

#[test]
fn p2_recovery_retries_after_original_document_was_restored() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let key = name(identity.as_uuid());
    let mut snapshot = writer
        .control(ControlLimits::default())
        .unwrap()
        .read(ControlNamespace::Journals, &key)
        .unwrap()
        .unwrap();
    let mut journal = decode(&snapshot, Some(identity.as_uuid())).unwrap();
    let mut session = ProjectLockSession::acquire(
        &project.join("pakro.lock"),
        MAX_LOCK,
        context.policy().deadline,
    )
    .unwrap();
    let current = session.snapshot().unwrap();
    session
        .compare_and_replace_journaled(&current, OLD, |temporary, inode| {
            journal.rollback_staged = Some((temporary.to_owned(), inode));
            snapshot = writer.control(ControlLimits::default())?.replace(
                ControlNamespace::Journals,
                &key,
                Some(&snapshot),
                &serde_json::to_vec(&journal)?,
            )?;
            Ok(())
        })
        .unwrap();
    // Simulate a crash after rollback's rename and before journal removal.
    drop(session);
    drop(snapshot);
    drop(repository);
    drop(writer);
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
    recover_project_lock(&context, &observe(&project)).unwrap();
    ensure_project_consistent(&context, &observe(&project)).unwrap();
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
}

#[test]
fn p2_staged_inode_is_recorded_before_rename_and_prewrite_failure_recovers() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let project_lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
    let project_guard = writer
        .control(ControlLimits::default())
        .unwrap()
        .acquire_project(&project_lock)
        .unwrap();
    let path = project.join("pakro.lock");
    let mut session =
        ProjectLockSession::acquire(&path, MAX_LOCK, context.policy().deadline).unwrap();
    let old = session.snapshot().unwrap();
    let mut journal = Journal {
        schema: 1,
        project: identity.as_uuid(),
        transaction: Uuid::now_v7(),
        expected_revision: 0,
        expected_project: project_state(repository.model(), identity.as_uuid()),
        path: path.clone(),
        parent: parent_identity(&path).unwrap(),
        old_bytes: old.bytes().map(<[u8]>::to_vec),
        old_identity: old.file_identity(),
        new_bytes: NEW.to_vec(),
        staged: None,
        rollback_staged: None,
        references: vec![pakro_store::Digest::Sha256([7; 32]).to_string()],
        preparing_packages: None,
    };
    let key = name(identity.as_uuid());
    let mut snapshot = writer
        .control(ControlLimits::default())
        .unwrap()
        .replace(
            ControlNamespace::Journals,
            &key,
            None,
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
    let result = session.compare_and_replace_journaled(&old, NEW, |temporary, inode| {
        journal.staged = Some((temporary.to_owned(), inode));
        snapshot = writer.control(ControlLimits::default())?.replace(
            ControlNamespace::Journals,
            &key,
            Some(&snapshot),
            &serde_json::to_vec(&journal)?,
        )?;
        assert_eq!(fs::read(&path).unwrap(), OLD);
        Err(eyre::eyre!("injected interruption before rename"))
    });
    assert!(result.is_err());
    drop(session);
    drop(snapshot);
    drop(project_guard);
    drop(project_lock);
    drop(repository);
    drop(writer);
    recover_project_lock(&context, &observe(&project)).unwrap();
    ensure_project_consistent(&context, &observe(&project)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), OLD);
}

#[test]
fn p2_another_project_observing_the_same_lock_reports_pending() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let nested = project.join("nested");
    fs::create_dir(&nested).unwrap();
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &nested,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    assert!(observed.observed_auxiliary(&project.join("pakro.lock")));
    assert!(ensure_project_consistent(&context, &observed).is_err());
    recover_project_lock(&context, &observe(&project)).unwrap();
}

#[test]
fn p2_recovery_handoff_preserves_owned_inode_and_original_nonlock_inputs() {
    for change in ["none", "lock", "declaration"] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let project = make_project(&root, "project", true);
        let context = context(&root, false);
        drop(apply(&context, &project, Uuid::now_v7()));
        let observed = observe(&project);
        let edit_root = project.clone();
        let result = with_commit_test_hook(
            move |stage| {
                if stage != "recovery-before-observe" {
                    return;
                }
                if change == "lock" {
                    fs::write(edit_root.join("foreign-lock"), OLD).unwrap();
                    fs::rename(edit_root.join("foreign-lock"), edit_root.join("pakro.lock"))
                        .unwrap();
                } else if change == "declaration" {
                    fs::write(
                        edit_root.join("pakro.toml"),
                        "[tools]\npython='3.13.16'\n# external change during recovery handoff\n",
                    )
                    .unwrap();
                }
            },
            || recover_project_lock_and_observe(&context, &observed),
        );
        if change == "none" {
            let fresh = result.unwrap().unwrap();
            fresh.recheck().unwrap();
            assert_eq!(fresh.deadline(), observed.deadline());
            assert_eq!(fresh.cli_arguments(), observed.cli_arguments());
            assert_eq!(fresh.environment(), observed.environment());
        } else {
            assert!(
                result.is_err(),
                "external {change} must not be adopted into this operation"
            );
        }
        assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
        if change == "declaration" {
            assert!(
                fs::read_to_string(project.join("pakro.toml"))
                    .unwrap()
                    .contains("external change during recovery handoff")
            );
        }
    }
}

#[test]
fn p2_readonly_lock_alias_cannot_bypass_another_projects_pending_transaction() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let alias = make_project(&root, "alias", false);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    std::os::unix::fs::symlink(project.join("pakro.lock"), alias.join("pakro.lock")).unwrap();
    let observed = observe(&alias);
    assert!(!observed.observed_auxiliary(&project.join("pakro.lock")));
    assert!(ensure_project_consistent(&context, &observed).is_err());
    assert!(
        std::fs::symlink_metadata(alias.join("pakro.lock"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
}

#[test]
fn p2_recovery_write_result_never_adopts_external_content_before_journal_clear() {
    for original_exists in [true, false] {
        for edit in ["write", "replace"] {
            let owner = tempfile::tempdir().unwrap();
            let root = owner.path().canonicalize().unwrap();
            let project = make_project(&root, "project", original_exists);
            let context = context(&root, false);
            drop(apply(&context, &project, Uuid::now_v7()));
            let observed = observe(&project);
            let edit_root = project.clone();
            // Replacing an existing restored lock uses identical bytes to
            // prove that retaining the actual inode matters independently.
            let foreign = if edit == "replace" && original_exists {
                OLD.to_vec()
            } else {
                b"lockfile_version=4\n# external edit after owned recovery write\n".to_vec()
            };
            let foreign_bytes = foreign.clone();
            let result = with_commit_test_hook(
                move |stage| {
                    if stage != "recovery-written" {
                        return;
                    }
                    if edit == "replace" {
                        fs::write(edit_root.join("foreign-lock"), &foreign_bytes).unwrap();
                        fs::rename(edit_root.join("foreign-lock"), edit_root.join("pakro.lock"))
                            .unwrap();
                    } else {
                        fs::write(edit_root.join("pakro.lock"), &foreign_bytes).unwrap();
                    }
                },
                || recover_project_lock_and_observe(&context, &observed),
            );
            assert!(
                result.is_err(),
                "external {edit}, original_exists={original_exists} must not become the owned recovery baseline"
            );
            assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), foreign);
            assert!(
                ensure_project_consistent(&context, &observe(&project)).is_err(),
                "journal must remain pending"
            );
            assert_eq!(
                pending_reference_chains(&context, [7; 32]).unwrap().len(),
                1
            );
            assert!(
                ProjectRepository::open_readonly(&context)
                    .unwrap()
                    .model()
                    .registered_projects()
                    .next()
                    .is_none()
            );
        }
    }
}

#[test]
fn preparing_conda_group_is_owned_by_the_original_p2_journal_and_not_rolled_back() {
    use crate::libraries::package::{
        conda::{package_key, record_source},
        manifest::{
            PACKAGE_FACTS_FILE, PackageFacts, PackageManifest, StorageManifest, package_facts_bytes,
        },
    };
    use crate::packages::python_environment::prefix::{
        EnvironmentStore, PrefixLimits,
        verify::{PrefixInventoryLimits, observe_tree_inventory},
    };
    use rattler_conda_types::{
        PackageName, PackageRecord, RepoDataRecord, Version,
        package::{ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType},
    };
    use std::{io::Write, str::FromStr};
    let crash_root = std::env::var_os("PAKRO_TEST_PREPARING_PACKAGE_ROOT").map(PathBuf::from);
    let owner = crash_root.is_none().then(|| tempfile::tempdir().unwrap());
    let root = crash_root
        .unwrap_or_else(|| owner.as_ref().unwrap().path().to_owned())
        .canonicalize()
        .unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let expected = crate::libraries::project::state::ExpectedProjectState::from_model(
        repository.model(),
        identity,
    );
    let writer = repository.writer().unwrap();
    let lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
    let guard = writer
        .control(ControlLimits::default())
        .unwrap()
        .acquire_project(&lock)
        .unwrap();
    let archive = b"owned Conda fixture archive";
    let digest = pakro_store::Digest::from_sha256_bytes(archive);
    let pakro_store::Digest::Sha256(hash) = digest else {
        unreachable!()
    };
    let mut record = PackageRecord::new(
        PackageName::from_str("demo").unwrap(),
        Version::from_str("1.0").unwrap(),
        "build_0".into(),
    );
    record.subdir = "noarch".into();
    record.sha256 = Some(hash.into());
    record.size = Some(archive.len() as u64);
    let record = RepoDataRecord {
        package_record: record,
        identifier: DistArchiveIdentifier::new(
            ArchiveIdentifier {
                name: "demo".into(),
                version: "1.0".into(),
                build_string: "build_0".into(),
            },
            DistArchiveType::Conda(CondaArchiveType::Conda),
        ),
        url: "https://example.invalid/noarch/demo-1.0-build_0.conda"
            .parse()
            .unwrap(),
        channel: Some("https://example.invalid/".into()),
    };
    let key = package_key(&record, std::slice::from_ref(&record)).unwrap();
    let facts = PackageFacts {
        key,
        name: "demo".into(),
        version: "1.0".into(),
        artifact: digest.to_string(),
        source: record_source(&record).unwrap(),
        dependencies: vec![],
    };
    let store = EnvironmentStore::open_shared_writer(
        &crate::libraries::package::manifest::store_path(&context),
        crate::libraries::package::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    for (path, bytes) in [
        ("conda-archive", archive.as_slice()),
        (
            PACKAGE_FACTS_FILE,
            package_facts_bytes(&facts, &None, &record.identifier.to_string())
                .unwrap()
                .as_slice(),
        ),
    ] {
        let mut output = prefix
            .create_file_exclusive(Path::new(path), 0o600)
            .unwrap();
        output.write_all(bytes).unwrap();
        output.finish().unwrap();
    }
    let physical = prefix.generation_identity().unwrap().facts().unwrap();
    let manifest = PackageManifest {
        facts,
        language: None,
        target: Some("noarch".into()),
        conda: Some(record.clone()),
        filename: record.identifier.to_string(),
        storage: StorageManifest {
            schema: 1,
            key,
            allocation: physical.allocation,
            lock: physical.lock,
            root_identity: physical.generation,
            inventory: observe_tree_inventory(&prefix, PrefixInventoryLimits::default()).unwrap(),
        },
    };
    let allocation = prefix.final_path().to_owned();
    prepare_package_publication(
        &context,
        &observed,
        &expected,
        &writer,
        &lock,
        &guard,
        Uuid::now_v7(),
        &project.join("pakro.lock"),
        &mut [(&mut prefix, &manifest)],
        &[],
        None,
    )
    .unwrap();
    if std::env::var_os("PAKRO_TEST_PREPARING_PACKAGE_ROOT").is_some() {
        std::process::exit(86);
    }
    assert_eq!(
        pending_protections(&context).unwrap()[0].references,
        vec![key]
    );
    drop(prefix);
    assert!(
        allocation.exists(),
        "original prepared allocation must outlive its caller"
    );
    drop((guard, lock, repository, writer));
    let report = crate::programs::uninstall::prune::run(&context, false).unwrap();
    assert!(
        !report.complete,
        "P4 must not delete an unfinished preparation"
    );
    assert!(allocation.exists());
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.14.0'\n").unwrap();
    assert!(
        recover_project_lock(&context, &observe(&project)).is_err(),
        "recovery must reject changed declarations"
    );
    assert!(allocation.exists());
    assert_eq!(
        pending_protections(&context).unwrap()[0].references,
        vec![key]
    );
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.13.16'\n").unwrap();
    assert!(recover_project_lock(&context, &observe(&project)).unwrap());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
    assert!(allocation.exists());
    assert!(pending_protections(&context).unwrap().is_empty());
    let acquired = crate::libraries::package::lease::acquire_package(&context, key)
        .unwrap()
        .unwrap();
    assert_eq!(acquired.manifest().facts.key, key);
    acquired.check_binding().unwrap();
}

#[test]
fn preparing_conda_journal_survives_process_exit_and_resumes_original_owner() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "programs::shared::lock_transaction::tests::preparing_conda_group_is_owned_by_the_original_p2_journal_and_not_rolled_back", "--nocapture"])
        .env("PAKRO_TEST_PREPARING_PACKAGE_ROOT", &root)
        .output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context = context(&root, false);
    let pending = pending_protections(&context).unwrap();
    assert_eq!(pending.len(), 1);
    let key = pending[0].references[0];
    assert!(
        crate::libraries::package::lease::acquire_package(&context, key)
            .unwrap()
            .is_none()
    );
    assert!(
        !crate::programs::uninstall::prune::run(&context, false)
            .unwrap()
            .complete
    );
    let project = root.join("project");
    assert!(recover_project_lock(&context, &observe(&project)).unwrap());
    assert!(pending_protections(&context).unwrap().is_empty());
    assert!(
        crate::libraries::package::lease::acquire_package(&context, key)
            .unwrap()
            .is_some()
    );
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
}

fn fixed_lock_install_observed(project: &Path) -> ObservedProjectInputs {
    observe(project)
        .with_fixed_project_lock(&project.join("pakro.lock"))
        .unwrap()
}
fn fixed_lock_install_apply(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    project: &Path,
    nonce: Uuid,
) -> PendingLockCommit {
    let identity = ProjectIdentity::from_observation(observed).unwrap();
    let repository = ProjectRepository::open_writer(context).unwrap();
    let writer = repository.writer().unwrap();
    let lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
    let _guard = writer
        .control(ControlLimits::default())
        .unwrap()
        .acquire_project(&lock)
        .unwrap();
    PendingLockCommit::apply(
        context,
        observed,
        writer.clone(),
        identity.as_uuid(),
        nonce,
        0,
        project_state(repository.model(), identity.as_uuid()),
        vec![pakro_store::Digest::Sha256([7; 32]).to_string()],
        &project.join("pakro.lock"),
        NEW,
    )
    .unwrap()
}
fn fixed_lock_install_journal(context: &OperationContext, uuid: Uuid) -> Journal {
    let read = read_store(context).unwrap().unwrap();
    let snapshot = read
        .control(ControlLimits::default())
        .unwrap()
        .read(ControlNamespace::Journals, &name(uuid))
        .unwrap()
        .unwrap();
    decode(&snapshot, Some(uuid)).unwrap()
}

#[test]
fn fixed_lock_install_changed_lock_after_observation_still_writes_prepared_result() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "changed", true);
    let observed = fixed_lock_install_observed(&project);
    let edited = b"user edit after observation\n".as_slice();
    fs::write(project.join("pakro.lock"), edited).unwrap();
    let pending = fixed_lock_install_apply(&context, &observed, &project, Uuid::new_v4());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    let uuid = ProjectIdentity::from_observation(&observed)
        .unwrap()
        .as_uuid();
    let journal = fixed_lock_install_journal(&context, uuid);
    // One write-time backup: bytes and identity from the same observation.
    assert_eq!(journal.old_bytes.as_deref(), Some(edited));
    assert!(journal.old_identity.is_some());
    drop(pending);
}

#[test]
fn fixed_lock_install_lock_appeared_after_missing_observation_is_replaced_by_prepared_result() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "appeared", false);
    let observed = fixed_lock_install_observed(&project);
    let appeared = b"lockfile_version = 4\n[future]\nvalue='user'\n".as_slice();
    fs::write(project.join("pakro.lock"), appeared).unwrap();
    let pending = fixed_lock_install_apply(&context, &observed, &project, Uuid::new_v4());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    let uuid = ProjectIdentity::from_observation(&observed)
        .unwrap()
        .as_uuid();
    let journal = fixed_lock_install_journal(&context, uuid);
    assert_eq!(journal.old_bytes.as_deref(), Some(appeared));
    assert!(journal.old_identity.is_some());
    drop(pending);
}

#[test]
fn fixed_lock_install_lock_deleted_after_observation_records_an_absent_backup() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "deleted", true);
    let observed = fixed_lock_install_observed(&project);
    fs::remove_file(project.join("pakro.lock")).unwrap();
    let pending = fixed_lock_install_apply(&context, &observed, &project, Uuid::new_v4());
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    let uuid = ProjectIdentity::from_observation(&observed)
        .unwrap()
        .as_uuid();
    let journal = fixed_lock_install_journal(&context, uuid);
    assert!(journal.old_bytes.is_none());
    assert!(journal.old_identity.is_none());
    drop(pending);
}

#[test]
fn fixed_lock_install_edit_between_backup_and_rename_still_writes_prepared_result() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "midflight", true);
    let observed = fixed_lock_install_observed(&project);
    // "documents-staged" fires after the write-time backup and before the
    // rename: an external edit in this window must not stop the publication.
    let lock_path = project.join("pakro.lock");
    let pending = with_commit_test_hook(
        move |stage| {
            if stage == "documents-staged" {
                fs::write(&lock_path, b"mid-flight external edit\n").unwrap();
            }
        },
        || fixed_lock_install_apply(&context, &observed, &project, Uuid::new_v4()),
    );
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), NEW);
    let uuid = ProjectIdentity::from_observation(&observed)
        .unwrap()
        .as_uuid();
    let journal = fixed_lock_install_journal(&context, uuid);
    assert_eq!(journal.old_bytes.as_deref(), Some(OLD));
    assert!(journal.old_identity.is_some());
    drop(pending);
}

#[test]
fn fixed_lock_install_edit_after_write_does_not_block_catalog_commit_or_finalize() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "postwrite", true);
    let observed = fixed_lock_install_observed(&project);
    let nonce = Uuid::new_v4();
    // "documents-applied" fires after the rename inside apply: the external
    // edit neither fails the pending transaction nor rewrites the lock again.
    let hook_lock = project.join("pakro.lock");
    let pending = with_commit_test_hook(
        move |stage| {
            if stage == "documents-applied" {
                fs::write(&hook_lock, b"post-write external edit\n").unwrap();
            }
        },
        || fixed_lock_install_apply(&context, &observed, &project, nonce),
    );
    commit_catalog(&context, &project, nonce);
    pending.recheck_inputs(&observed).unwrap();
    pending.finalize().unwrap();
    assert_eq!(
        fs::read(project.join("pakro.lock")).unwrap(),
        b"post-write external edit\n".as_slice()
    );
    let uuid = ProjectIdentity::from_observation(&observed)
        .unwrap()
        .as_uuid();
    let read = read_store(&context).unwrap().unwrap();
    assert!(
        read.control(ControlLimits::default())
            .unwrap()
            .read(ControlNamespace::Journals, &name(uuid))
            .unwrap()
            .is_none()
    );
}

#[test]
fn fixed_lock_install_flow_binds_initial_observed_bytes_after_disk_change() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let project = make_project(&root, "initial-input", true);
    let observed = fixed_lock_install_observed(&project);
    let later = b"later lock content that is not a document\n";
    fs::write(project.join("pakro.lock"), later).unwrap();
    // The installation's package phase binds the initial observed bytes; the
    // later disk content is never decoded as input.
    let path = crate::programs::python::project_lock_path(&observed).unwrap();
    let bound = crate::packages::python_resolution::lock_session::BoundPythonLock::bind_fixed(
        &path,
        observed.document(&path),
        Default::default(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    bound.recheck().unwrap();
    assert!(bound.model().is_none());
    drop(bound);
    // The disk really holds the later content: an acquiring session would
    // have to decode it and refuses.
    assert!(
        crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(
            &path,
            Default::default(),
            Instant::now() + Duration::from_secs(60),
        )
        .is_err()
    );
}

#[test]
fn fixed_lock_install_fixed_observation_exempts_only_the_project_lock_entry() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "exempt", true);
    let fixed = fixed_lock_install_observed(&project);
    let plain = observe(&project);
    fs::write(project.join("pakro.lock"), b"externally changed\n").unwrap();
    fixed.recheck().unwrap();
    assert!(plain.recheck().is_err());
    // Configuration edits still refuse a fixed installation input.
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.14.1'\n").unwrap();
    assert!(fixed.recheck().is_err());
}

#[test]
fn fixed_lock_install_missing_lock_fixed_observation_allows_its_appearance() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "missing", false);
    let fixed = fixed_lock_install_observed(&project);
    fixed.recheck().unwrap();
    fs::write(project.join("pakro.lock"), "lockfile_version = 4\n").unwrap();
    fixed.recheck().unwrap();
}

#[test]
fn fixed_lock_install_with_fixed_refuses_an_unobserved_lock_path() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "unobserved", true);
    let observed = observe(&project);
    assert!(
        observed
            .with_fixed_project_lock(&project.join("other.lock"))
            .is_err()
    );
}

#[test]
fn fixed_lock_install_symlinked_lock_keeps_cross_project_pending_ownership() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    // An unfinished transaction of another project targets its own lock.
    let other = make_project(&root, "other", true);
    let other_observed = observe(&other);
    drop(fixed_lock_install_apply(
        &context,
        &other_observed,
        &other,
        Uuid::new_v4(),
    ));
    // This project's lock is a link to that other project's real lock; the
    // fixed input records the resolved target and must not bypass ownership.
    let project = make_project(&root, "linked", false);
    std::os::unix::fs::symlink(other.join("pakro.lock"), project.join("pakro.lock")).unwrap();
    let observed = fixed_lock_install_observed(&project);
    assert_eq!(
        observed.fixed_lock_target(),
        Some(other.join("pakro.lock").as_path())
    );
    assert!(
        own_pending_lock_commit(&context, &observed).is_err(),
        "the linked lock's real target belongs to another unfinished transaction"
    );
}

#[test]
fn fixed_lock_install_repointed_link_does_not_move_input_or_guard() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let initial = make_project(&root, "initial", true);
    let later = make_project(&root, "later", true);
    let project = make_project(&root, "linkproj", false);
    let link = project.join("pakro.lock");
    std::os::unix::fs::symlink(initial.join("pakro.lock"), &link).unwrap();
    let observed = fixed_lock_install_observed(&project);
    // Re-point the link after the observation: neither the recorded target
    // nor the input bytes may follow the later destination.
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(later.join("pakro.lock"), &link).unwrap();
    assert_eq!(
        observed.fixed_lock_target(),
        Some(initial.join("pakro.lock").as_path())
    );
    let path = crate::programs::python::project_lock_path(&observed).unwrap();
    let bound = crate::packages::python_resolution::lock_session::BoundPythonLock::bind_fixed(
        observed.fixed_lock_target().unwrap(),
        observed.document(&path),
        Default::default(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(bound.model().is_none());
    bound.recheck().unwrap();
    // The guard sits on the initial target's directory: the later target is
    // free to acquire, the initial one is not while the input is held.
    ProjectLockSession::acquire(
        &later.join("pakro.lock"),
        1024,
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert!(
        ProjectLockSession::acquire(
            &initial.join("pakro.lock"),
            1024,
            Instant::now() + Duration::from_millis(150),
        )
        .is_err()
    );
    drop(bound);
}

#[tokio::test]
async fn fixed_lock_install_pending_history_recovers_through_real_entry_first() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    // Valid durations the execution path parses regardless of the offline
    // policy, plus the same local-archive attestation posture as the other
    // real-entry tests. Everything this flow uses is prepared locally.
    let mut settings = Settings::default();
    settings.http_timeout = "20s".into();
    settings.http_download_timeout = "60s".into();
    settings.fetch_remote_versions_cache = "1h".into();
    settings.python.github_attestations = Some(false);
    let context = capture_operation_context(OperationContextInputs {
        settings: &settings,
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 64,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: true,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: Some(crate::programs::python::public_sources().unwrap()),
        python_target_description: None,
    })
    .unwrap();
    // The shared helper declares 3.13.16; this test prepares 3.15.0 locally
    // and must declare exactly that, or the flow would look for an unprepared
    // runtime in the native catalog.
    let project = make_project(&root, "recovery", true);
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let language = crate::libraries::language::repository::tests::archive("3.15.0");
    drop(
        crate::libraries::language::repository::tests::prepare(
            &crate::libraries::language::repository::tests::install_spec(&language),
            &context,
            &language,
        )
        .await
        .unwrap(),
    );
    // Leave this project's own unfinished journal in place.
    let pending = b"# pending native\nlockfile_version=4\n[future]\nstate='pending'\n";
    {
        let observed = observe(&project);
        let identity = ProjectIdentity::from_observation(&observed).unwrap();
        let repository = ProjectRepository::open_writer(&context).unwrap();
        let writer = repository.writer().unwrap();
        let lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
        let _guard = writer
            .control(ControlLimits::default())
            .unwrap()
            .acquire_project(&lock)
            .unwrap();
        PendingLockCommit::apply(
            &context,
            &observed,
            writer.clone(),
            identity.as_uuid(),
            Uuid::new_v4(),
            0,
            project_state(repository.model(), identity.as_uuid()),
            vec![],
            &project.join("pakro.lock"),
            pending,
        )
        .unwrap();
    }
    // The scenario is real: the pending bytes are durable on disk and this
    // project has an unfinished transaction.
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), pending);
    assert!(own_pending_lock_commit(&context, &observe(&project)).unwrap());
    // The real installation entry recovers the pending transaction first,
    // then fixes this run's input and completes its own work.
    let plan = crate::programs::python::project_plan_with_intent(
        context.clone(),
        observe(&project),
        &[],
        crate::programs::shared::python_roots::PythonLockIntent::Preserve,
    )
    .unwrap()
    .unwrap();
    let config = crate::config::Config::load_from_config_files(Default::default(), false)
        .await
        .unwrap();
    plan.install(config).await.unwrap();
    // The catalog was never committed for the pending transaction, so
    // recovery withdraws its write and restores the original document; this
    // installation then builds on that recovered input.
    let final_lock = fs::read_to_string(project.join("pakro.lock")).unwrap();
    assert!(
        final_lock.contains("value='preserved'"),
        "recovery rolled the uncommitted pending write back to the original"
    );
    assert!(
        !final_lock.contains("state='pending'"),
        "the uncommitted pending bytes must not survive recovery"
    );
    assert!(
        final_lock.contains("[python]"),
        "this installation generates its own graph section"
    );
    assert!(
        !own_pending_lock_commit(&context, &observe(&project)).unwrap(),
        "no unfinished journal may remain"
    );
}
