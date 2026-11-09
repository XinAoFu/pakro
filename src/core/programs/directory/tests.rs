use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::operation_context;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ObservedProjectInputs, ReadPolicy, observe_project};

fn observe(root: &std::path::Path) -> ObservedProjectInputs {
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_owned()];
    observe_project(
        root,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(120)),
    )
    .unwrap()
}
fn context(root: &std::path::Path) -> OperationContext {
    let initial = operation_context(root, true, Duration::from_secs(120));
    let mut policy = *initial.policy();
    policy.writes.project_selection = true;
    capture_operation_context(OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: Some(crate::programs::python::public_sources().unwrap()),
        python_target_description: None,
    })
    .unwrap()
}

#[test]
fn f1_nearest_local_declaration_wins() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let child = root.join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.11.14'\n").unwrap();
    std::fs::write(child.join("pakro.toml"), "[tools]\npython='3.12.12'\n").unwrap();
    std::fs::write(
        child.join("pakro.local.toml"),
        "[tools]\npython='3.13.16'\n",
    )
    .unwrap();
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_owned()];
    let observed = observe_project(
        &child,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(20)),
    )
    .unwrap();
    assert_eq!(
        crate::programs::query::snapshot::effective_python_declaration(&observed).as_deref(),
        Some("3.13.16")
    );
}

#[tokio::test]
async fn f3_new_project_reuses_ready_layout_and_missing_never_commits() {
    use crate::libraries::package::layouts::{
        prepare_layout,
        tests::{real_language, wheel},
    };
    use crate::packages::python_environment::model::{
        PackageGraphNode, PackageGraphRecord, PackageSourcePin,
    };
    use crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint;
    use crate::packages::python_resolution::lock_model::{
        ContentPin, PinEvidence, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    };
    use crate::programs::shared::commit::{
        commit_package_selection, prepare_package_selection_commit,
    };
    use pakro_index::python_source::PythonRegistry;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let context = context(&stage);
    let language = real_language(&context).await;
    let (mut facts, archive) = wheel(
        &context,
        &language,
        "demo",
        [61; 32],
        vec![],
        vec![("demo.py", b"VALUE='from-shared-tree'\n")],
        None,
    );
    let registry = PythonRegistry::new("pypi", "https://pypi.org/simple/", None).unwrap();
    let base = canonical_registry_base_fingerprint(registry.canonical_base());
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: base,
            project: "demo",
            version: "1.0",
            filename: archive.filename(),
            kind: RegistryArtifactKind::Wheel,
        },
        Default::default(),
    )
    .unwrap();
    let pin = ContentPin::new(
        &file,
        archive.digest(),
        archive.size(),
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap();
    facts.source = hex::encode(pin.id().as_bytes());
    let graph = PackageGraphRecord {
        roots: vec![0],
        nodes: vec![PackageGraphNode {
            name: "demo".into(),
            version: "1.0".into(),
            instance: facts.key,
            source_pin: facts.source.clone(),
            artifact: facts.artifact.clone(),
            pin: PackageSourcePin {
                registry: "pypi".into(),
                base,
                filename: archive.filename().into(),
                size: archive.size(),
                advertised: true,
                build: None,
                conda: None,
            },
        }],
        edges: vec![],
    };
    let layout = prepare_layout(
        &context,
        &language,
        graph.fingerprint().unwrap(),
        vec![(facts, archive)],
    )
    .unwrap();
    let bin = layout.bin();
    let key = layout.key();
    let language_key = language.key();
    drop(layout);
    drop(language);
    let a = root.join("a");
    let b = root.join("b");
    let missing = root.join("missing");
    for project in [&a, &b, &missing] {
        std::fs::create_dir(project).unwrap();
    }
    let declaration = "[tools]\npython='3.13.16'\n[packages.\"python@3.13.16\"]\ndemo='1.0'\n";
    std::fs::write(a.join("pakro.toml"), declaration).unwrap();
    std::fs::write(b.join("pakro.toml"), declaration).unwrap();
    std::fs::write(
        missing.join("pakro.toml"),
        declaration.replace("1.0", "2.0"),
    )
    .unwrap();
    let observed_a = observe(&a);
    commit_package_selection(
        prepare_package_selection_commit(&context, &observed_a, language_key, key, graph).unwrap(),
    )
    .unwrap();
    drop(observed_a);
    let observed_b = observe(&b);
    let selected = apply_observed(&context, &observed_b)
        .unwrap()
        .into_ready()
        .unwrap()
        .expect("same exact local graph should select");
    assert_eq!(selected.bin_dir, bin);
    let output = std::process::Command::new(selected.executable)
        .args(["-c", "import demo; print(demo.VALUE)"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "from-shared-tree"
    );
    let observed_missing = observe(&missing);
    assert!(matches!(
        apply_observed(&context, &observed_missing).unwrap(),
        PythonQueryReport::MissingPackages { .. }
    ));
    let projects = ProjectRepository::open_readonly(&context)
        .unwrap()
        .read_views()
        .registered_projects();
    assert_eq!(projects.len(), 2);
    assert!(!missing.join(".venv").exists());
    assert!(!missing.join("pakro.lock").exists());
}

#[tokio::test]
async fn p3_user_pyproject_requires_python_does_not_block_pakro_selection() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        "[project]\nname='demo'\nversion='1.0'\nrequires-python='>=4'\n",
    )
    .unwrap();
    let observed = observe(&project);
    let selected = apply_observed(&context, &observed).unwrap();
    assert!(matches!(selected, PythonQueryReport::Ready(_)));
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .read_views()
            .registered_projects()
            .len()
            == 1
    );
}

