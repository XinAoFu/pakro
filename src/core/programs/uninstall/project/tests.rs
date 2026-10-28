use super::*;
use crate::packages::python_environment::model::SelectionKey;
use crate::programs::*;
use pakro_store::control::{ControlNamespace, lease::StableLock};
use std::time::{Duration, Instant};

fn context(root: &std::path::Path, write: bool) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(30),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: false,
                project_selection: write,
                lockfile: false,
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

fn register(context: &OperationContext, project: Uuid, root: &std::path::Path) {
    let mut repository = ProjectRepository::open_writer(context).unwrap();
    let writer = repository.writer().unwrap();
    let name = format!("project-{}.lock", project.hyphenated());
    drop(
        StableLock::create_new(
            writer.build_handle(),
            &name,
            crate::libraries::project::repository::repository_uid(),
        )
        .unwrap(),
    );
    repository
        .model_mut()
        .register_project(project, root.to_str().unwrap(), &"a".repeat(64))
        .unwrap();
    for (index, group) in ["default", "auxiliary"].into_iter().enumerate() {
        let key = SelectionKey::new(
            project,
            group,
            [index as u8 + 1; 32],
            repository.model().limits(),
        )
        .unwrap();
        let expected = repository.model().snapshot(&key);
        repository
            .model_mut()
            .apply_language_selection(
                &key,
                &expected,
                &pakro_store::Digest::Sha256([7; 32]).to_string(),
                Uuid::now_v7(),
            )
            .unwrap();
    }
    repository.commit_model().unwrap();
}

fn bytes(context: &OperationContext) -> Vec<u8> {
    ProjectRepository::open_readonly(context)
        .unwrap()
        .model()
        .encode_catalog(Default::default())
        .unwrap()
}

#[test]
fn p3_forget_deleted_directory_clears_all_groups_preserves_b_and_versions() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, true);
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let gone = root.join("already-deleted-project");
    register(&context, first, &gone);
    register(&context, second, &root.join("other-project"));
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let first_before =
        ExpectedProjectState::from_model(repository.model(), ProjectIdentity::from_uuid(first));
    let second_before =
        ExpectedProjectState::from_model(repository.model(), ProjectIdentity::from_uuid(second));
    let initial_transactions = repository
        .model()
        .selection_slots()
        .filter(|(key, _)| key.project() == first)
        .map(|(_, stamp)| stamp.transaction())
        .collect::<Vec<_>>();
    drop(repository);
    assert!(!gone.exists());
    let report = forget(&context, &first.to_string(), false).unwrap();
    assert!(report.forgotten);
    assert_eq!(report.associations.len(), 2);
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    assert!(repository.model().project_registration(first).is_none());
    assert!(first_before.ensure_current(repository.model()).is_err());
    second_before.ensure_current(repository.model()).unwrap();
    for (key, stamp) in repository
        .model()
        .selection_slots()
        .filter(|(key, _)| key.project() == first)
    {
        assert_eq!(stamp.revision(), 2);
        assert!(stamp.language().is_none() && stamp.generation().is_none());
        assert_ne!(key.project(), second);
    }
    for transaction in initial_transactions {
        assert!(
            repository
                .model()
                .applied_transaction(transaction)
                .is_some()
        );
    }
    drop(repository);
    let after = bytes(&context);
    assert!(
        forget(&context, &first.to_string(), false)
            .unwrap()
            .already_forgotten
    );
    assert_eq!(bytes(&context), after);
}

#[test]
fn p3_forget_preview_is_readonly_and_absence_does_not_initialize_a_store() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let read = context(&root, false);
    let id = Uuid::from_u128(1);
    assert!(
        forget(&read, &id.to_string(), true)
            .unwrap()
            .already_forgotten
    );
    assert!(!root.join("data").exists());
    assert!(!root.join("cache").exists());
    let write = context(&root, true);
    register(&write, id, &root.join("project"));
    let before = bytes(&write);
    let preview = forget(&read, &id.to_string(), true).unwrap();
    assert!(preview.dry_run && !preview.forgotten && !preview.already_forgotten);
    assert_eq!(preview.associations.len(), 2);
    assert_eq!(bytes(&write), before);
    assert!(forget(&read, &id.to_string(), false).is_err());
    assert_eq!(bytes(&write), before);
}

