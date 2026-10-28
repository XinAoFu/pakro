use super::*;
use crate::libraries::language::repository::tests::operation_context;
use std::time::Duration;

#[test]
fn p6_source_recipe_query_facts_preserve_legacy_and_runtime_only_meanings() {
    let (_, record, _) = crate::libraries::language::lease::pin_tests::source_facts();
    let current = record.input().unwrap();
    for (recipe, expected) in [
        (1, "含CPython自身测试套件及测试扩展"),
        (2, "不含CPython自身测试套件及测试扩展"),
    ] {
        let input = current.for_test_recipe(recipe).unwrap();
        let before = input.record(record.output_sha256, record.runtime_target).unwrap();
        let input = before.input().unwrap();
        assert_eq!(
            source_runtime_configuration(&input),
            (recipe, expected.to_owned())
        );
        assert_eq!(
            input.record(record.output_sha256, record.runtime_target).unwrap(),
            before,
            "querying recipe facts must not reinterpret or mutate the stored source result"
        );
    }
}

#[test]
fn f4_query_is_readonly_and_corruption_is_not_empty() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, false, Duration::from_secs(10));
    assert!(inventory(&context).unwrap().is_empty());
    assert!(!root.join("data").exists());
    std::fs::create_dir_all(root.join("data/projects")).unwrap();
    std::fs::write(root.join("data/projects/foreign"), "broken").unwrap();
    assert!(inventory(&context).is_err());
}

#[tokio::test]
async fn f4_library_relations_survive_restart() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    use crate::libraries::project::repository::ProjectRepository;
    use crate::packages::python_environment::model::SelectionKey;
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let initial = operation_context(&root, true, Duration::from_secs(20));
    let mut policy = *initial.policy();
    policy.writes.project_selection = true;
    let context = capture_operation_context(OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let project = root.join("registered-project");
    std::fs::create_dir(&project).unwrap();
    let identity = uuid::Uuid::new_v4();
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let key = SelectionKey::new(
        identity,
        "default",
        spec.key().target(),
        repository.model().limits(),
    )
    .unwrap();
    let expected = repository.model().snapshot(&key);
    repository
        .model_mut()
        .register_project(identity, project.to_str().unwrap(), &"a".repeat(64))
        .unwrap();
    repository
        .model_mut()
        .apply_language_selection(
            &key,
            &expected,
            &crate::libraries::project::repository::language_digest(spec.key().fingerprint()),
            uuid::Uuid::new_v4(),
        )
        .unwrap();
    repository.commit_model().unwrap();
    drop(repository);
    let rows = inventory(&context).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].instance, spec.key().filename());
    assert!(rows[0].source_recipe.is_none() && rows[0].runtime_configuration.is_none());
    let encoded = serde_json::to_value(&rows[0]).unwrap();
    assert!(
        encoded.get("source_recipe").is_none() && encoded.get("runtime_configuration").is_none(),
        "prebuilt instances must not acquire source recipe facts"
    );
    assert!(
        rows[0]
            .users
            .iter()
            .any(|chain| chain.contains(project.to_str().unwrap()))
    );
    let projects = crate::programs::query::projects::projects(&context).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0].language.as_deref(),
        Some(spec.key().filename().as_str())
    );
    assert!(projects[0].accessible);
    std::fs::remove_dir(&project).unwrap();
    assert!(!crate::programs::query::projects::projects(&context).unwrap()[0].accessible);
    assert!(!inventory(&context).unwrap()[0].users.is_empty());
}