#[tokio::test]
async fn p1_explicit_targets_and_language_only_do_not_borrow_current_packages() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    use crate::programs::query::environment::query_observed;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let language = prepare(&spec, &context, &bytes).await.unwrap();
    let language_root = language.root().to_owned();
    drop(language);
    let observed = observe(&project);
    let query = |target: Option<&str>, scope| PythonQueryRequest {
        target: target.map(str::to_owned),
        scope,
    };
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(None, PythonQueryScope::Environment)
        )
        .unwrap()
        .report,
        PythonQueryReport::NoDeclaration
    ));
    let selected = query_observed(
        &context,
        &observed,
        &query(Some("3.15"), PythonQueryScope::Environment),
    )
    .unwrap()
    .report
    .into_ready()
    .unwrap()
    .unwrap();
    assert_eq!(selected.root, language_root);
    drop(observed);
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.15.0'\n[packages.\"python@3.15.0\"]\ndemo='1.0'\n",
    )
    .unwrap();
    let observed = observe(&project);
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(None, PythonQueryScope::Language)
        )
        .unwrap()
        .report,
        PythonQueryReport::Ready(_)
    ));
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(None, PythonQueryScope::Environment)
        )
        .unwrap()
        .report,
        PythonQueryReport::MissingPackages { .. }
    ));
    let current = crate::programs::library::current_observed(&context, &observed)
        .unwrap()
        .unwrap();
    assert_eq!(current.status, "missing-packages");
    assert_eq!(current.path.as_deref(), Some(language_root.as_path()));
    assert!(current.instance.is_some());
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(Some("3.14"), PythonQueryScope::Environment)
        )
        .unwrap()
        .report,
        PythonQueryReport::MissingLanguage { .. }
    ));
    drop(observed);
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.14.0'\n[packages.\"python@3.14.0\"]\ndemo='1.0'\n",
    )
    .unwrap();
    let observed = observe(&project);
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(Some("3.15"), PythonQueryScope::Language)
        )
        .unwrap()
        .report,
        PythonQueryReport::Ready(_)
    ));
    assert!(matches!(
        query_observed(
            &context,
            &observed,
            &query(Some("3.15"), PythonQueryScope::Environment)
        )
        .unwrap()
        .report,
        PythonQueryReport::IncompatibleTarget { .. }
    ));
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .read_views()
            .registered_projects()
            .is_empty()
    );
    assert!(!stage.join("data/projects").exists());
}

