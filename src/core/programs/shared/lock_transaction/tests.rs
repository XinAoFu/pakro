use super::*;
use crate::config::{FrozenConfigDiscovery, Settings};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::*;
use std::fs;
use std::time::{Duration, Instant};

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
