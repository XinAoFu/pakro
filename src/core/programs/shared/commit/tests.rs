use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{
    archive, install_spec, install_spec_for, prepare,
};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_resolution::source::registry_depot::ProjectRegistryFixture;
use crate::programs::install::resolve::resolve_exact_roots;
use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};
use std::time::{Duration, Instant};

#[path = "p2_tests.rs"]
mod p2_tests;

#[test]
fn p6_commit_runtime_admission_keeps_the_complete_source_result() {
    let (_, record, source) = crate::libraries::language::lease::pin_tests::source_facts();
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let entry = crate::lockfile::LockfileTool {
        version: record.version.clone(),
        backend: Some("core:python".into()),
        specifiers: std::collections::BTreeSet::from([record.version.clone()]),
        options: std::collections::BTreeMap::from([("source".into(), "source".into())]),
        platforms: std::collections::BTreeMap::from([(target.clone(), source.clone())]),
    };
    validate_runtime_lock_entry(&entry, &source, &target).unwrap();
    for output in [true, false] {
        let mut changed = entry.clone();
        let record = changed
            .platforms
            .get_mut(&target)
            .unwrap()
            .source_python
            .as_mut()
            .unwrap();
        if output {
            record.output_sha256[0] ^= 1;
        } else {
            record.runtime_target[0] ^= 1;
        }
        let error = validate_runtime_lock_entry(&changed, &source, &target).unwrap_err();
        assert!(format!("{error:#}").contains("complete build result"));
    }
    let mut changed = entry.clone();
    changed.options.clear();
    assert!(
        validate_runtime_lock_entry(&changed, &source, &target).is_err(),
        "a source result cannot use the default prebuilt intent"
    );
    let mut changed = entry.clone();
    changed.platforms.get_mut(&target).unwrap().install = None;
    assert!(
        validate_runtime_lock_entry(&changed, &source, &target).is_err(),
        "the source install marker must travel with its actual result"
    );
    let mut changed = entry;
    changed
        .options
        .insert("unrecognized".into(), "value".into());
    assert!(validate_runtime_lock_entry(&changed, &source, &target).is_err());
}

#[test]
fn conda_runtime_lock_must_retain_source_and_exact_artifact() {
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let source = crate::lockfile::PlatformInfo {
        install: Some("conda".into()),
        url: Some("https://conda.anaconda.org/conda-forge/osx-arm64/python.conda".into()),
        checksum: Some(format!("sha256:{}", "12".repeat(32))),
        ..Default::default()
    };
    let mut entry = crate::lockfile::LockfileTool {
        version: "3.13.13".into(),
        backend: Some("core:python".into()),
        specifiers: std::collections::BTreeSet::from(["3.13.13".into()]),
        options: std::collections::BTreeMap::from([("source".into(), "conda".into())]),
        platforms: std::collections::BTreeMap::from([(target.clone(), source.clone())]),
    };
    validate_runtime_lock_entry(&entry, &source, &target).unwrap();
    entry.platforms.get_mut(&target).unwrap().checksum =
        Some(format!("sha256:{}", "34".repeat(32)));
    assert!(validate_runtime_lock_entry(&entry, &source, &target).is_err());
    entry.platforms.insert(target.clone(), source.clone());
    entry.options.clear();
    assert!(validate_runtime_lock_entry(&entry, &source, &target).is_err());
}

