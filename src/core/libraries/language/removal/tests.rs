use super::*;
use crate::libraries::language::repository::{LanguageRepository, tests::*};
use std::time::Duration;

#[tokio::test]
async fn f4_exact_unreferenced_target_only_is_removed() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(20));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    let target = repository.enumerate_published().unwrap().remove(0);
    remove(&context, &target).unwrap();
    assert!(repository.enumerate_published().unwrap().is_empty());
    assert!(
        root.join("data/languages/.leases/build")
            .join(format!("{}.lock", spec.key().filename()))
            .is_file()
    );
}

#[tokio::test]
async fn f4_new_reference_and_live_lease_block_removal() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(20));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    let target = repository.enumerate_published().unwrap().remove(0);
    let error = remove(&context, &target).unwrap_err().to_string();
    assert!(error.contains("in use"), "{error}");
    lease.check_binding().unwrap();
}

#[tokio::test]
async fn f4_reference_added_after_query_blocks_removal() {
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    use crate::programs::query::snapshot::LanguageCandidate;
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let original = operation_context(&root, true, Duration::from_secs(20));
    let mut policy = *original.policy();
    policy.writes.project_selection = true;
    let context = capture_operation_context(OperationContextInputs {
        settings: original.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: original.requested_data_root().to_owned(),
        cache_root: original.requested_cache_root().to_owned(),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    let target = repository.enumerate_published().unwrap().remove(0);
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython = \"3.15.0\"\n").unwrap();
    let mut discovery = crate::config::FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let candidate = LanguageCandidate::new(
        target.key(),
        target.payload_root().to_owned(),
        target.entrypoint().to_owned(),
        target.version(),
    );
    crate::programs::install::selection::apply_directory_selection(&context, &observed, &candidate)
        .unwrap();
    let error = remove(&context, &target).unwrap_err().to_string();
    assert!(
        error.contains("project") && error.contains("referenced"),
        "{error}"
    );
    assert!(repository.acquire_by_key(&target.key()).unwrap().is_some());
    std::fs::remove_dir_all(&project).unwrap();
    assert!(
        remove(&context, &target)
            .unwrap_err()
            .to_string()
            .contains("referenced")
    );
}

#[tokio::test]
async fn f4_unknown_or_partial_delete_is_not_success() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(20));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    let target = repository.enumerate_published().unwrap().remove(0);
    std::fs::write(
        target.payload_root().join("untracked-user-file"),
        b"preserve",
    )
    .unwrap();
    assert!(remove(&context, &target).is_err());
    assert_eq!(
        std::fs::read(target.payload_root().join("untracked-user-file")).unwrap(),
        b"preserve"
    );
    assert_eq!(repository.enumerate_published().unwrap().len(), 1);
}

#[tokio::test]
async fn f4_partial_delete_revokes_ready() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(20));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    let target = repository.enumerate_published().unwrap().remove(0);
    let permit = acquire(&context, &target).unwrap();
    let count = std::cell::Cell::new(0);
    let result = capture_and_remove(permit, || {
        count.set(count.get() + 1);
        eyre::ensure!(count.get() < 2, "injected second unlink failure");
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("injected"));
    assert_eq!(count.get(), 2);
    assert!(repository.enumerate_published().unwrap().is_empty());
    assert!(repository.acquire_by_key(&target.key()).unwrap().is_none());
    assert!(
        root.join("data/languages/.leases/build")
            .join(format!("{}.lock", spec.key().filename()))
            .is_file()
    );
}
