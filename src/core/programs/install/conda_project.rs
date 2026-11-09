//! Conda and mixed Python project installation through the shared stores.
mod dependency_groups;

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use eyre::{ContextCompat, Result, ensure};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, PackageName, ParseStrictness,
    Platform, RepoDataRecord,
};

use crate::libraries::language::repository::{LanguageInstallSpec, LanguageRepository};
use crate::libraries::language::source::PythonRuntimeSource;
use crate::libraries::package::lease::{PackageLease, acquire_package};
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::libraries::project::state::ExpectedProjectState;
use crate::packages::conda_artifact::{self, CondaTree};
use crate::packages::python_environment::model::{
    PackageGraphEdge, PackageGraphNode, PackageGraphRecord,
};
use crate::packages::python_resolution::lock_model::{
    CondaLockGraph, LockModelLimits, PythonLockModel,
};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::request::{PackageInstallRequest, PackageSource};
use crate::programs::OperationContext;
use crate::programs::shared::python_roots::PythonLockIntent;

pub(crate) fn requests(
    observed: &ObservedProjectInputs,
    version: &str,
    extra: &[PackageInstallRequest],
) -> Result<Vec<PackageInstallRequest>> {
    ensure!(
        extra.is_empty(),
        "temporary Conda package requests are not supported yet"
    );
    let mut result = Vec::new();
    for request in observed
        .effective_packages()?
        .iter()
        .map(|item| &item.request)
        .chain(extra)
    {
        ensure!(
            request.runtime_name == "python" && request.runtime_version == version,
            "Conda package binding differs from the project's Python"
        );
        if request.source == PackageSource::Conda {
            ensure!(
                request.marker.is_none() && request.extras.is_empty(),
                "Conda package markers and extras are not supported yet"
            );
        }
        result.push(request.clone());
    }
    observed.recheck()?;
    Ok(result)
}

pub(crate) struct CondaAdmission {
    host: (Platform, Vec<GenericVirtualPackage>),
    validated_lock: Option<CondaLockGraph>,
    intent: PythonLockIntent,
}

pub(crate) fn preflight(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    intent: PythonLockIntent,
) -> Result<CondaAdmission> {
    if intent == PythonLockIntent::Update
        && crate::programs::shared::lock_transaction::own_pending_lock_commit(context, observed)?
    {
        return Ok(CondaAdmission {
            host: super::conda::native_target()?,
            validated_lock: None,
            intent,
        });
    }
    let path = crate::programs::python::project_lock_path(observed)?;
    let model = if let Some(bytes) = observed.document(&path) {
        let model = crate::packages::python_resolution::lock_session::decode_root(
            bytes,
            Default::default(),
        )?
        .context("existing lock has no Conda graph")?;
        ensure!(
            model.graphs().is_empty() && model.conda().len() == 1 && model.mixed().len() <= 1,
            "existing lock does not describe one supported Conda project"
        );
        if intent == PythonLockIntent::Preserve {
            super::lock_document::validate_declarations(observed, &model)?;
        } else {
            super::conda_update::validate_previous_selection(context, observed, &model)?;
        }
        Some(model)
    } else {
        ensure!(
            !context.policy().locked,
            "locked Conda installation requires an existing lock"
        );
        None
    };
    let host = super::conda::native_target()?;
    let validated_lock = if let Some(model) = model {
        let graph = model.conda()[0].clone();
        super::conda::validate_host(&graph, &host)?;
        Some(graph)
    } else {
        None
    };
    Ok(CondaAdmission {
        host,
        validated_lock,
        intent,
    })
}

fn dependencies(record: &RepoDataRecord) -> Result<Vec<String>> {
    record
        .package_record
        .depends
        .iter()
        .map(|value| {
            let spec = MatchSpec::from_str(value, ParseStrictness::Lenient)?;
            Ok(spec
                .name
                .as_exact()
                .context("Conda dependency name is not exact")?
                .as_normalized()
                .to_owned())
        })
        .collect()
}