#[ignore = "explicit fixture staging for tests/e2e/cli/test_f4_library_query_and_uninstall"]
#[tokio::test]
async fn f4_e2e_stage_library() {
    use crate::libraries::language::repository::tests::{archive_for, install_spec_for, prepare};
    use crate::libraries::package::layouts::tests::{real_language, wheel};
    use crate::libraries::package::tree::prepare_package_tree;
    use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let root = std::path::PathBuf::from(
        std::env::var_os("PAKRO_F4_STAGE_DATA_ROOT").expect("PAKRO_F4_STAGE_DATA_ROOT is required"),
    );
    let root = root.canonicalize().unwrap_or_else(|_| {
        root.parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(root.file_name().unwrap())
    });
    let initial = operation_context(&root, true, Duration::from_secs(300));
    let context = capture_operation_context(OperationContextInputs {
        settings: initial.settings(),
        policy: *initial.policy(),
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.clone(),
        cache_root: root.join(".f4-stage-cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let platform = if cfg!(target_os = "macos") {
        RuntimePlatform::Macos
    } else {
        RuntimePlatform::LinuxGnu
    };
    let target = RuntimeArchiveTarget::new(std::env::consts::ARCH, platform).unwrap();
    for (build, variant) in [("20261013", "first"), ("20261014", "second")] {
        let bytes = archive_for("3.15.0", &target, variant);
        let spec = install_spec_for("3.15.0", &bytes, &target, build);
        drop(prepare(&spec, &context, &bytes).await.unwrap());
    }
    let language = real_language(&context).await;
    let (facts, archive) = wheel(
        &context,
        &language,
        "dep",
        [51; 32],
        vec![],
        vec![("dep.py", b"VALUE=1\n")],
        None,
    );
    let dependency = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let (facts, archive) = wheel(
        &context,
        &language,
        "parent",
        [52; 32],
        vec![[51; 32]],
        vec![("parent.py", b"import dep\n")],
        None,
    );
    let parent = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let (facts, archive) = wheel(
        &context,
        &language,
        "orphan",
        [53; 32],
        vec![],
        vec![("orphan.py", b"VALUE=2\n")],
        None,
    );
    let orphan = prepare_package_tree(&context, &language, facts, archive).unwrap();
    drop((dependency, parent, orphan, language));
    assert_eq!(inventory(&context).unwrap().len(), 6);
}

#[tokio::test]
async fn f4_current_damaged_payload_is_not_ready() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(20));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let executable = lease.entrypoint().to_owned();
    drop(lease);
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.15.0+20261013'\n",
    )
    .unwrap();
    let mut discovery = crate::config::FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let current = crate::programs::library::current_observed(&context, &observed)
        .unwrap()
        .unwrap();
    assert_eq!(current.requested, "3.15.0+20261013");
    assert_eq!(current.resolved_version.as_deref(), Some("3.15.0"));
    assert_eq!(current.status, "ready");
    std::fs::write(executable, b"damaged interpreter").unwrap();
    assert!(crate::programs::library::current_observed(&context, &observed).is_err());
}

#[tokio::test]
async fn f4_missing_committed_catalog_is_unknown_and_blocks_removal() {
    use crate::libraries::language::repository::{
        LanguageRepository,
        tests::{archive, install_spec, prepare},
    };
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    use crate::programs::query::snapshot::LanguageCandidate;
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let initial = operation_context(&root, true, Duration::from_secs(20));
    let mut policy = *initial.policy();
    policy.writes.project_selection = true;
    let context = capture_operation_context(OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let languages = LanguageRepository::open_readonly(&context).unwrap();
    let target = languages.enumerate_published().unwrap().remove(0);
    let project = root.join("still-registered-project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
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
    assert!(!inventory(&context).unwrap()[0].users.is_empty());
    let catalog = context
        .requested_data_root()
        .join("projects/.control/catalog/projects");
    std::fs::rename(&catalog, root.join("saved-projects-catalog")).unwrap();
    // Exercise both paths before asserting, so the RED shows whether removal
    // actually treats the same lost authority as an empty reference set.
    let query = inventory(&context);
    let removal = crate::libraries::language::removal::remove(&context, &target);
    let payload_survived = target.entrypoint().is_file();
    assert!(
        query.is_err(),
        "missing committed catalog was treated as empty; removal_error={}, payload_survived={payload_survived}",
        removal.is_err()
    );
    assert!(
        removal.is_err(),
        "loss of the sole reference authority must block deletion"
    );
    assert!(payload_survived, "referenced payload must remain unchanged");
    assert!(languages.acquire_by_key(&target.key()).unwrap().is_some());
}
