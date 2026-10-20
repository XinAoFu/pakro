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

fn context(root: &std::path::Path) -> OperationContext {
    use crate::programs::*;
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(120),
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
