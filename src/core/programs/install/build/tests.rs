//! The dynamic solve must reuse the exact source-built input already observed
//! by the backend, including a legitimate non-reproducible generated wheel.
use super::*;
use crate::packages::python_resolution::candidate_policy::Sdists;
use crate::packages::python_resolution::source::registry_depot::ProjectRegistryFixture;
use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "native_tests.rs"]
mod native;

#[tokio::test(flavor = "current_thread")]
async fn p6_registry_loop_keeps_one_complete_language_entry_check() {
    use crate::libraries::language::lease::full_inventory_checks;
    use crate::libraries::language::repository::tests::{archive, install_spec, operation_context, prepare};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(30));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes).await.unwrap();
    let fixture = ProjectRegistryFixture::wheels();
    let root = BoundPythonLock::acquire(&stage.join("pakro.lock"), Default::default(), context.policy().deadline).unwrap();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    let mut budget = BuildBudget::new();
    // This future remains on the current test thread; setup's blocking work
    // is deliberately outside the measured inventory counter interval.
    let before = full_inventory_checks();
    let plan = prepare_registry(&context, &language, &root, vec!["demo-pkg==1.0".into()],
        registry.clone(), &mut budget, 0).await.unwrap();
    let closed = close_registry_plan(plan, Default::default()).unwrap();
    assert_eq!(closed.plan().selected.len(), 2);
    assert_eq!(full_inventory_checks(), before + 1, "registry entry must fully verify Python once; its typed-held state loop must not rewalk it");
    drop(closed);
    std::fs::write(language.root().join("lib/python3.15/stdlib-marker"), b"changed").unwrap();
    let requests = fixture.requests().len();
    let before = full_inventory_checks();
    let mut budget = BuildBudget::new();
    assert!(prepare_registry(&context, &language, &root, vec!["demo-pkg==1.0".into()],
        registry, &mut budget, 0).await.is_err());
    assert_eq!(full_inventory_checks(), before + 1);
    assert_eq!(fixture.requests().len(), requests, "damaged runtime must fail before new registry work");
}

#[tokio::test]
async fn p3_nonce_installed_output_survives_unrelated_update_without_zip_cache_or_hooks() {
    const TEST: &str = "programs::install::build::tests::p3_nonce_installed_output_survives_unrelated_update_without_zip_cache_or_hooks";
    const CHILD: &str = "PAKRO_TEST_P3_NONCE_REUSE_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1");
        for variable in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(variable);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "nonce published-output fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = nonce_installed_output_fixture();
    eprintln!(
        "P3_NONCE_PHASE=fixture future_bytes={}",
        std::mem::size_of_val(&fixture)
    );
    fixture.boxed_local().await;
}

