use super::*;
use crate::libraries::language::repository::tests::operation_context;
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
    let selected = select_observed(&context, &observed_b, true)
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
    assert!(
        select_observed(&context, &observed_missing, true)
            .unwrap()
            .is_none()
    );
    let projects = ProjectRepository::open_readonly(&context)
        .unwrap()
        .read_views()
        .registered_projects();
    assert_eq!(projects.len(), 2);
    assert!(!missing.join(".venv").exists());
    assert!(!missing.join("pakro.lock").exists());
}

#[tokio::test]
async fn f1_requires_python_blocks_language_only_selection() {
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
    assert!(select_observed(&context, &observed, true).is_err());
    assert!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .read_views()
            .registered_projects()
            .is_empty()
    );
}