#[test]
fn conda_graph_roots_keep_transitive_packages_as_dependencies() {
    use crate::packages::python_environment::model::{
        PackageGraphEdge, PackageGraphNode, PackageGraphRecord, PackageSourcePin,
    };
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{
        GenericVirtualPackage, PackageName, PackageRecord, RepoDataRecord, Version,
    };

    let mut package = PackageRecord::new(
        "python".parse::<PackageName>().unwrap(),
        "3.13.13".parse::<Version>().unwrap(),
        "build".into(),
    );
    package.depends = vec!["openssl >=3".into(), "__osx >=11".into()];
    let python = RepoDataRecord {
        package_record: package,
        identifier: DistArchiveIdentifier::new(
            ArchiveIdentifier {
                name: "python".into(),
                version: "3.13.13".into(),
                build_string: "build".into(),
            },
            DistArchiveType::Conda(CondaArchiveType::Conda),
        ),
        url: "https://example.invalid/python-3.13.13-build.conda"
            .parse()
            .unwrap(),
        channel: Some("https://example.invalid/".into()),
    };
    let virtual_packages = vec![GenericVirtualPackage {
        name: "__osx".parse().unwrap(),
        version: "14".parse().unwrap(),
        build_string: "0".into(),
    }];
    let nodes = ["pip", "urllib3", "openssl", "libz"]
        .into_iter()
        .map(|name| PackageGraphNode {
            name: name.into(),
            version: "1.0".into(),
            instance: [0; 32],
            source_pin: String::new(),
            artifact: String::new(),
            pin: PackageSourcePin {
                registry: String::new(),
                base: [0; 32],
                filename: String::new(),
                size: 0,
                advertised: false,
                build: None,
                conda: None,
            },
        })
        .collect();
    let graph = PackageGraphRecord {
        roots: vec![0, 2],
        nodes,
        edges: vec![
            PackageGraphEdge {
                from: 0,
                to: 1,
                declaration: "urllib3 >=1".into(),
            },
            PackageGraphEdge {
                from: 2,
                to: 3,
                declaration: "libz >=1".into(),
            },
        ],
    };
    let declared = ["python".to_owned(), "pip".to_owned()]
        .into_iter()
        .collect();
    validate_conda_graph_roots(&python, &declared, &virtual_packages, &graph).unwrap();
    let mut wrong = graph.clone();
    wrong.roots.reverse();
    validate_conda_graph_roots(&python, &declared, &virtual_packages, &wrong).unwrap();
    wrong = graph.clone();
    wrong.roots.push(1);
    assert!(validate_conda_graph_roots(&python, &declared, &virtual_packages, &wrong).is_err());
    wrong.roots = vec![0];
    assert!(validate_conda_graph_roots(&python, &declared, &virtual_packages, &wrong).is_err());
    wrong.roots = vec![0, 2, 2];
    assert!(validate_conda_graph_roots(&python, &declared, &virtual_packages, &wrong).is_err());
    assert!(validate_conda_graph_roots(&python, &declared, &[], &graph).is_err());
}

fn context(root: &std::path::Path) -> OperationContext {
    context_with_lock(root, false)
}
fn context_with_lock(root: &std::path::Path, lockfile: bool) -> OperationContext {
    use crate::programs::*;
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(if lockfile { 300 } else { 120 }),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile,
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
fn observe(project: &std::path::Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(
        project,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(120)),
    )
    .unwrap()
}

#[tokio::test]
async fn p3_forget_invalidates_work_admitted_before_or_after_content_preparation() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = context(&stage);
    let archive = archive("3.15.0");
    let spec = install_spec(&archive);
    drop(prepare(&spec, &context, &archive).await.unwrap());
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let initially_absent = ExpectedProjectState::capture(&context, &identity).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
        panic!("language candidate missing")
    };
    commit_selection(
        prepare_selection_commit_with_state(&context, &observed, &candidate, &initially_absent)
            .unwrap(),
    )
    .unwrap();
    let registered = ExpectedProjectState::capture(&context, &identity).unwrap();
    let prepared_early =
        prepare_selection_commit_with_state(&context, &observed, &candidate, &registered).unwrap();
    let report = crate::programs::uninstall::project::forget(
        &context,
        &identity.as_uuid().to_string(),
        false,
    )
    .unwrap();
    assert!(report.forgotten);
    assert!(commit_selection(prepared_early).is_err());
    assert!(
        prepare_selection_commit_with_state(&context, &observed, &candidate, &registered).is_err(),
        "old installation must not take a fresh baseline after content preparation"
    );
    assert!(
        prepare_selection_commit_with_state(&context, &observed, &candidate, &initially_absent)
            .is_err(),
        "first-install absence must not survive another install and forget"
    );
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    assert!(
        repository
            .model()
            .project_registration(identity.as_uuid())
            .is_none()
    );
    drop(repository);
    let fresh = ExpectedProjectState::capture(&context, &identity).unwrap();
    let receipt = commit_selection(
        prepare_selection_commit_with_state(&context, &observed, &candidate, &fresh).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt.revision(), 3);
}