fn allowed_sources(records: &[RepoDataRecord], channels: &[Channel]) -> Result<bool> {
    let config = ChannelConfig::default_with_root_dir(std::path::PathBuf::from("/"));
    for record in records {
        let source = Channel::from_str(
            record
                .channel
                .as_deref()
                .context("Conda package channel is missing")?,
            &config,
        )?;
        if !channels
            .iter()
            .any(|channel| channel.base_url == source.base_url)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn project_root_indices(
    python: &RepoDataRecord,
    requests: &[PackageInstallRequest],
    indices: &BTreeMap<String, usize>,
) -> Result<Vec<usize>> {
    let mut roots = BTreeSet::new();
    for name in dependencies(python)? {
        if name == "python" || name.starts_with("__") {
            continue;
        }
        roots.insert(
            *indices
                .get(&name)
                .context("Python runtime dependency is absent from the package graph")?,
        );
    }
    for request in requests {
        let name = PackageName::from_str(&request.name)?;
        roots.insert(
            *indices
                .get(name.as_normalized())
                .context("declared Conda package is absent from the package graph")?,
        );
    }
    Ok(roots.into_iter().collect())
}

fn visit(
    name: &str,
    records: &BTreeMap<String, &RepoDataRecord>,
    visiting: &mut BTreeSet<String>,
    done: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) -> Result<()> {
    if done.contains(name) || name.starts_with("__") || name == "python" {
        return Ok(());
    }
    ensure!(
        visiting.insert(name.to_owned()),
        "Conda dependency cycle revisits {name}; active packages [{}]; cycle installation is not supported yet",
        visiting.iter().map(String::as_str).collect::<Vec<_>>().join(", ")
    );
    let record = records
        .get(name)
        .context("Conda dependency is absent from its locked graph")?;
    for dependency in dependencies(record)? {
        visit(&dependency, records, visiting, done, order)?;
    }
    visiting.remove(name);
    done.insert(name.to_owned());
    order.push(name.to_owned());
    Ok(())
}

async fn publish_package(
    context: &OperationContext,
    record: &RepoDataRecord,
    records: &[RepoDataRecord],
    leases: &BTreeMap<String, PackageLease>,
    language: Option<&crate::libraries::language::lease::LanguageLease>,
    transport: &mut Option<pakro_index::artifact::ArtifactTransport>,
    project_package: bool,
) -> Result<(PackageLease, Option<CondaTree>)> {
    let key = crate::libraries::package::conda::package_key(record, records)?;
    if let Some(existing) = acquire_package(context, key)? {
        ensure!(
            existing.manifest().conda.as_ref() == Some(record),
            "published Conda package differs from its fixed record"
        );
        return Ok((existing, None));
    }
    let mut artifact = conda_artifact::acquire(context, record, transport).await?;
    let tree = if project_package {
        conda_artifact::extract_project(context, &mut artifact, record)?
    } else {
        conda_artifact::extract(context, &mut artifact, record)?
    };
    let mut held = Vec::new();
    for name in dependencies(record)? {
        if name == "python" || name.starts_with("__") {
            continue;
        }
        held.push(
            leases
                .get(&name)
                .context("Conda package dependency is not published")?
                .clone(),
        );
    }
    let lease = crate::libraries::package::conda::publish_conda_package(
        context,
        &mut artifact,
        &tree,
        records,
        &held,
        language,
    )?;
    Ok((lease, Some(tree)))
}

pub(crate) async fn install(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    requests: &[PackageInstallRequest],
    expected: &ExpectedProjectState,
    admission: &CondaAdmission,
) -> Result<()> {
    let conda_requests: Vec<_> = requests.iter().filter(|request| request.source == PackageSource::Conda).cloned().collect();
    let pypi_requests: Vec<_> = requests.iter().filter(|request| request.source == PackageSource::Pypi).cloned().collect();
    let path = crate::programs::python::project_lock_path(observed)?;
    // Guard the real target the initial observation resolved, never a later
    // re-pointed link.
    let bind_target = observed
        .fixed_lock_target()
        .map(|target| target.to_path_buf())
        .unwrap_or_else(|| path.clone());
    let bound = BoundPythonLock::bind_fixed_conda(
        &bind_target,
        observed.document(&path),
        Default::default(),
        context.policy().deadline,
    )?;
    if admission.intent == PythonLockIntent::Update
        && let Some(model) = bound.model()
        && (model.conda().len() != 1 || admission.validated_lock.as_ref() != model.conda().first())
    {
        super::conda_update::validate_previous_selection(context, observed, model)?;
    }
    let request = observed
        .python_request()
        .map_err(|error| eyre::eyre!(error.to_owned()))?
        .context("Conda Python declaration is missing")?;
    let repository = LanguageRepository::open_readonly(context)?;
    let mut pinned = Vec::new();
    let mut held_existing_language = None;
    if bound.model().is_none() || admission.intent == PythonLockIntent::Update {
        let channels = super::conda::configured_channels(
            &context.settings().python.conda.channels,
            context.requested_data_root(),
        )?;
        let candidates = repository
            .enumerate_published()
            .map_err(|error| eyre::eyre!("language library is unavailable: {error:?}"))?;
        let previous_key = if admission.intent == PythonLockIntent::Update {
            bound.model().and_then(|model| model.conda().first())
                .and_then(|graph| graph.records.iter().find(|record| record.package_record.name.as_normalized() == "python"))
                .map(|record| {
                    let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(record)?;
                    Ok::<_, eyre::Report>(LanguageInstallSpec::from_plan(&plan, false)?.key())
                }).transpose()?
        } else { None };
        let mut ambiguous = false;
        let mut preferred_ready = false;
        for candidate in candidates {
            if candidate.source_kind() != PythonRuntimeSource::Conda
                || candidate.version() != request.version
                || !candidate.matches_host_platform()
            {
                continue;
            }
            let record = candidate
                .spec()
                .plan()
                .conda_record()
                .context("published Conda Python record is missing")?;
            if request
                .build
                .as_ref()
                .is_some_and(|build| build != &record.package_record.build)
            {
                continue;
            }
            if !allowed_sources(std::slice::from_ref(record), &channels)? {
                continue;
            }
            if let Some(channel) = &request.channel {
                let config = rattler_conda_types::ChannelConfig::default_with_root_dir(
                    std::path::PathBuf::from("/"),
                );
                let expected_channel = rattler_conda_types::Channel::from_str(channel, &config)?;
                let actual_channel = rattler_conda_types::Channel::from_str(
                    record
                        .channel
                        .as_deref()
                        .context("Conda Python channel is missing")?,
                    &config,
                )?;
                if expected_channel.base_url != actual_channel.base_url {
                    continue;
                }
            }
            let lease = repository
                .acquire_by_key(&candidate.key())?
                .context("installed Conda Python disappeared")?;
            let mut runtime_records = Vec::new();
            let mut allowed = true;
            for key in lease.runtime_dependencies() {
                let package = acquire_package(context, *key)?
                    .context("installed Conda Python runtime dependency is missing")?;
                let dependency = package
                    .manifest()
                    .conda
                    .clone()
                    .context("installed Conda runtime dependency record is missing")?;
                if !allowed_sources(std::slice::from_ref(&dependency), &channels)? {
                    allowed = false;
                    break;
                }
                runtime_records.push(dependency);
            }
            if !allowed {
                continue;
            }
            if previous_key.is_some_and(|key| key == candidate.key()) {
                pinned.clear();
                pinned.push(record.clone());
                pinned.extend(runtime_records);
                held_existing_language = Some(lease);
                preferred_ready = true;
                break;
            }
            if held_existing_language.is_some() {
                ambiguous = true;
                continue;
            }
            pinned.push(record.clone());
            pinned.extend(runtime_records);
            held_existing_language = Some(lease);
        }
        ensure!(preferred_ready || !ambiguous, "multiple installed Conda Python instances match; select a build");
    }
    let limits = LockModelLimits::default();
    let (mut model, locked, write_lock) = if let Some(model) = bound.model()
        && admission.intent == PythonLockIntent::Preserve
    {
        ensure!(
            model.graphs().is_empty() && model.conda().len() == 1 && model.mixed().len() <= 1,
            "unsupported Conda lock"
        );
        ensure!(
            pypi_requests.is_empty() == model.mixed().is_empty(),
            "locked mixed graph differs from the project declarations"
        );
        super::lock_document::validate_declarations(observed, model)?;
        if admission.validated_lock.as_ref() != Some(&model.conda()[0]) {
            // Recovery can replace the original observation; use the same
            // host snapshot for the newly observed locked graph.
            super::conda::validate_host(&model.conda()[0], &admission.host)?;
        }
        (model.clone(), model.conda()[0].clone(), false)
    } else {
        ensure!(!context.policy().locked, "locked Conda installation requires an existing lock");
        if let Some(previous) = bound.model() {
            ensure!(
                previous.graphs().is_empty() && previous.conda().len() == 1 && previous.mixed().len() <= 1,
                "existing lock contains unsupported additional graphs"
            );
        }
        let empty = PythonLockModel::from_parts(&[], &[], limits)?;
        let preferred = bound.model().map(|model| model.conda()[0].records.clone()).unwrap_or_default();
        let solved = super::conda::resolve_requests_into_lock(
            context,
            &admission.host,
            Some(request),
            &conda_requests,
            pinned,
            preferred,
            empty,
            String::new(),
            crate::programs::shared::commit::DEFAULT_GROUP.into(),
            [0; 32],
            limits,
        )
        .await?;
        let mut graph = solved
            .conda()
            .first()
            .context("Conda solver returned no graph")?
            .clone();
        let python = graph
            .records
            .iter()
            .find(|item| item.package_record.name.as_normalized() == "python")
            .context("Conda graph has no Python")?;
        let spec = LanguageInstallSpec::from_plan(
            &crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                python,
            )?,
            false,
        )?;
        graph.target_fingerprint = spec.key().target();
        let model =
            PythonLockModel::from_parts(&[], &[], limits)?.with_conda(graph.clone(), limits)?;
        (model, graph, true)
    };
    let mut by_name = BTreeMap::new();
    for record in &locked.records {
        by_name.insert(
            record.package_record.name.as_normalized().to_owned(),
            record,
        );
    }
    let python_record = *by_name.get("python").context("Conda graph has no Python")?;
    let mut runtime = BTreeSet::new();
    let mut runtime_order = Vec::new();
    let mut visiting = BTreeSet::new();
    for name in dependencies(python_record)? {
        visit(
            &name,
            &by_name,
            &mut visiting,
            &mut runtime,
            &mut runtime_order,
        )?;
    }
    let mut order = runtime_order.clone();
    let mut done = runtime.clone();
    for name in by_name.keys() {
        visit(name, &by_name, &mut visiting, &mut done, &mut order)?;
    }
    let mut python_bound = BTreeSet::new();
    for name in &order {
        if dependencies(by_name[name])?
            .iter()
            .any(|dependency| dependency == "python" || python_bound.contains(dependency))
        {
            python_bound.insert(name.clone());
        }
    }
    ensure!(
        runtime.is_disjoint(&python_bound),
        "Conda Python and its runtime packages form a dependency cycle [{}]; this is not supported yet",
        std::iter::once("python").chain(runtime.intersection(&python_bound).map(String::as_str)).collect::<Vec<_>>().join(", ")
    );

    let spec = LanguageInstallSpec::from_plan(
        &crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
            python_record,
        )?,
        false,
    )?;
    ensure!(
        locked.target_fingerprint == spec.key().target(),
        "Conda lock target differs from Python"
    );
    let mut leases = BTreeMap::new();
    let mut trees = BTreeMap::new();
    let mut transport = None;
    for name in &runtime_order {
        let (lease, tree) = publish_package(
            context,
            by_name[name],
            &locked.records,
            &leases,
            None,
            &mut transport,
            false,
        )
        .await?;
        if let Some(tree) = tree {
            trees.insert(name.clone(), tree);
        }
        leases.insert(name.clone(), lease);
    }
    let mut expected_runtime = runtime_order
        .iter()
        .map(|name| leases[name].manifest().key())
        .collect::<Vec<_>>();
    expected_runtime.sort_unstable();
    expected_runtime.dedup();
    let language = if let Some(lease) = repository.acquire_existing(&spec)? {
        ensure!(
            lease.spec().plan().conda_record() == spec.plan().conda_record()
                && lease.runtime_dependencies() == expected_runtime.as_slice(),
            "installed Conda Python has different fixed dependencies"
        );
        lease
    } else {
        let mut artifact = conda_artifact::acquire(context, python_record, &mut transport).await?;
        let tree = conda_artifact::extract(context, &mut artifact, python_record)?;
        for name in &runtime_order {
            if !trees.contains_key(name) {
                let mut artifact =
                    crate::libraries::package::conda::held_conda_artifact(context, &leases[name])?;
                trees.insert(
                    name.clone(),
                    conda_artifact::extract(context, &mut artifact, by_name[name])?,
                );
            }
        }
        let runtime_packages = runtime_order
            .iter()
            .map(|name| (&leases[name], &trees[name]))
            .collect::<Vec<_>>();
        crate::libraries::language::conda::install_conda(
            context,
            &artifact,
            &tree,
            &runtime_packages,
        )
        .await?
    };
    drop(held_existing_language);
    for name in order.iter().filter(|name| !runtime.contains(*name)) {
        let bound_language = dependencies(by_name[name])?
            .iter()
            .any(|dependency| dependency == "python")
            .then_some(&language);
        let (lease, tree) = publish_package(
            context,
            by_name[name],
            &locked.records,
            &leases,
            bound_language,
            &mut transport,
            true,
        )
        .await?;
        if let Some(tree) = tree {
            trees.insert(name.clone(), tree);
        }
        leases.insert(name.clone(), lease);
    }
    let names = by_name
        .keys()
        .filter(|name| name.as_str() != "python")
        .cloned()
        .collect::<Vec<_>>();
    let indices = names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::with_capacity(names.len());
    let mut edges = Vec::new();
    for name in &names {
        nodes.push(PackageGraphNode::from_conda_record(
            by_name[name],
            leases[name].manifest().key(),
        )?);
        for raw in &by_name[name].package_record.depends {
            let spec = MatchSpec::from_str(raw, ParseStrictness::Lenient)?;
            let name_dep = spec
                .name
                .as_exact()
                .context("Conda dependency name is not exact")?
                .as_normalized();
            if let Some(&target) = indices.get(name_dep) {
                edges.push(PackageGraphEdge {
                    from: indices[name],
                    to: target,
                    declaration: raw.clone(),
                });
            }
        }
    }
    let mut graph = PackageGraphRecord {
        roots: project_root_indices(python_record, &conda_requests, &indices)?,
        nodes,
        edges,
    };
    let mut packages = names
        .iter()
        .map(|name| leases[name].clone())
        .collect::<Vec<_>>();
    let projected_names = order
        .iter()
        .filter(|name| !runtime.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    let mut mixed = None;
    let layout = if pypi_requests.is_empty() {
        let fingerprint = graph.fingerprint()?;
        if let Some(layout) =
            crate::libraries::package::conda::acquire_conda_layout(context, &language, fingerprint)?
        {
            layout
        } else {
            for name in &projected_names {
                if !trees.contains_key(name) {
                    let mut artifact =
                        crate::libraries::package::conda::held_conda_artifact(context, &leases[name])?;
                    trees.insert(
                        name.clone(),
                        conda_artifact::extract_project(context, &mut artifact, by_name[name])?,
                    );
                }
            }
            let projected = projected_names
                .iter()
                .map(|name| (&leases[name], &trees[name]))
                .collect::<Vec<_>>();
            crate::libraries::package::conda::prepare_conda_layout(
                context, &language, fingerprint, &packages, &projected,
            ).await?
        }
    } else {
        for name in &names {
            if !trees.contains_key(name) {
                let mut artifact =
                    crate::libraries::package::conda::held_conda_artifact(context, &leases[name])?;
                trees.insert(
                    name.clone(),
                    if runtime.contains(name) {
                        conda_artifact::extract(context, &mut artifact, by_name[name])?
                    } else {
                        conda_artifact::extract_project(context, &mut artifact, by_name[name])?
                    },
                );
            }
        }
        let mut fixed = Vec::new();
        for name in &names {
            fixed.extend(
                crate::packages::python_resolution::source::registry_depot::conda_metadata::inspect_conda_distributions(
                    context, &leases[name], &trees[name],
                )?,
            );
        }
        let mut warm = BTreeMap::new();
        if !write_lock {
            let repository = crate::libraries::project::repository::ProjectRepository::open_readonly(context)?;
            for candidate in repository.read_views().conda_package_graphs(language.key().fingerprint())? {
                let candidate_graph = PackageGraphRecord {
                    roots: candidate.roots().to_vec(),
                    nodes: candidate.nodes().to_vec(),
                    edges: candidate.edges().to_vec(),
                };
                if crate::programs::shared::commit::validate_conda_lock(observed, &model, &language, &candidate_graph).is_err() {
                    continue;
                }
                if let Some(layout) = crate::libraries::package::lease::acquire_layout(context, *candidate.layout())? {
                    crate::programs::query::snapshot::validate_layout_graph(&candidate, &layout)?;
                    for package in layout.packages().iter().filter(|package| package.manifest().conda.is_none()) {
                        warm.insert(package.manifest().key(), package.clone());
                    }
                    break;
                }
            }
        }
        let preferred = bound.model().map(|model| model.pins().to_vec()).unwrap_or_default();
        let wheels = super::mixed::resolve_project_wheels(
            context, &language, &bound, &pypi_requests, fixed,
            &warm.into_values().collect::<Vec<_>>(), admission.intent, preferred,
        ).await?;
        if write_lock {
            let declarations = pypi_requests.iter()
                .map(|request| request.requirement().map_err(|error| eyre::eyre!(error)))
                .collect::<Result<Vec<_>>>()?;
            let (mixed_graph, pins) = wheels.lock_graph(&language, declarations)?;
            model = model.with_mixed(mixed_graph, &pins, &[], limits)?;
        }
        let mut mapping = Vec::with_capacity(wheels.graph.nodes.len());
        for node in &wheels.graph.nodes {
            let index = if let Some(index) = graph.nodes.iter().position(|existing| existing.instance == node.instance) {
                ensure!(graph.nodes[index] == *node, "mixed Conda package differs from the held graph");
                index
            } else {
                ensure!(node.pin.conda.is_none(), "mixed Conda package is absent from the held graph");
                let index = graph.nodes.len();
                graph.nodes.push(node.clone());
                index
            };
            mapping.push(index);
        }
        for edge in &wheels.graph.edges {
            let from = mapping[edge.from];
            let to = mapping[edge.to];
            if graph.nodes[from].pin.conda.is_some() {
                let package = packages.iter().find(|package| package.manifest().key() == graph.nodes[from].instance)
                    .context("mixed Conda dependency owner is missing")?;
                ensure!(
                    package.manifest().facts.dependencies.contains(&graph.nodes[to].instance),
                    "Conda Python metadata introduces an unsupported dependency outside its fixed Conda package graph"
                );
            }
            if !graph.edges.iter().any(|existing| existing.from == from && existing.to == to && existing.declaration == edge.declaration) {
                graph.edges.push(PackageGraphEdge { from, to, declaration: edge.declaration.clone() });
            }
        }
        graph.roots.extend(wheels.graph.roots.iter().map(|root| mapping[*root]));
        graph.roots.sort_unstable();
        graph.roots.dedup();
        packages.extend(wheels.wheels.iter().cloned());
        let fingerprint = graph.fingerprint()?;
        let layout = if let Some(layout) =
            crate::libraries::package::conda::acquire_conda_layout(context, &language, fingerprint)?
        {
            layout
        } else {
        let projected = projected_names
            .iter()
            .map(|name| (&leases[name], &trees[name]))
            .collect::<Vec<_>>();
        crate::libraries::package::conda::prepare_conda_layout(
            context, &language, fingerprint, &packages, &projected,
        ).await?
        };
        mixed = Some(wheels);
        layout
    };
    let bytes = if write_lock {
        Some(
            crate::programs::shared::commit::prepare_conda_lock_document(
                &bound,
                observed,
                &language,
                model.clone(),
                admission.intent == PythonLockIntent::Update,
            )?,
        )
    } else {
        None
    };
    crate::programs::shared::commit::validate_conda_lock(observed, &model, &language, &graph)?;
    observed.recheck()?;
    bound.recheck()?;
    if let Some(mixed) = &mixed {
        mixed.evidence.recheck()?;
    }
    let language_key = language.key();
    let layout_key = layout.key();
    drop(layout);
    drop(language);
    drop(mixed);
    drop(bound);
    let prepared = crate::programs::shared::commit::prepare_package_selection_commit_with_state(
        context,
        observed,
        language_key,
        layout_key,
        graph,
        expected,
    )?;
    if let Some(bytes) = bytes {
        crate::programs::shared::commit::commit_package_selection_with_lock(
            prepared, &path, &bytes,
        )?;
    } else {
        crate::programs::shared::commit::commit_package_selection(prepared)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{PackageName, PackageRecord, Version};

    fn record(name: &str, dependencies: &[&str]) -> RepoDataRecord {
        let mut package = PackageRecord::new(
            PackageName::from_str(name).unwrap(),
            Version::from_str("1.0").unwrap(),
            "build_0".into(),
        );
        package.depends = dependencies.iter().map(|value| (*value).into()).collect();
        RepoDataRecord {
            package_record: package,
            identifier: DistArchiveIdentifier::new(
                ArchiveIdentifier {
                    name: name.into(),
                    version: "1.0".into(),
                    build_string: "build_0".into(),
                },
                DistArchiveType::Conda(CondaArchiveType::Conda),
            ),
            url: format!("https://example.invalid/{name}-1.0-build_0.conda")
                .parse()
                .unwrap(),
            channel: Some("https://example.invalid/".into()),
        }
    }

    #[test]
    fn existing_python_and_every_runtime_package_must_use_configured_channels() {
        use rattler_solve::{
            ChannelPriority, SolveStrategy, SolverImpl, SolverTask, resolvo::Solver,
        };
        let channels = super::super::conda::configured_channels(
            &["https://example.invalid/b".into()],
            std::path::Path::new("/"),
        )
        .unwrap();
        let mut python = record("python", &["lib >=1"]);
        python.channel = Some("https://example.invalid/a/".into());
        python.package_record.sha256 = Some([1u8; 32].into());
        let mut runtime = record("lib", &[]);
        runtime.channel = Some("https://example.invalid/b/".into());
        assert!(!allowed_sources(std::slice::from_ref(&python), &channels).unwrap());
        let mut new_python = record("python", &[]);
        new_python.channel = Some("https://example.invalid/b/".into());
        new_python.url = "https://example.invalid/b/python-1.0-build_0.conda"
            .parse()
            .unwrap();
        new_python.package_record.sha256 = Some([2u8; 32].into());
        let mut pins = Vec::new();
        if allowed_sources(std::slice::from_ref(&python), &channels).unwrap() {
            pins.push(&python);
        }
        let solved = Solver
            .solve(SolverTask {
                available_packages: [std::slice::from_ref(&new_python)],
                locked_packages: vec![],
                pinned_packages: pins,
                virtual_packages: vec![],
                specs: vec![MatchSpec::from_str("python ==1.0", ParseStrictness::Strict).unwrap()],
                constraints: vec![],
                timeout: None,
                channel_priority: ChannelPriority::Strict,
                exclude_newer: None,
                strategy: SolveStrategy::Highest,
                dependency_overrides: vec![],
                excluded_candidates: Default::default(),
                cancellation_token: None,
            })
            .unwrap();
        assert_eq!(solved.records, vec![new_python]);
        python.channel = Some("https://example.invalid/b/".into());
        runtime.channel = Some("https://example.invalid/a/".into());
        assert!(!allowed_sources(std::slice::from_ref(&runtime), &channels).unwrap());
        runtime.channel = Some("https://example.invalid/b/".into());
        assert!(allowed_sources(&[python, runtime], &channels).unwrap());
    }

    #[test]
    fn project_roots_include_direct_declarations_and_python_runtime_only() {
        let python = record("python", &["runtime >=1", "__osx >=11"]);
        let runtime = record("runtime", &["transitive >=1"]);
        let indices = BTreeMap::from([
            ("direct".into(), 0),
            ("runtime".into(), 1),
            ("transitive".into(), 2),
        ]);
        let mut direct = PackageInstallRequest::pypi("python", "3.13.13", "direct", "1.0");
        direct.source = PackageSource::Conda;
        assert_eq!(
            project_root_indices(&python, &[direct], &indices).unwrap(),
            vec![0, 1]
        );
        let transitive = record("transitive", &[]);
        let records = BTreeMap::from([
            ("runtime".into(), &runtime),
            ("transitive".into(), &transitive),
        ]);
        let mut order = Vec::new();
        visit(
            "runtime",
            &records,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut order,
        )
        .unwrap();
        assert_eq!(order, ["transitive", "runtime"]);
    }

    #[test]
    fn original_owa_cycle_error_names_both_packages() {
        // Exact records from conda-forge/noarch repodata retained on 2026-10-29.
        // Only graph traversal is exercised; external dependencies are already done.
        let originals = [
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/fixtures/conda-cycles/owa-core-0.4.1-pyhd8ed1ab_0.conda.repodata-record.json")),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/fixtures/conda-cycles/owa-msgs-0.4.1-pyhd8ed1ab_0.conda.repodata-record.json")),
        ].map(|raw| {
            let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
            let filename = format!("{}-{}-{}.conda", value["name"].as_str().unwrap(), value["version"].as_str().unwrap(), value["build"].as_str().unwrap());
            value["fn"] = filename.clone().into();
            value["url"] = format!("https://conda.anaconda.org/conda-forge/noarch/{filename}").into();
            value["channel"] = "https://conda.anaconda.org/conda-forge/".into();
            serde_json::from_value::<RepoDataRecord>(value).unwrap()
        });
        let parent = record("consumer", &["owa-core"]);
        let by_name = originals.iter().chain([&parent])
            .map(|record| (record.package_record.name.as_normalized().to_owned(), record)).collect::<BTreeMap<_, _>>();
        for start in ["owa-core", "owa-msgs", "consumer"] {
            let mut done = originals.iter().flat_map(|record| dependencies(record).unwrap())
                .filter(|name| !by_name.contains_key(name)).collect();
            let error = visit(start, &by_name, &mut BTreeSet::new(), &mut done, &mut Vec::new()).unwrap_err().to_string();
            assert!(error.contains("cycle revisits owa-") && error.contains("owa-core") && error.contains("owa-msgs"), "{error}");
            assert!(error.contains("active packages"), "the active set must not be described as an exact cycle: {error}");
            if start == "consumer" { assert!(error.contains("consumer")); }
        }
    }

    #[test]
    fn runtime_dependencies_are_ordered_and_cycles_are_rejected() {
        let a = record("a", &["b >=1", "python >=3", "__osx >=11"]);
        let b = record("b", &[]);
        let records = BTreeMap::from([("a".into(), &a), ("b".into(), &b)]);
        let mut visiting = BTreeSet::new();
        let mut done = BTreeSet::new();
        let mut order = Vec::new();
        visit("a", &records, &mut visiting, &mut done, &mut order).unwrap();
        assert_eq!(order, ["b", "a"]);
        let cyclic = record("b", &["a >=1"]);
        let records = BTreeMap::from([("a".into(), &a), ("b".into(), &cyclic)]);
        assert!(
            visit(
                "a",
                &records,
                &mut BTreeSet::new(),
                &mut BTreeSet::new(),
                &mut Vec::new()
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "conda_project/cli_tests.rs"]
mod cli_tests;