#[test]
fn p1_stale_and_expired_observation_errors_are_not_empty_selections() {
    use crate::programs::query::environment::query_observed;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = context(&stage);
    let observed = observe(&project);
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.14.0'\n").unwrap();
    let request = PythonQueryRequest {
        target: None,
        scope: PythonQueryScope::Environment,
    };
    assert!(
        query_observed(&context, &observed, &request)
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert!(apply_observed(&context, &observed).is_err());
    let observed = observe(&project);
    let expired = operation_context(&stage, false, Duration::from_millis(5));
    std::thread::sleep(Duration::from_millis(6));
    assert!(query_observed(&expired, &observed, &request).is_err());
    assert!(!stage.join("data").exists());
}

#[path = "resource_baseline.rs"]
mod resource_baseline;

#[tokio::test]
async fn p1_already_applied_selection_rechecks_inputs_and_cancellation() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    use crate::programs::query::environment::query_observed;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    drop(
        prepare(&install_spec(&bytes), &context, &bytes)
            .await
            .unwrap(),
    );
    let declaration = "[tools]\npython='3.15.0'\n";
    std::fs::write(project.join("pakro.toml"), declaration).unwrap();
    let observed = observe(&project);
    assert!(matches!(
        apply_observed(&context, &observed).unwrap(),
        PythonQueryReport::Ready(_)
    ));
    let request = PythonQueryRequest {
        target: None,
        scope: PythonQueryScope::Environment,
    };
    let state = ExpectedProjectState::capture(
        &context,
        &ProjectIdentity::from_observation(&observed).unwrap(),
    )
    .unwrap();
    let selected = query_observed(&context, &observed, &request).unwrap();
    assert!(selected.already_applied);
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.14.0'\n").unwrap();
    assert!(
        apply_selection(&context, &observed, selected, &state).is_err(),
        "changed inputs must not emit an already-applied success"
    );
    drop(observed);
    std::fs::write(project.join("pakro.toml"), declaration).unwrap();
    let observed = observe(&project);
    let state = ExpectedProjectState::capture(
        &context,
        &ProjectIdentity::from_observation(&observed).unwrap(),
    )
    .unwrap();
    let selected = query_observed(&context, &observed, &request).unwrap();
    assert!(selected.already_applied);
    context.cancellation_token().cancel();
    assert!(
        apply_selection(&context, &observed, selected, &state).is_err(),
        "cancelled operations must not emit an already-applied success"
    );
}

#[path = "resource_comparison.rs"]
mod resource_comparison;

#[tokio::test]
async fn fixed_lock_install_directory_selection_still_rejects_lock_change() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    use crate::programs::query::environment::query_observed;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    drop(
        prepare(&install_spec(&bytes), &context, &bytes)
            .await
            .unwrap(),
    );
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    std::fs::write(project.join("pakro.lock"), "lockfile_version=4\n").unwrap();
    let observed = observe(&project);
    assert!(matches!(
        apply_observed(&context, &observed).unwrap(),
        PythonQueryReport::Ready(_)
    ));
    let request = PythonQueryRequest {
        target: None,
        scope: PythonQueryScope::Environment,
    };
    let state = ExpectedProjectState::capture(
        &context,
        &ProjectIdentity::from_observation(&observed).unwrap(),
    )
    .unwrap();
    let selected = query_observed(&context, &observed, &request).unwrap();
    assert!(selected.already_applied);
    std::fs::write(project.join("pakro.lock"), "lockfile_version=5\n").unwrap();
    assert!(
        apply_selection(&context, &observed, selected, &state).is_err(),
        "directory selection keeps its original lock-change semantics"
    );
}