#[tokio::test]
async fn p3_commit_rejects_other_group_changes_after_its_initial_observation() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = context(&stage);
    let archive = archive("3.15.0");
    let spec = install_spec(&archive);
    drop(prepare(&spec, &context, &archive).await.unwrap());
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
        panic!("candidate missing")
    };
    commit_selection(prepare_selection_commit(&context, &observed, &candidate).unwrap()).unwrap();
    let state = ExpectedProjectState::capture(&context, &identity).unwrap();
    let prepared =
        prepare_selection_commit_with_state(&context, &observed, &candidate, &state).unwrap();
    let mut other = ProjectRepository::open_writer(&context).unwrap();
    let key = SelectionKey::new(
        identity.as_uuid(),
        "auxiliary",
        [8; 32],
        other.model().limits(),
    )
    .unwrap();
    let expected = other.model().snapshot(&key);
    other
        .model_mut()
        .apply_selection(&key, &expected, None, Uuid::now_v7())
        .unwrap();
    other.commit_model().unwrap();
    let bytes = other.model().encode_catalog(Default::default()).unwrap();
    assert!(commit_selection(prepared).is_err());
    assert_eq!(
        ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .encode_catalog(Default::default())
            .unwrap(),
        bytes
    );
}
#[tokio::test]
async fn f3_failed_apply_keeps_old_selection() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    let config = project.join("pakro.toml");
    let initial = "[tools]\npython='3.14.0'\n";
    std::fs::write(&config, initial).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let language = prepare(&spec, &context, &bytes).await.unwrap();
    let old_bytes = archive("3.14.0");
    let old_spec = install_spec_for(
        "3.14.0",
        &old_bytes,
        &crate::plugins::core::python::native::RuntimeArchiveTarget::new(
            "aarch64",
            crate::plugins::core::python::native::RuntimePlatform::Macos,
        )
        .unwrap(),
        "20261013",
    );
    let old_language = prepare(&old_spec, &context, &old_bytes).await.unwrap();
    drop(old_language);
    let observed = observe(&project);
    let query = open_query_context(&context, &observed).unwrap();
    let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
        panic!("language candidate missing")
    };
    commit_selection(prepare_selection_commit(&context, &observed, &candidate).unwrap()).unwrap();
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    drop(query);
    drop(observed);

    let declaration = "[tools]\npython='3.15.0'\n[packages.\"python@3.15.0\"]\ndemo-pkg='1.0'\n";
    std::fs::write(&config, declaration).unwrap();
    let fixture = ProjectRegistryFixture::wheels();
    let lock = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let closed = resolve_exact_roots(
        &context,
        &language,
        &lock,
        &[("demo-pkg", "1.0")],
        fixture.context(&endpoints, &policy),
    )
    .await
    .unwrap();
    let facts = closed.package_facts(&language).unwrap();
    let graph = closed
        .projection(&facts.iter().map(|fact| fact.key).collect::<Vec<_>>())
        .unwrap();
    let fingerprint = closed.fingerprint().unwrap();
    let (archives, evidence) = closed.into_sources().unwrap();
    let sources = archives
        .into_iter()
        .map(|archive| {
            let fact = facts
                .iter()
                .find(|fact| fact.name == archive.manifest().name())
                .unwrap()
                .clone();
            (fact, archive)
        })
        .collect();
    let layout = crate::libraries::package::layouts::prepare_layout(
        &context,
        &language,
        fingerprint,
        sources,
    )
    .unwrap();
    let layout_key = layout.key();
    drop(layout);
    drop(language);
    let observed = observe(&project);
    let prepared = prepare_package_selection_commit(
        &context,
        &observed,
        spec.key(),
        layout_key,
        graph.clone(),
    )
    .unwrap();
    let changed = format!("{declaration}# changed while waiting for commit\n");
    std::fs::write(&config, &changed).unwrap();
    assert!(commit_package_selection(prepared).is_err());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), changed);
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let old = repository
        .read_views()
        .language_selection(&identity)
        .unwrap();
    assert_eq!(old.language(), &old_spec.key().fingerprint());
    assert!(old.package_references().is_empty());
    drop(repository);
    drop(observed);

    std::fs::write(&config, declaration).unwrap();
    let observed = observe(&project);
    let prepared =
        prepare_package_selection_commit(&context, &observed, spec.key(), layout_key, graph)
            .unwrap();
    commit_package_selection(prepared).unwrap();
    evidence
        .recheck(tokio::time::Instant::now() + Duration::from_secs(20))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), declaration);
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let views = repository.read_views();
    assert!(
        views
            .reference_chains(old_spec.key().fingerprint())
            .unwrap()
            .is_empty(),
        "switching target must retire the old project reference"
    );
    let selected = views.package_graph(&identity).unwrap().unwrap();
    assert_eq!(selected.roots().len(), 1);
    assert_eq!(selected.nodes().len(), 2);
    assert_eq!(selected.edges().len(), 1);
    assert_eq!(
        views
            .matching_package_graphs(spec.key().fingerprint(), &[("demo-pkg", "1.0")])
            .unwrap()
            .len(),
        1
    );
    let dep = selected
        .nodes()
        .iter()
        .find(|node| node.name() == "dep")
        .unwrap();
    let chains = views.reference_chains(*dep.instance()).unwrap();
    assert!(
        chains
            .iter()
            .any(|chain| chain.contains("demo-pkg==1.0 -> dep==1.0")),
        "{chains:?}"
    );
}