#[test]
fn p3_forget_rejects_missing_stable_lock_and_keeps_the_registration() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, true);
    let id = Uuid::from_u128(1);
    register(&context, id, &root.join("project"));
    let before = bytes(&context);
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let name = format!("project-{}.lock", id.hyphenated());
    nix::unistd::unlinkat(
        writer.build_handle(),
        name.as_str(),
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    )
    .unwrap();
    assert!(forget(&context, &id.to_string(), false).is_err());
    assert_eq!(bytes(&context), before);
    assert!(
        nix::sys::stat::fstatat(
            writer.build_handle(),
            name.as_str(),
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW
        )
        .is_err()
    );
}

#[test]
fn p3_forget_rejects_pending_and_damaged_commit_journals_without_changes() {
    for damaged in [false, true] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = context(&root, true);
        let id = Uuid::from_u128(1);
        register(&context, id, &root.join("project"));
        let before = bytes(&context);
        let repository = ProjectRepository::open_writer(&context).unwrap();
        let writer = repository.writer().unwrap();
        let name = format!("p2-lock-{}", id.simple());
        let record = if damaged {
            b"damaged journal".to_vec()
        } else {
            serde_json::to_vec(&serde_json::json!({
                "schema":1,"project":id.to_string(),"transaction":Uuid::from_u128(9).to_string(),
                "expected_revision":1,"expected_project":vec![0u8;32],"path":root.join("project/pakro.lock"),
                "parent":[0,0],"old_bytes":null,"old_identity":null,"new_bytes":[],
                "staged":null,"rollback_staged":null,"references":[]
            })).unwrap()
        };
        writer
            .control(ControlLimits::default())
            .unwrap()
            .replace(ControlNamespace::Journals, &name, None, &record)
            .unwrap();
        assert!(forget(&context, &id.to_string(), true).is_err());
        assert!(forget(&context, &id.to_string(), false).is_err());
        assert_eq!(bytes(&context), before);
        assert_eq!(
            writer
                .control(ControlLimits::default())
                .unwrap()
                .read(ControlNamespace::Journals, &name)
                .unwrap()
                .unwrap()
                .bytes(),
            record.as_slice()
        );
    }
}

#[test]
fn p3_forget_preserves_project_files_and_rejects_an_orphan_active_selection() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, true);
    let id = Uuid::from_u128(1);
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    for name in ["pakro.toml", "pakro.lock", "source.py"] {
        std::fs::write(project.join(name), name).unwrap();
    }
    register(&context, id, &project);
    forget(&context, &id.to_string(), false).unwrap();
    for name in ["pakro.toml", "pakro.lock", "source.py"] {
        assert_eq!(std::fs::read(project.join(name)).unwrap(), name.as_bytes());
    }
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let key = SelectionKey::new(id, "default", [1; 32], repository.model().limits()).unwrap();
    let expected = repository.model().snapshot(&key);
    repository
        .model_mut()
        .apply_language_selection(
            &key,
            &expected,
            &pakro_store::Digest::Sha256([7; 32]).to_string(),
            Uuid::now_v7(),
        )
        .unwrap();
    repository.commit_model().unwrap();
    let before = bytes(&context);
    assert!(forget(&context, &id.to_string(), true).is_err());
    assert!(forget(&context, &id.to_string(), false).is_err());
    assert_eq!(bytes(&context), before);
    for invalid in [
        "not-an-id",
        "00000000000000000000000000000001",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert!(forget(&context, invalid, false).is_err());
    }
}
