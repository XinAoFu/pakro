//! Reconcile the old Conda lock with the existing project selection before update.
use std::collections::{BTreeMap, BTreeSet};

use eyre::{ContextCompat, Result, ensure};
use rattler_conda_types::{Channel, ChannelConfig, MatchSpec, ParseStrictness};

use crate::libraries::language::repository::LanguageInstallSpec;
use crate::libraries::project::{
    identity::ProjectIdentity, repository::ProjectRepository, snapshot::ObservedProjectInputs,
    state::ExpectedProjectState,
};
use crate::packages::python_resolution::lock_model::PythonLockModel;
use crate::programs::OperationContext;

/// Validate old facts without consulting the newly edited declarations.
pub(crate) fn validate_previous_selection(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    model: &PythonLockModel,
) -> Result<()> {
    context.checkpoint()?;
    observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(context, observed)?;
    ensure!(
        model.declaration_fingerprint().is_some()
            && model.graphs().is_empty()
            && model.conda().len() == 1
            && model.mixed().len() <= 1,
        "cannot update an incomplete or ambiguous Conda lock"
    );
    let locked = &model.conda()[0];
    let records: BTreeMap<_, _> = locked
        .records
        .iter()
        .map(|record| (record.package_record.name.as_normalized(), record))
        .collect();
    let python = *records
        .get("python")
        .context("old Conda lock lacks Python")?;
    let plan =
        crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
            python,
        )?;
    let spec = LanguageInstallSpec::from_plan(&plan, false)?;
    ensure!(
        locked.target_fingerprint == spec.key().target(),
        "old Conda lock target differs from Python"
    );
    let path = crate::programs::python::project_lock_path(observed)?;
    let bytes = observed
        .document(&path)
        .context("old Conda lock document is missing")?;
    let native = crate::lockfile::Lockfile::parse_observed(bytes)?;
    let entries = native
        .tools()
        .get("python")
        .context("old Conda runtime entry is missing")?;
    ensure!(entries.len() == 1, "old Conda runtime entry is ambiguous");
    let entry = &entries[0];
    let root = locked
        .roots
        .iter()
        .map(|raw| MatchSpec::from_str(raw, ParseStrictness::Strict))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .find(|root| {
            root.name
                .as_exact()
                .is_some_and(|name| name.as_normalized() == "python")
        })
        .context("old Conda lock lacks its Python root")?;
    let config = ChannelConfig::default_with_root_dir(std::path::PathBuf::from("/"));
    let channel = entry
        .options
        .get("channel")
        .map(|value| Channel::from_str(value, &config))
        .transpose()?;
    let build = entry
        .options
        .get("build")
        .cloned()
        .map(rattler_conda_types::match_spec::matcher::StringMatcher::Exact);
    let platform = entry
        .platforms
        .get(&crate::backend::platform_target::PlatformTarget::from_current().to_key())
        .context("old Conda runtime entry lacks the current platform")?;
    let digest = python
        .package_record
        .sha256
        .context("old Conda Python digest is missing")?;
    ensure!(
        entry.version == python.package_record.version.to_string()
            && entry.specifiers.len() == 1
            && entry.specifiers.contains(&entry.version)
            && entry.backend.as_deref() == Some("core:python")
            && entry.options.get("source").map(String::as_str) == Some("conda")
            && entry
                .options
                .keys()
                .all(|key| matches!(key.as_str(), "source" | "channel" | "build"))
            && root.channel.as_ref().map(|value| &value.base_url)
                == channel.as_ref().map(|value| &value.base_url)
            && root.build == build
            && platform.install.as_deref() == Some("conda")
            && platform.url.as_deref() == Some(python.url.as_str())
            && platform.checksum.as_deref()
                == Some(format!("sha256:{}", hex::encode(digest)).as_str())
            && platform.source_python.is_none()
            && platform.conda_deps.is_none()
            && platform.additional_artifacts.is_empty(),
        "old Conda runtime entry differs from its fixed graph"
    );

    let identity = ProjectIdentity::from_observation(observed)?;
    let mut repository = ProjectRepository::open_readonly(context)?;
    let expected = ExpectedProjectState::from_model(repository.model(), identity);
    if repository
        .model()
        .project_registration(identity.as_uuid())
        .is_none()
    {
        ensure!(
            !repository
                .model()
                .selection_slots()
                .any(|(key, stamp)| key.project() == identity.as_uuid()
                    && (stamp.generation().is_some() || stamp.language().is_some())),
            "project catalog has active selections without a registration"
        );
    } else {
        let graph = repository
            .read_views()
            .package_graph(&identity)?
            .context("registered project has no committed Conda package graph")?;
        ensure!(
            *graph.language() == spec.key().fingerprint(),
            "old Conda lock differs from the committed Python"
        );
        let layout = crate::libraries::package::lease::acquire_layout(context, *graph.layout())?
            .context("committed Conda layout is missing")?;
        crate::programs::query::snapshot::validate_layout_graph(&graph, &layout)?;
        ensure!(
            layout.language_lease().spec().plan().conda_record() == plan.conda_record(),
            "old Conda lock source differs from the held Python"
        );
        if !model.mixed().is_empty() {
            let mixed = &model.mixed()[0];
            ensure!(
                mixed.language == spec.key().fingerprint()
                    && mixed.target_fingerprint == spec.key().target(),
                "old mixed lock differs from the committed Python"
            );
            let record = crate::packages::python_environment::model::PackageGraphRecord {
                roots: graph.roots().to_vec(),
                nodes: graph.nodes().to_vec(),
                edges: graph.edges().to_vec(),
            };
            crate::programs::shared::commit::validate_mixed_catalog(model, &record)?;
            crate::programs::install::mixed::validate_held_metadata(
                context,
                model,
                layout.language_lease(),
                &layout,
            )?;
        } else {
            let packages: Vec<_> = locked
                .records
                .iter()
                .filter(|record| record.package_record.name.as_normalized() != "python")
                .collect();
            ensure!(
                packages.len() == graph.nodes().len(),
                "old Conda lock package count differs from catalog"
            );
            for record in packages {
                let node = graph
                    .nodes()
                    .iter()
                    .find(|node| node.name == record.package_record.name.as_normalized())
                    .context("old Conda lock package is absent from catalog")?;
                ensure!(
                    node.conda_record()?.as_ref() == Some(record)
                        && node.instance
                            == crate::libraries::package::conda::package_key(
                                record,
                                &locked.records
                            )?,
                    "old Conda lock package differs from catalog"
                );
                let mut edges = BTreeSet::new();
                for raw in &record.package_record.depends {
                    let dependency = MatchSpec::from_str(raw, ParseStrictness::Lenient)?;
                    let name = dependency
                        .name
                        .as_exact()
                        .context("old Conda dependency name is not exact")?
                        .as_normalized();
                    if name != "python" && records.contains_key(name) {
                        edges.insert((name.to_owned(), raw.as_str()));
                    }
                }
                let index = graph
                    .nodes()
                    .iter()
                    .position(|item| item.name == node.name)
                    .expect("node is in graph");
                let actual = graph
                    .edges()
                    .iter()
                    .filter(|edge| edge.from == index)
                    .map(|edge| {
                        (
                            graph.nodes()[edge.to].name.clone(),
                            edge.declaration.as_str(),
                        )
                    })
                    .collect::<BTreeSet<_>>();
                ensure!(
                    actual == edges,
                    "old Conda dependency edges differ from catalog"
                );
            }
            let mut roots = BTreeSet::new();
            for raw in locked.roots.iter().chain(&python.package_record.depends) {
                let spec = MatchSpec::from_str(raw, ParseStrictness::Lenient)?;
                let name = spec
                    .name
                    .as_exact()
                    .context("old Conda root name is not exact")?
                    .as_normalized();
                if name != "python" && records.contains_key(name) {
                    roots.insert(name.to_owned());
                }
            }
            let actual: BTreeSet<_> = graph
                .roots()
                .iter()
                .map(|index| graph.nodes()[*index].name.clone())
                .collect();
            ensure!(
                actual.len() == graph.roots().len() && actual == roots,
                "old Conda roots differ from catalog"
            );
        }
        let mut pending = vec![python];
        let mut reached = BTreeSet::new();
        let mut runtime = Vec::new();
        while let Some(record) = pending.pop() {
            let name = record.package_record.name.as_normalized();
            if !reached.insert(name) {
                continue;
            }
            if name != "python" {
                runtime.push(crate::libraries::package::conda::package_key(
                    record,
                    &locked.records,
                )?);
            }
            for raw in &record.package_record.depends {
                let dependency = MatchSpec::from_str(raw, ParseStrictness::Lenient)?;
                let name = dependency
                    .name
                    .as_exact()
                    .context("old Conda dependency name is not exact")?
                    .as_normalized();
                if let Some(record) = records.get(name) {
                    pending.push(*record);
                }
            }
        }
        runtime.sort_unstable();
        ensure!(
            runtime == layout.language_lease().runtime_dependencies(),
            "old Conda runtime dependencies differ from held Python"
        );
        layout.check_binding()?;
    }
    repository.reload()?;
    expected.ensure_current(repository.model())?;
    observed.recheck()?;
    context.checkpoint()?;
    Ok(())
}