#[tokio::test]
async fn f1_default_target_switch_retires_previous_reference() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    let context = context(&stage);
    let target = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    let first_bytes = archive("3.14.0");
    let first = install_spec_for("3.14.0", &first_bytes, &target, "20261013");
    let second_bytes = archive("3.15.0");
    let second = install_spec(&second_bytes);
    drop(prepare(&first, &context, &first_bytes).await.unwrap());
    drop(prepare(&second, &context, &second_bytes).await.unwrap());
    for version in ["3.14.0", "3.15.0"] {
        std::fs::write(
            project.join("pakro.toml"),
            format!("[tools]\npython='{version}'\n"),
        )
        .unwrap();
        let observed = observe(&project);
        let query = open_query_context(&context, &observed).unwrap();
        let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
            panic!("candidate missing")
        };
        commit_selection(prepare_selection_commit(&context, &observed, &candidate).unwrap())
            .unwrap();
    }
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let views = repository.read_views();
    assert_eq!(
        views.language_selection(&identity).unwrap().language(),
        &second.key().fingerprint()
    );
    assert!(
        views
            .reference_chains(first.key().fingerprint())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        views
            .reference_chains(second.key().fingerprint())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repository
            .model()
            .selection_slots()
            .filter(|(key, stamp)| key.project() == identity.as_uuid()
                && (stamp.language().is_some() || stamp.generation().is_some()))
            .count(),
        1
    );
}

#[tokio::test]
async fn p1_same_project_competing_commits_keep_one_winner_and_other_project() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let context = context(&stage);
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let projects = [root.join("a"), root.join("b")];
    let mut identities = Vec::new();
    for project in &projects {
        std::fs::create_dir(project).unwrap();
        std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
        let observed = observe(project);
        identities.push(ProjectIdentity::from_observation(&observed).unwrap());
        let query = open_query_context(&context, &observed).unwrap();
        let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
            panic!("the fixture language must be Ready");
        };
        commit_selection(prepare_selection_commit(&context, &observed, &candidate).unwrap())
            .unwrap();
    }
    let before = ProjectRepository::open_readonly(&context).unwrap();
    let key = SelectionKey::new(
        identities[0].as_uuid(),
        DEFAULT_GROUP,
        spec.key().target(),
        before.model().limits(),
    )
    .unwrap();
    let revision = before.model().snapshot(&key).revision();
    drop(before);
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let mut competitors = Vec::new();
        for _ in 0..2 {
            let project = &projects[0];
            let context = &context;
            let barrier = &barrier;
            competitors.push(scope.spawn(move || {
                let observed = observe(project);
                let query = open_query_context(context, &observed).unwrap();
                let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
                    panic!("the shared language must remain Ready");
                };
                let prepared = prepare_selection_commit(context, &observed, &candidate).unwrap();
                // Both requests freeze the same revision before either commits.
                barrier.wait();
                commit_selection(prepared)
                    .map(|receipt| receipt.revision())
                    .map_err(|error| format!("{error:#}"))
            }));
        }
        competitors
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert!(
        results.iter().any(|result| result
            .as_ref()
            .is_err_and(|error| error.contains("project registration or selections changed"))),
        "{results:?}"
    );
    let after = ProjectRepository::open_readonly(&context).unwrap();
    assert_eq!(after.model().snapshot(&key).revision(), revision + 1);
    assert_eq!(after.read_views().registered_projects().len(), 2);
    for identity in &identities {
        let selected = after.read_views().language_selection(identity).unwrap();
        assert_eq!(selected.language(), &spec.key().fingerprint());
        assert!(selected.package_references().is_empty());
    }
}