async fn nonce_installed_output_fixture() {
    use crate::config::FrozenConfigDiscovery;
    use crate::libraries::project::{
        identity::ProjectIdentity,
        repository::ProjectRepository,
        snapshot::{ReadPolicy, observe_project},
    };
    use crate::programs::shared::python_roots::PythonLockIntent;
    use crate::programs::*;
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let (fixture, _, _) = ProjectRegistryFixture::nonce_source_with_optional_runtime_dep();
    let registry = (*fixture.endpoint().project.registry).clone();
    let mut settings = crate::config::Settings::default();
    settings.http_timeout = "30s".into();
    settings.http_download_timeout = "300s".into();
    settings.python.github_attestations = Some(false);
    let context = capture_operation_context(OperationContextInputs {
        settings: &settings,
        policy: OperationPolicy {
            locked: false,
            offline: false,
            deadline: std::time::Instant::now() + Duration::from_secs(300),
            budget: RequestBudget {
                max_arguments: 16,
                max_total_argument_bytes: 4096,
                max_declarations: 32,
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
        data_root: stage.join("data"),
        cache_root: stage.join("cache"),
        python_sources: Some(PythonSourceInputs {
            registries: vec![registry],
            policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap()),
        }),
        python_target_description: None,
    })
    .unwrap();
    nonce_phase("first-runtime");
    let language = crate::libraries::package::layouts::tests::real_language(&context)
        .boxed_local()
        .await;
    let language_key = language.key();
    let language_version = language.lock_version().unwrap();
    // Separate install calls model separate CLI invocations. Preserve the
    // identity description, not an extra test-only runtime lease across them.
    drop(language);
    let raw_project = fixture.project_path().parent().unwrap().to_owned();
    let project = std::fs::canonicalize(&raw_project).unwrap();
    eprintln!(
        "P3_NONCE_PROJECT raw={} canonical={}",
        raw_project.display(),
        project.display()
    );
    let declaration = format!(
        "[tools]\npython='{}'\n[packages.\"python@{}\"]\ndemo-pkg='1.0'\n",
        language_version, language_version
    );
    std::fs::write(project.join("pakro.toml"), &declaration).unwrap();
    let observe = || {
        let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
        observe_project(
            &project,
            ReadPolicy::new(discovery, context.policy().deadline),
        )
        .unwrap()
    };
    let config = crate::config::Config::load_from_config_files(Default::default(), false)
        .boxed_local()
        .await
        .unwrap();
    nonce_phase("first-install");
    crate::programs::python::project_plan(context.clone(), observe(), &[])
        .unwrap()
        .unwrap()
        .install(config.clone())
        .await
        .unwrap();
    let first = fixture.lock_at_deadline(context.policy().deadline);
    let model = first.model().unwrap();
    let source = model.graphs()[0]
        .nodes()
        .iter()
        .find(|node| node.name() == "demo-pkg")
        .unwrap()
        .source_pin();
    let expected = first.source_build(source).unwrap().clone();
    let artifact_cache = context.requested_cache_root().join("shared-python");
    let mut deleted = 0;
    for (_, record) in model.builds() {
        let path = artifact_cache
            .join("artifacts")
            .join(&record.output_digest)
            .join(&record.output_filename);
        std::fs::remove_file(path).unwrap();
        deleted += 1;
    }
    assert_eq!(
        deleted, 2,
        "the original parent and recursive nonce wheel archives must both have been preserved before this deliberate cache eviction"
    );
    let observed = observe();
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let graph = repository
        .read_views()
        .package_graph(&identity)
        .unwrap()
        .unwrap();
    let old_node = graph
        .nodes()
        .iter()
        .find(|node| node.name == "demo-pkg")
        .unwrap()
        .clone();
    let old_package =
        crate::libraries::package::lease::acquire_package(&context, old_node.instance)
            .unwrap()
            .unwrap();
    old_package.check_binding().unwrap();
    let old_manifest = serde_json::to_vec(old_package.manifest()).unwrap();
    let old_root = old_package.root().to_owned();
    drop(old_package);
    drop(repository);
    drop(observed);
    drop(first);
    let hooks = crate::packages::python_source_build::observed_hook_calls();
    assert!(
        hooks > 0,
        "the original nonce output must come from actual backend execution"
    );
    std::fs::write(
        project.join("pakro.toml"),
        format!("{declaration}helper='1.0'\n"),
    )
    .unwrap();
    nonce_phase("update-unrelated-wheel");
    crate::programs::python::project_plan_with_intent(
        context.clone(),
        observe(),
        &[],
        PythonLockIntent::Update,
    )
    .unwrap()
    .unwrap()
    .install(config.clone())
    .await
    .unwrap();
    assert_eq!(
        crate::packages::python_source_build::observed_hook_calls(),
        hooks,
        "an unrelated wheel must not execute any old source hook"
    );
    let root = fixture.lock_at_deadline(context.policy().deadline);
    assert_eq!(root.source_build(source), Some(&expected));
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let graph = repository
        .read_views()
        .package_graph(&identity)
        .unwrap()
        .unwrap();
    let actual = graph
        .nodes()
        .iter()
        .find(|node| node.name == "demo-pkg")
        .unwrap();
    assert_eq!(actual, &old_node);
    assert!(graph.nodes().iter().any(|node| node.name == "helper"));
    let layout = crate::libraries::package::lease::acquire_layout(&context, *graph.layout())
        .unwrap()
        .unwrap();
    let output = std::process::Command::new(layout.bin().join("python")).args(["-B", "-c", "import demo_pkg,helper; assert len(demo_pkg.VALUE)==32 and helper.VALUE==42; print('nonce-ready-output-reused')"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "nonce-ready-output-reused"
    );
    let old_package =
        crate::libraries::package::lease::acquire_package(&context, old_node.instance)
            .unwrap()
            .unwrap();
    old_package.check_binding().unwrap();
    assert_eq!(
        serde_json::to_vec(old_package.manifest()).unwrap(),
        old_manifest,
        "unrelated update must preserve the complete original package manifest and physical inventory"
    );
    drop(old_package);
    drop(layout);
    drop(repository);
    drop(root);
    // Activating S's own extra changes its graph key/dependency references.
    // Its original nonce wheel must still be reused and independently placed.
    let extra_declaration = declaration.replace(
        "demo-pkg='1.0'",
        "demo-pkg={version='1.0',extras=['feature']}",
    );
    std::fs::write(
        project.join("pakro.toml"),
        format!("{extra_declaration}helper='1.0'\n"),
    )
    .unwrap();
    nonce_phase("update-source-extra");
    crate::programs::python::project_plan_with_intent(
        context.clone(),
        observe(),
        &[],
        PythonLockIntent::Update,
    )
    .unwrap()
    .unwrap()
    .install(config.clone())
    .await
    .unwrap();
    assert_eq!(
        crate::packages::python_source_build::observed_hook_calls(),
        hooks,
        "changing the source package's runtime closure must not rebuild its nonce wheel"
    );
    let root = fixture.lock_at_deadline(context.policy().deadline);
    assert_eq!(root.source_build(source), Some(&expected));
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let graph = repository
        .read_views()
        .package_graph(&identity)
        .unwrap()
        .unwrap();
    let derived = graph
        .nodes()
        .iter()
        .find(|node| node.name == "demo-pkg")
        .unwrap();
    let helper = graph
        .nodes()
        .iter()
        .find(|node| node.name == "helper")
        .unwrap();
    assert_ne!(derived.instance, old_node.instance);
    assert_eq!(derived.artifact, old_node.artifact);
    assert_eq!(derived.source_pin, old_node.source_pin);
    let derived_package =
        crate::libraries::package::lease::acquire_package(&context, derived.instance)
            .unwrap()
            .unwrap();
    assert_eq!(
        derived_package.manifest().facts.dependencies,
        [helper.instance]
    );
    derived_package.check_binding().unwrap();
    let old_package =
        crate::libraries::package::lease::acquire_package(&context, old_node.instance)
            .unwrap()
            .unwrap();
    old_package.check_binding().unwrap();
    assert_eq!(
        serde_json::to_vec(old_package.manifest()).unwrap(),
        old_manifest,
        "derived output must not change its original Ready source tree"
    );
    use std::os::unix::fs::MetadataExt;
    let old_payload =
        std::fs::metadata(old_package.site_packages().join("demo_pkg/__init__.py")).unwrap();
    let new_payload =
        std::fs::metadata(derived_package.site_packages().join("demo_pkg/__init__.py")).unwrap();
    assert_ne!(
        (old_payload.dev(), old_payload.ino()),
        (new_payload.dev(), new_payload.ino())
    );
    let script = std::fs::read_to_string(derived_package.root().join("bin/demo_pkg-src")).unwrap();
    assert!(!script.contains(old_root.to_str().unwrap()));
    assert!(script.contains(derived_package.root().to_str().unwrap()));
    drop(old_package);
    let layout = crate::libraries::package::lease::acquire_layout(&context, *graph.layout())
        .unwrap()
        .unwrap();
    let output = std::process::Command::new(layout.bin().join("demo_pkg-src"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim().len(), 32);
    let output = std::process::Command::new(layout.bin().join("python")).args(["-B", "-c", "import csv,hashlib,base64,importlib.metadata,pathlib; d=importlib.metadata.distribution('demo-pkg'); row=next(r for r in csv.reader(d.read_text('RECORD').splitlines()) if r[0]=='../bin/demo_pkg-src'); p=pathlib.Path(d.locate_file(row[0])); data=p.read_bytes(); assert row[1]=='sha256='+base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b'=').decode() and int(row[2])==len(data); print('derived-record-correct')"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "derived-record-correct"
    );
    let bytes = std::fs::read(project.join("pakro.lock")).unwrap();
    let catalog = std::fs::read(
        context
            .requested_data_root()
            .join("projects/.control/catalog/projects"),
    )
    .unwrap();
    let derived_manifest = serde_json::to_vec(derived_package.manifest()).unwrap();
    let derived_key = derived_package.manifest().key();
    drop(derived_package);
    drop(layout);
    drop(repository);
    // A separate empty output/package store must still use exact original
    // build inputs and reject a genuinely different nonce reconstruction.
    let cold_cache = stage.join("cold-artifacts");
    let cold_scratch = stage.join("cold-scratch");
    std::fs::create_dir_all(&cold_scratch).unwrap();
    let cold_context = capture_operation_context(OperationContextInputs {
        settings: context.settings(),
        policy: *context.policy(),
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: stage.join("cold-data"),
        cache_root: stage.join("cold-cache"),
        python_sources: Some(PythonSourceInputs {
            registries: vec![(*fixture.endpoint().project.registry).clone()],
            policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap()),
        }),
        python_target_description: None,
    })
    .unwrap();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    let language =
        crate::libraries::language::repository::LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_by_key(&language_key)
            .unwrap()
            .unwrap();
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.deadline = context.policy().deadline;
    registry.cache_root = &cold_cache;
    registry.scratch_root = &cold_scratch;
    let mut retained_registry = registry.clone();
    retained_registry.cache_root = &artifact_cache;
    let mut retained_budget = BuildBudget::new();
    nonce_phase("strict-ready");
    let retained = replay_registry(
        &context,
        &language,
        &root,
        vec!["demo-pkg[feature]==1.0".into(), "helper==1.0".into()],
        retained_registry,
        &mut retained_budget,
        0,
    )
    .await
    .unwrap();
    let retained = close_registry_plan(retained, Default::default()).unwrap();
    let (retained_sources, retained_proof) = retained.into_shared_package_sources().unwrap();
    assert!(retained_sources.iter().any(|source| matches!(source,
        crate::packages::python_resolution::source::registry_depot::closed::SharedRegistryPackageSource::Existing(package)
            if package.manifest().key() == derived_key)));
    assert_eq!(retained_budget.builds, 0);
    assert_eq!(
        retained_budget.policy_work, 0,
        "strict Ready reuse must not enumerate candidates"
    );
    assert_eq!(
        crate::packages::python_source_build::observed_hook_calls(),
        hooks
    );
    retained_proof
        .recheck(tokio::time::Instant::from_std(context.policy().deadline))
        .unwrap();
    drop(retained_sources);
    drop(retained_proof);
    let mut budget = BuildBudget::new();
    nonce_phase("cold-output");
    let error = match prepare_registry(
        &cold_context,
        &language,
        &root,
        vec!["demo-pkg[feature]==1.0".into(), "helper==1.0".into()],
        registry,
        &mut budget,
        0,
    )
    .await
    {
        Ok(_) => panic!("missing old output must not accept a changed nonce rebuild"),
        Err(error) => error,
    };
    assert!(
        error.chain().any(|cause| cause.to_string()
            == "strict source rebuilt output differs from the locked artifact"),
        "wrong cold failure: {error:#}"
    );
    assert!(crate::packages::python_source_build::observed_hook_calls() > hooks);
    assert!(budget.active.is_empty());
    assert_eq!(budget.workspace_reserved, 0);
    assert_eq!(std::fs::read(project.join("pakro.lock")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(
            context
                .requested_data_root()
                .join("projects/.control/catalog/projects")
        )
        .unwrap(),
        catalog
    );
    drop(root);
    drop(language);
    // Corrupt Ready content is an error, not a cache miss or rebuild request.
    let derived_package = crate::libraries::package::lease::acquire_package(&context, derived_key)
        .unwrap()
        .unwrap();
    derived_package.check_binding().unwrap();
    assert_eq!(
        serde_json::to_vec(derived_package.manifest()).unwrap(),
        derived_manifest
    );
    let metadata = derived_package
        .manifest()
        .storage
        .inventory
        .entries()
        .iter()
        .find(|entry| {
            entry
                .relative
                .file_name()
                .is_some_and(|name| name == "METADATA")
        })
        .unwrap()
        .relative
        .clone();
    std::fs::write(
        derived_package.root().join(metadata),
        b"corrupted retained metadata\n",
    )
    .unwrap();
    drop(derived_package);
    let before = crate::packages::python_source_build::observed_hook_calls();
    nonce_phase("corrupt-ready");
    let rejected = match crate::programs::python::project_plan_with_intent(
        context.clone(),
        observe(),
        &[],
        PythonLockIntent::Update,
    ) {
        Ok(Some(plan)) => plan.install(config).await,
        Ok(None) => panic!("the damaged fixture lost its declared Python project"),
        Err(error) => Err(error),
    };
    let error = rejected.unwrap_err();
    assert!(
        error.chain().any(|cause| matches!(
            cause.to_string().as_str(),
            "published package inventory changed"
                | "published wheel METADATA bytes differ from their immutable inventory"
        )),
        "Ready damage must reach a genuine content-verification error: {error:#}"
    );
    assert_eq!(
        crate::packages::python_source_build::observed_hook_calls(),
        before
    );
    assert_eq!(std::fs::read(project.join("pakro.lock")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(
            context
                .requested_data_root()
                .join("projects/.control/catalog/projects")
        )
        .unwrap(),
        catalog
    );
}

fn nonce_phase(label: &str) {
    eprintln!(
        "P3_NONCE_PHASE={label} open_fds={:?}",
        std::fs::read_dir("/dev/fd").map(|entries| entries.count())
    );
}

fn changed_runtime_model(
    model: &crate::packages::python_resolution::lock_model::PythonLockModel,
    change_target: bool,
) -> crate::packages::python_resolution::lock_model::PythonLockModel {
    use crate::packages::python_resolution::lock_model::{
        EdgeInput, GroupKey, LockGraphInput, NodeInput, PythonLockModel, RootInput, RuntimePin,
    };
    assert_eq!(model.graphs().len(), 1);
    let graph = &model.graphs()[0];
    let original = graph.runtime();
    let mut content = *original.content_tree();
    let mut target = *original.target();
    if change_target {
        target[0] ^= 1;
    } else {
        content[0] ^= 1;
    }
    let key = GroupKey::new(
        graph.key().config_scope(),
        graph.key().group(),
        target,
        Default::default(),
    )
    .unwrap();
    let declarations = graph
        .root_declarations()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let roots = graph
        .roots()
        .iter()
        .map(|root| RootInput {
            declaration_index: root.declaration_index(),
            node: root.node(),
        })
        .collect::<Vec<_>>();
    let nodes = graph
        .nodes()
        .iter()
        .map(|node| NodeInput {
            name: node.name(),
            version: node.version(),
            source_pin: node.source_pin(),
        })
        .collect::<Vec<_>>();
    let edges = graph
        .edges()
        .iter()
        .map(|edge| EdgeInput {
            from: edge.from(),
            to: edge.to(),
            declaration: edge.declaration(),
        })
        .collect::<Vec<_>>();
    PythonLockModel::from_parts_with_builds(
        model.pins(),
        &[LockGraphInput {
            key: &key,
            semantics: graph.semantics(),
            runtime: RuntimePin::new(content, original.recipe(), target).unwrap(),
            root_declarations: &declarations,
            roots: &roots,
            nodes: &nodes,
            edges: &edges,
        }],
        model.builds(),
        Default::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3_update_reuses_original_source_inputs_only_for_the_same_runtime_and_target() {
    use crate::packages::python_environment::model::{
        PackageGraphNode, PackageGraphRecord, PackageSourcePin,
    };
    use crate::packages::python_resolution::source::registry_depot::RegistryDepotError;
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let (fixture, _, _) = ProjectRegistryFixture::deterministic_source_dependency();
    let root = fixture.lock_at_deadline(context.policy().deadline);
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.candidate_policy.sdists = Sdists::Include;
    registry.deadline = context.policy().deadline;
    let declarations = vec!["demo-pkg==1.0".to_owned()];
    let mut first_budget = BuildBudget::new();
    let first = prepare_registry(
        &context,
        &language,
        &root,
        declarations.clone(),
        registry.clone(),
        &mut first_budget,
        0,
    )
    .await
    .unwrap();
    let closed = close_registry_plan(first, Default::default()).unwrap();
    assert_eq!(closed.plan().selected.len(), 1);
    let node = &closed.plan().selected[0];
    let source = closed.owner(node.key).unwrap();
    let pin = source.pin().clone();
    let expected = source.build_record().unwrap().clone();
    let projection = PackageGraphRecord {
        roots: vec![0],
        nodes: vec![PackageGraphNode {
            name: node.name.clone(),
            version: node.version.clone(),
            instance: [0; 32],
            source_pin: hex::encode(pin.id().as_bytes()),
            artifact: source.output_digest().to_string(),
            pin: PackageSourcePin {
                registry: pin.file().registry_id().into(),
                base: *pin.file().canonical_base_fingerprint(),
                filename: pin.file().filename().into(),
                size: pin.size(),
                advertised: matches!(pin.evidence(), PinEvidence::Advertised),
                build: Some(expected.clone()),
            },
        }],
        edges: vec![],
    };
    let model = super::super::lock::project_lock_model(
        &root,
        &language,
        &projection,
        &[("demo-pkg".into(), "1.0".into())],
        &first_budget.records(),
    )
    .unwrap();
    drop(closed);
    drop(root);
    drop(first_budget);
    let lock_path = fixture.project_path().with_file_name("pakro.lock");
    let scratch = stage.join("update-source-scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    registry.scratch_root = &scratch;
    // This case specifically exercises strict reconstruction. Evict only
    // these owned fixture outputs; the separate nonce-install regression
    // covers Ready reuse when the original ZIP cache has been cleared.
    for (_, record) in model.builds() {
        std::fs::remove_file(
            registry
                .cache_root
                .join("artifacts")
                .join(&record.output_digest)
                .join(&record.output_filename),
        )
        .unwrap();
    }
    // The runtime solve needs one project. A fresh dynamic build solve would
    // need both counter-dep and helper and must exceed this test boundary.
    registry.limits.max_projects = 1;
    registry.preferred_pins = vec![pin.clone()];
    for mode in 0..3 {
        let variant = match mode {
            0 => model.clone(),
            1 => changed_runtime_model(&model, false),
            _ => changed_runtime_model(&model, true),
        };
        let bytes = format!(
            "lockfile_version=4\n{}",
            variant.encode_projection(Default::default()).unwrap()
        )
        .into_bytes();
        std::fs::write(&lock_path, &bytes).unwrap();
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let reusable = reusable_source_record(&root, &language, pin.id()).unwrap();
        assert_eq!(reusable.is_some(), mode == 0);
        let nested_source = expected
            .inputs
            .iter()
            .find(|input| input.source_is_sdist)
            .unwrap();
        let nested_pin = PinId::parse_hex(&nested_source.source_pin).unwrap();
        assert!(root.source_build(nested_pin).is_some());
        assert!(
            reusable_source_record(&root, &language, nested_pin)
                .unwrap()
                .is_none(),
            "a build-only receipt is not an old runtime-graph selection"
        );
        let mut budget = BuildBudget::new();
        let prepared = prepare_registry(
            &context,
            &language,
            &root,
            declarations.clone(),
            registry.clone(),
            &mut budget,
            0,
        )
        .await;
        if mode == 0 {
            let closed = close_registry_plan(
                prepared.expect(
                    "the old build environments must replay without a new multi-project solve",
                ),
                Default::default(),
            )
            .unwrap();
            let output = closed.owner(closed.plan().selected[0].key).unwrap();
            assert_eq!(output.build_record().unwrap(), &expected);
            assert_eq!(output.output_digest().to_string(), expected.output_digest);
            assert_eq!(
                budget.builds, 2,
                "the source parent and original source input each build once"
            );
            assert_eq!(budget.completed.len(), 2);
            closed.recheck_for(&root).unwrap();
        } else {
            let error = match prepared {
                Ok(_) => panic!("a changed runtime or target must prepare fresh build inputs"),
                Err(error) => error,
            };
            assert!(
                error
                    .chain()
                    .any(|cause| cause.downcast_ref::<RegistryDepotError>()
                        == Some(&RegistryDepotError::Limit)),
                "fresh build preparation must reach the multi-project dependency limit: {error:#}"
            );
            assert!(
                budget.builds >= 2,
                "the fresh parent and source dependency must actually have started"
            );
        }
        assert!(budget.active.is_empty());
        assert_eq!(budget.workspace_reserved, 0);
        root.recheck().unwrap();
        language.check_binding().unwrap();
        fixture.no_publish_with_existing_lock(&bytes);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_coordinator_reuses_nonce_source_dependency_across_dynamic_solve() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let (fixture, counter_digest, helper_digest) =
        ProjectRegistryFixture::dynamic_source_dependency();
    let root = fixture.lock_at_deadline(context.policy().deadline);
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.candidate_policy.sdists = Sdists::Include;
    registry.deadline = context.policy().deadline;
    let mut budget = BuildBudget::new();
    let prepared = prepare_registry(
        &context,
        &language,
        &root,
        vec!["demo-pkg==1.0".into()],
        registry,
        &mut budget,
        0,
    )
    .await
    .expect("the final backend must see the exact nonce input from its requirements hook");
    let closed = close_registry_plan(prepared, Default::default()).unwrap();
    closed.recheck_for(&root).unwrap();
    assert_eq!(
        closed.plan().selected.len(),
        1,
        "build inputs are not invented runtime dependencies"
    );
    let node = &closed.plan().selected[0];
    assert_eq!(node.name, "demo-pkg");
    let source = closed.owner(node.key).unwrap();
    assert_eq!(source.pin().file().kind(), RegistryArtifactKind::Sdist);
    let record = source.build_record().unwrap().clone();
    record.validate().unwrap();
    assert_eq!(record.inputs.len(), 2);
    let counter = record
        .inputs
        .iter()
        .find(|input| input.name == "counter-dep")
        .unwrap();
    assert!(counter.source_is_sdist);
    assert_eq!(counter.source_filename, "counter_dep-1.0.tar.gz");
    assert_eq!(counter.source_digest, counter_digest.to_string());
    assert_eq!(counter.output_filename, "counter_dep-1.0-py3-none-any.whl");
    assert_ne!(counter.source_digest, counter.output_digest);
    counter.content_pin().unwrap();
    let cached = budget
        .completed
        .values()
        .find(|(build, _)| build.record().source_digest == counter.source_digest)
        .unwrap();
    assert_eq!(counter.output_digest, cached.0.record().output_digest);
    assert_eq!(counter.output_size, cached.0.record().output_size);
    let helper = record
        .inputs
        .iter()
        .find(|input| input.name == "helper")
        .unwrap();
    assert!(!helper.source_is_sdist);
    assert_eq!(helper.source_digest, helper_digest.to_string());
    assert_eq!(helper.output_digest, helper.source_digest);
    assert_eq!(
        budget.builds, 2,
        "demo and counter are each actually built once"
    );
    assert_eq!(budget.completed.len(), 2);
    assert!(budget.active.is_empty());
    assert!(budget.policy_work > 0 && budget.policy_work <= 16_777_216);
    assert!(budget.policy_utf8 > 0 && budget.policy_utf8 <= 64 * 1024 * 1024);
    assert_eq!(budget.workspace_reserved, 0);
    let (archives, evidence) = closed.into_shared_sources().unwrap();
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].manifest().name(), "demo-pkg");
    assert_eq!(archives[0].digest().to_string(), record.output_digest);
    evidence
        .recheck(tokio::time::Instant::from_std(context.policy().deadline))
        .unwrap();
    root.recheck().unwrap();
    language.check_binding().unwrap();
    fixture.no_publish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p3_strict_cached_source_outputs_reject_changed_backend_and_request_without_publication() {
    const TEST: &str = "programs::install::build::tests::p3_strict_cached_source_outputs_reject_changed_backend_and_request_without_publication";
    const CHILD: &str = "PAKRO_TEST_P3_CACHED_RECORD_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1");
        for variable in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(variable);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "cached source receipt fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    cached_source_record_fixture().boxed_local().await;
}

async fn cached_source_record_fixture() {
    use crate::libraries::project::repository::ProjectRepository;
    use crate::packages::python_resolution::source::locked_graph::{
        LockedBuildInputPreparation, acquire_build_input,
    };
    use crate::programs::install::{lock, resolve};
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: std::time::Instant::now() + Duration::from_secs(300),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: stage.join("data"),
        cache_root: stage.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let language = crate::libraries::package::layouts::tests::real_language(&context)
        .boxed_local()
        .await;
    // Keep a real catalog authority, including its bytes, across every failed
    // replay. Source preparation does not publish a replacement selection.
    drop(ProjectRepository::open_writer(&context).unwrap());
    let catalog_path = context
        .requested_data_root()
        .join("projects/.control/catalog/projects");
    let catalog = std::fs::read(&catalog_path).unwrap();
    let (fixture, _, _) = ProjectRegistryFixture::deterministic_source_dependency();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.deadline = context.policy().deadline;
    let root = fixture.lock_at_deadline(context.policy().deadline);
    eprintln!("P3_CACHED_RECORD_PHASE=first-actual-build");
    let graph = resolve::resolve_exact_roots(
        &context,
        &language,
        &root,
        &[("demo-pkg", "1.0")],
        registry.clone(),
    )
    .boxed_local()
    .await
    .unwrap();
    let selected = graph
        .graph()
        .plan()
        .selected
        .iter()
        .find(|node| node.name == "demo-pkg")
        .unwrap();
    let main = graph.graph().owner(selected.key).unwrap().pin().id();
    let record = graph
        .graph()
        .owner(selected.key)
        .unwrap()
        .build_record()
        .unwrap()
        .clone();
    let child = record
        .inputs
        .iter()
        .find(|input| input.name == "counter-dep")
        .unwrap()
        .content_pin()
        .unwrap()
        .id();
    let projection = graph
        .projection(&vec![[0; 32]; graph.graph().plan().selected.len()])
        .unwrap();
    let model = lock::project_lock_model(
        &root,
        &language,
        &projection,
        &[("demo-pkg".into(), "1.0".into())],
        graph.builds(),
    )
    .unwrap();
    let bytes = root
        .prepare_document(model.clone(), Default::default())
        .unwrap()
        .bytes()
        .to_vec();
    drop(graph);
    drop(root);
    let lock_path = fixture.project_path().with_file_name("pakro.lock");
    let cold_cache = stage.join("empty-output-cache");
    let cold_scratch = stage.join("cold-source-scratch");
    std::fs::create_dir_all(&cold_scratch).unwrap();
    let hooks = crate::packages::python_source_build::observed_hook_calls();
    assert!(hooks > 0);
    std::fs::write(&lock_path, &bytes).unwrap();
    {
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let mut budget = BuildBudget::new();
        let prepared = replay_registry(
            &context,
            &language,
            &root,
            vec!["demo-pkg==1.0".into()],
            registry.clone(),
            &mut budget,
            0,
        )
        .await
        .unwrap();
        let closed = close_registry_plan(prepared, Default::default()).unwrap();
        assert_eq!(closed.plan().selected.len(), 1);
        drop(closed);
        let mut continuation = match acquire_build_input(
            &root,
            main,
            "counter-dep",
            registry.clone(),
        )
        .boxed()
        .await
        .unwrap()
        {
            LockedBuildInputPreparation::NeedsBuild(continuation) => continuation,
            LockedBuildInputPreparation::Ready(_) => panic!(
                "even a valid cached source dependency requires actual source/runtime validation"
            ),
        };
        let source = continuation
            .source_verified_copy(registry.scratch_root)
            .unwrap();
        let built = super::replay::rebuild(
            &context,
            &language,
            &root,
            child,
            source,
            model.source_build(child).unwrap().clone(),
            registry.clone(),
            &mut budget,
            0,
        )
        .await
        .unwrap();
        let (input, archive, proof) = continuation.resume_with_built(built).unwrap().into_parts();
        assert_eq!(input.name, "counter-dep");
        assert_eq!(archive.digest().to_string(), input.output_digest);
        proof.recheck(&root).unwrap();
        assert_eq!(budget.builds, 0);
        assert_eq!(budget.policy_work, 0);
        assert_eq!(
            crate::packages::python_source_build::observed_hook_calls(),
            hooks
        );
        assert_eq!(std::fs::read(&lock_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&catalog_path).unwrap(), catalog);
    }
    for (source, build_input) in [(main, false), (child, true)] {
        let original = model.source_build(source).unwrap();
        let cached = crate::artifact_snapshot::cached_verified_artifact(
            &original.output_filename,
            original.output_digest.parse().unwrap(),
            registry.cache_root,
            original.output_size,
        )
        .unwrap()
        .expect("the original output ZIP is genuinely cached");
        assert_eq!(cached.size(), original.output_size);
        drop(cached);
        for request_changed in [false, true] {
            let mut document = std::str::from_utf8(&bytes)
                .unwrap()
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            let source_hex = hex::encode(source.as_bytes());
            let changed = document["python"]["builds"]
                .as_array_of_tables_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["source_pin"].as_str() == Some(source_hex.as_str()))
                .unwrap();
            if request_changed {
                let mut request = original.request_id;
                request[0] ^= 1;
                changed["request_id"] = toml_edit::value(hex::encode(request));
            } else {
                changed["backend"] = toml_edit::value("different_backend");
            }
            let changed_bytes = document.to_string().into_bytes();
            std::fs::write(&lock_path, &changed_bytes).unwrap();
            let root = fixture.lock_at_deadline(context.policy().deadline);
            let changed_record = root.source_build(source).unwrap();
            changed_record.validate().unwrap();
            assert_eq!(changed_record.source_digest, original.source_digest);
            assert_eq!(changed_record.output_digest, original.output_digest);
            let expected_error = if request_changed {
                "cached source output request differs from its source, runtime, recipe or fixed build inputs"
            } else {
                "cached source output differs from its actual original source declarations"
            };
            for warm in [true, false] {
                eprintln!(
                    "P3_CACHED_RECORD_PHASE=replay build_input={build_input} request_changed={request_changed} warm={warm}"
                );
                let mut attempt = registry.clone();
                if !warm {
                    attempt.cache_root = &cold_cache;
                    attempt.scratch_root = &cold_scratch;
                    assert!(
                        crate::artifact_snapshot::cached_verified_artifact(
                            &original.output_filename,
                            original.output_digest.parse().unwrap(),
                            attempt.cache_root,
                            original.output_size,
                        )
                        .unwrap()
                        .is_none(),
                        "the cold attempt has no output ZIP"
                    );
                }
                let mut budget = BuildBudget::new();
                let error = if build_input {
                    let mut continuation = match acquire_build_input(
                        &root,
                        main,
                        "counter-dep",
                        attempt.clone(),
                    )
                    .boxed()
                    .await
                    .unwrap()
                    {
                        LockedBuildInputPreparation::NeedsBuild(continuation) => continuation,
                        LockedBuildInputPreparation::Ready(_) => panic!(
                            "a cached build dependency must require actual source/runtime validation"
                        ),
                    };
                    let source = continuation
                        .source_verified_copy(attempt.scratch_root)
                        .unwrap();
                    match super::replay::rebuild(
                        &context,
                        &language,
                        &root,
                        child,
                        source,
                        changed_record.clone(),
                        attempt,
                        &mut budget,
                        0,
                    )
                    .await
                    {
                        Ok(_) => panic!("a changed child receipt must not pass strict replay"),
                        Err(error) => error,
                    }
                } else {
                    match replay_registry(
                        &context,
                        &language,
                        &root,
                        vec!["demo-pkg==1.0".into()],
                        attempt,
                        &mut budget,
                        0,
                    )
                    .await
                    {
                        Ok(_) => panic!("a changed source receipt must not pass strict replay"),
                        Err(error) => error,
                    }
                };
                assert!(
                    error
                        .chain()
                        .any(|cause| cause.to_string() == expected_error),
                    "warm={warm} build_input={build_input} request_changed={request_changed}: {error:#}"
                );
                assert_eq!(
                    crate::packages::python_source_build::observed_hook_calls(),
                    hooks,
                    "invalid receipts must fail before executing a hook in either cache state"
                );
                assert_eq!(budget.policy_work, 0);
                assert_eq!(budget.workspace_reserved, 0);
                assert!(budget.active.is_empty());
                assert_eq!(std::fs::read(&lock_path).unwrap(), changed_bytes);
                assert_eq!(std::fs::read(&catalog_path).unwrap(), catalog);
                root.recheck().unwrap();
                fixture.no_publish_with_existing_lock(&changed_bytes);
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2_source_cold_and_warm_replay_keeps_recursive_inputs_and_rejects_changed_output() {
    source_cold_and_warm_replay(false).boxed_local().await;
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p6_zip_source_cold_and_warm_replay_keeps_recursive_inputs_and_rejects_changed_output() {
    source_cold_and_warm_replay(true).boxed_local().await;
}

#[cfg(target_os = "macos")]
async fn source_cold_and_warm_replay(zip: bool) {
    use crate::programs::install::{lock, resolve};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    for deterministic in [true, false] {
        let (fixture, _, _) = if zip {
            ProjectRegistryFixture::zip_source_dependency(deterministic)
        } else if deterministic {
            ProjectRegistryFixture::deterministic_source_dependency()
        } else {
            ProjectRegistryFixture::dynamic_source_dependency()
        };
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let endpoints = [fixture.endpoint()];
        let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
        let mut registry = fixture.context(&endpoints, &policy);
        registry.environment = language.target().environment();
        registry.supported = language.target().supported();
        registry.semantics = language.target().semantics();
        registry.candidate_policy.sdists = Sdists::Include;
        registry.deadline = context.policy().deadline;
        let roots = if deterministic {
            vec![("demo-pkg", "1.0"), ("other-pkg", "1.0")]
        } else {
            vec![("demo-pkg", "1.0")]
        };
        let declarations = roots
            .iter()
            .map(|(name, version)| format!("{name}=={version}"))
            .collect::<Vec<_>>();
        let graph =
            resolve::resolve_exact_roots(&context, &language, &root, &roots, registry.clone())
                .await
                .unwrap();
        let main = graph
            .graph()
            .plan()
            .selected
            .iter()
            .find(|node| node.name == "demo-pkg")
            .unwrap();
        let record = graph
            .graph()
            .owner(main.key)
            .unwrap()
            .build_record()
            .unwrap()
            .clone();
        assert_eq!(record.inputs.len(), 2);
        assert_eq!(record.observation.as_ref().unwrap().initial_inputs.len(), 1);
        if zip {
            let pin = graph.graph().owner(main.key).unwrap().pin();
            assert_eq!(pin.file().filename(), "demo_pkg-1.0.zip");
            assert_eq!(pin.file().kind(), RegistryArtifactKind::Sdist);
            assert_eq!(record.source_digest, pin.digest().to_string());
            let counter = record
                .inputs
                .iter()
                .find(|input| input.name == "counter-dep")
                .unwrap();
            assert!(counter.source_is_sdist);
            assert_eq!(counter.source_filename, "counter_dep-1.0.zip");
            assert!(
                fixture
                    .requests()
                    .iter()
                    .any(|request| request == "/files/demo_pkg-1.0.zip")
            );
            assert!(
                fixture
                    .requests()
                    .iter()
                    .any(|request| request == "/files/counter_dep-1.0.zip")
            );
        }
        let projection = graph
            .projection(&vec![[0; 32]; graph.graph().plan().selected.len()])
            .unwrap();
        let model = lock::project_lock_model(
            &root,
            &language,
            &projection,
            &roots
                .iter()
                .map(|(name, version)| ((*name).into(), (*version).into()))
                .collect::<Vec<_>>(),
            graph.builds(),
        )
        .unwrap();
        if zip {
            let expected: std::collections::BTreeSet<_> = if deterministic {
                [
                    "demo_pkg-1.0.zip",
                    "counter_dep-1.0.zip",
                    "other_pkg-1.0.zip",
                ]
                .into_iter()
                .collect()
            } else {
                ["demo_pkg-1.0.zip", "counter_dep-1.0.zip"]
                    .into_iter()
                    .collect()
            };
            let actual = model
                .pins()
                .iter()
                .filter(|pin| pin.file().kind() == RegistryArtifactKind::Sdist)
                .map(|pin| pin.file().filename())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, expected);
            let encoded = model.encode_projection(Default::default()).unwrap();
            let decoded =
                crate::packages::python_resolution::lock_model::PythonLockModel::decode_projection(
                    &encoded,
                    Default::default(),
                )
                .unwrap();
            assert_eq!(
                decoded.encode_projection(Default::default()).unwrap(),
                encoded
            );
            assert_eq!(
                decoded
                    .pins()
                    .iter()
                    .filter(|pin| pin.file().kind() == RegistryArtifactKind::Sdist)
                    .map(|pin| pin.file().filename())
                    .collect::<std::collections::BTreeSet<_>>(),
                expected
            );
        }
        let bytes = root
            .prepare_document(model, Default::default())
            .unwrap()
            .bytes()
            .to_vec();
        drop(graph);
        drop(root);
        let lock_path = fixture.project_path().with_file_name("pakro.lock");
        std::fs::write(&lock_path, &bytes).unwrap();
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let cache = stage.join(format!("strict-cache-{deterministic}"));
        let scratch = stage.join(format!("strict-scratch-{deterministic}"));
        std::fs::create_dir_all(&scratch).unwrap();
        registry.cache_root = &cache;
        registry.scratch_root = &scratch;
        let before = fixture.requests().len();
        let mut budget = BuildBudget::new();
        let replayed = replay_registry(
            &context,
            &language,
            &root,
            declarations.clone(),
            registry.clone(),
            &mut budget,
            0,
        )
        .await;
        assert_eq!(
            budget.policy_work, 0,
            "strict replay cannot enumerate solver candidates"
        );
        assert_eq!(budget.workspace_reserved, 0);
        assert!(budget.active.is_empty());
        if !deterministic {
            let error = match replayed {
                Ok(_) => {
                    panic!("a newly generated nonce output must not replace the locked output")
                }
                Err(error) => error,
            };
            assert!(
                error.chain().any(|cause| cause.to_string()
                    == "strict source rebuilt output differs from the locked artifact"),
                "nonce replay must reject its actual rebuilt output; another error is not evidence: {error:#}"
            );
            assert_eq!(std::fs::read(&lock_path).unwrap(), bytes);
            fixture.no_publish_with_existing_lock(&bytes);
            eprintln!(
                "P2_STRICT_NONCE_OUTPUT_REJECTED locked_bytes_preserved=true no_publication=true"
            );
            continue;
        }
        assert!(
            record
                .inputs
                .iter()
                .find(|input| input.name == "counter-dep")
                .unwrap()
                .output_size
                < 64 * 1024,
            "the backend's 64 KiB intermediate must exceed its compressed wheel size"
        );
        assert_eq!(
            budget.builds, 3,
            "two parents and their shared initial/final source dependency must each build once"
        );
        assert_eq!(budget.completed.len(), 3);
        let counter = record
            .inputs
            .iter()
            .find(|input| input.name == "counter-dep")
            .unwrap();
        let cached = budget
            .completed
            .values()
            .find(|(build, _)| build.record().source_digest == counter.source_digest)
            .unwrap();
        assert_eq!(cached.0.record().output_digest, counter.output_digest);
        let closed = close_registry_plan(replayed.unwrap(), Default::default()).unwrap();
        assert_eq!(closed.plan().selected.len(), 2);
        let main = closed
            .plan()
            .selected
            .iter()
            .find(|node| node.name == "demo-pkg")
            .unwrap();
        let output = closed.owner(main.key).unwrap();
        assert_eq!(
            output.build_record().unwrap().fingerprint().unwrap(),
            record.fingerprint().unwrap()
        );
        assert!(
            fixture.requests().len() > before,
            "cold replay must acquire actual original bytes"
        );
        drop(closed);
        let after_cold = fixture.requests().len();
        let mut warm = BuildBudget::new();
        let replayed = replay_registry(
            &context,
            &language,
            &root,
            declarations,
            registry,
            &mut warm,
            0,
        )
        .await
        .unwrap();
        close_registry_plan(replayed, Default::default()).unwrap();
        assert_eq!(
            fixture.requests().len(),
            after_cold,
            "warm exact inputs require no registry access"
        );
        assert_eq!(warm.policy_work, 0);
        assert_eq!(
            warm.builds, 0,
            "the new warm operation must reuse the verified original source outputs"
        );
        assert_eq!(warm.completed.len(), 0);
        assert_eq!(warm.workspace_reserved, 0);
        assert!(warm.active.is_empty());
        assert_eq!(std::fs::read(&lock_path).unwrap(), bytes);
        fixture.no_publish_with_existing_lock(&bytes);
    }
}
