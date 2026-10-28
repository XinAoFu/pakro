//! Old committed results are soft candidate preferences, independent of the
//! new interpreter. Nothing here acquires or restores an old runtime payload.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Context, ContextCompat, Result, ensure};
use pakro_index::python_markers::{PythonMarkerScope, evaluate_python_marker};
use pakro_index::python_requirements::parse_python_requirement;
use pakro_resolver::solve::{Pep440, VersionAdapter};

use crate::libraries::language::{
    identity::LanguageKey,
    repository::{LanguageInstallSpec, LanguageRepository},
};
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::libraries::project::{
    identity::ProjectIdentity, repository::ProjectRepository, state::ExpectedProjectState,
};
use crate::packages::python_environment::model::PackageGraphRecord;
use crate::packages::python_resolution::lock_model::{
    ContentPin, LockedGraph, PythonLockModel, RuntimePin,
};
use crate::packages::python_resolution::lock_session::{BoundPythonLock, decode_root};
use crate::packages::python_resolution::source::locked_graph::FrozenRegistryTarget;
use crate::programs::{OperationContext, shared::commit::DEFAULT_GROUP};

#[derive(Debug, Clone, Copy)]
struct NativeRuntime {
    key: LanguageKey,
    current_platform: bool,
}

pub(crate) fn preferred_runtime_pins(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    root: &BoundPythonLock,
) -> Result<Vec<ContentPin>> {
    context.checkpoint()?;
    observed.recheck()?;
    root.recheck()?;
    let pins = if let Some(model) = root.model() {
        preferred_runtime_pins_from_observation(context, observed, model)?
    } else {
        // No portable Python result exists: preserve P2's first-lock rule.
        crate::programs::shared::lock_transaction::ensure_project_consistent(context, observed)?;
        Vec::new()
    };
    root.recheck()?;
    observed.recheck()?;
    context.checkpoint()?;
    Ok(pins)
}

/// Preflight/preview counterpart. The model must equal the bounded projection
/// decoded from the original readonly observation; no document guard is opened.
pub(crate) fn preferred_runtime_pins_from_observation(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    model: &PythonLockModel,
) -> Result<Vec<ContentPin>> {
    context.checkpoint()?;
    observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(context, observed)?;
    ensure!(
        model.declaration_fingerprint().is_some(),
        "cannot update an incomplete Python lock: original declaration fingerprint is missing"
    );
    ensure!(
        !model.graphs().is_empty(),
        "cannot update an incomplete Python lock: original runtime graph is missing"
    );
    let natives = native_runtimes(context, observed, model)?;
    validate_runtime_sources(model, &natives)?;
    for graph in model.graphs() {
        validate_root_syntax(context, graph)?;
    }
    let identity = ProjectIdentity::from_observation(observed)?;
    let mut repository = ProjectRepository::open_readonly(context)?;
    let expected = ExpectedProjectState::from_model(repository.model(), identity);
    let graph = if repository
        .model()
        .project_registration(identity.as_uuid())
        .is_some()
    {
        registered_graph(context, &repository, &identity, model, &natives)?
    } else {
        ensure!(
            !repository
                .model()
                .selection_slots()
                .any(|(key, stamp)| key.project() == identity.as_uuid()
                    && (stamp.generation().is_some() || stamp.language().is_some())),
            "project catalog has active selections without a registration"
        );
        relocated_graph(model, &natives)?
    };
    // Format and content-pin consistency were checked against the original
    // observation's decoder. These remain data preferences; acquisition verifies bytes and
    // the new target's candidate policy decides whether each file is usable.
    let pins = pins_for_graph(model, graph)?;
    repository.reload()?;
    expected.ensure_current(repository.model())?;
    observed.recheck()?;
    context.checkpoint()?;
    Ok(pins)
}

fn native_runtimes(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    model: &PythonLockModel,
) -> Result<Vec<NativeRuntime>> {
    use crate::plugins::core::python::{
        native::{pbs_filename, pipeline::RuntimeInstallPlan},
        pinned_shared_language_spec,
    };
    let current = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let mut runtimes = BTreeMap::<LanguageKey, bool>::new();
    let mut matching_documents = 0usize;
    for path in observed.paths().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "lock")
    }) {
        context.checkpoint()?;
        let bytes = observed
            .document(path)
            .context("observed lock document is missing")?;
        let projection = decode_root(bytes, Default::default())?;
        if projection.as_ref() != Some(model) {
            continue;
        }
        matching_documents += 1;
        let native = crate::lockfile::Lockfile::parse_observed(bytes)?;
        for (name, entries) in native.tools() {
            for entry in entries {
                if name != "python"
                    && !crate::programs::query::snapshot::python_tool_name(observed, name)
                    && !entry.backend.as_deref().is_some_and(|backend| {
                        matches!(backend, "python" | "core:python" | "asdf:python")
                    })
                {
                    continue;
                }
                crate::programs::shared::exact_version::exact_python_request(&entry.version)?;
                ensure!(
                    entry
                        .backend
                        .as_deref()
                        .is_none_or(|backend| matches!(backend, "python" | "core:python"))
                        && entry.options.iter().all(|(key,value)| key == "source" && matches!(value.as_str(), "prebuilt" | "source"))
                        && !entry.platforms.is_empty(),
                    "cannot update a Python lock with incomplete or unsupported original runtime facts"
                );
                for (platform, pin) in &entry.platforms {
                    context.checkpoint()?;
                    ensure!(
                        pin.url.is_some()
                            && pin.checksum.is_some()
                            && (pin.install.is_none() || pin.source_python.is_some())
                            && pin.conda_deps.is_none()
                            && pin.additional_artifacts.is_empty(),
                        "cannot update a Python lock without its complete original runtime source"
                    );
                    if let Some(input) = crate::libraries::language::source::input_from_pin(&entry.version, pin)? {
                        ensure!(entry.options.get("source").is_some_and(|value| value == "source"),
                            "old source CPython lock has no matching source declaration");
                        let key = input.language_key()?;
                        let record = pin.source_python.as_ref().expect("typed source input");
                        for graph in model.graphs().iter().filter(|graph| graph.runtime().content_tree() == &key.fingerprint()) {
                            ensure!(graph.runtime().target() == &record.runtime_target,
                                "old source CPython graph target differs from its actual original build result");
                        }
                        runtimes.entry(key).and_modify(|is_current| *is_current |= platform == &current).or_insert(platform == &current);
                        continue;
                    }
                    ensure!(!entry.options.get("source").is_some_and(|value| value == "source"),
                        "old source CPython lock is missing its complete build result");
                    RuntimeInstallPlan::validate_locked_pbs_entry(&entry.version, pin)?;
                    let url = url::Url::parse(pin.url.as_deref().expect("validated source URL"))?;
                    let filename = url
                        .path_segments()
                        .and_then(|mut parts| parts.next_back())
                        .context("original runtime artifact filename is missing")?;
                    let filename = urlencoding::decode(filename)?;
                    let (_, _, target, flavor) = pbs_filename(&filename)?
                        .context("original runtime source is not a supported CPython artifact")?;
                    let plan =
                        RuntimeInstallPlan::from_locked(&entry.version, &target, flavor, pin)?;
                    for patch in [false, true] {
                        let spec = if platform == &current {
                            pinned_shared_language_spec(
                                &entry.version,
                                pin,
                                patch,
                                context.settings(),
                            )?
                        } else {
                            LanguageInstallSpec::from_plan(&plan, patch)?
                        };
                        runtimes
                            .entry(spec.key())
                            .and_modify(|is_current| *is_current |= platform == &current)
                            .or_insert(platform == &current);
                        ensure!(
                            runtimes.len() <= 8192,
                            "original runtime identity budget exceeded"
                        );
                    }
                }
            }
        }
    }
    ensure!(
        matching_documents == 1,
        "old Python lock is not bound to one complete observed document"
    );
    ensure!(
        !runtimes.is_empty(),
        "cannot update an incomplete Python lock: original native runtime source is missing"
    );
    Ok(runtimes
        .into_iter()
        .map(|(key, current_platform)| NativeRuntime {
            key,
            current_platform,
        })
        .collect())
}

fn validate_runtime_sources(model: &PythonLockModel, natives: &[NativeRuntime]) -> Result<()> {
    let identities: BTreeSet<_> = natives
        .iter()
        .map(|runtime| runtime.key.fingerprint())
        .collect();
    for graph in model.graphs() {
        ensure!(
            graph.key().config_scope().is_empty() && graph.key().group() == "shared-runtime",
            "cannot identify an unsupported old Python runtime graph for lock update"
        );
        ensure!(
            identities.contains(graph.runtime().content_tree()),
            "old Python runtime graph conflicts with its original native source or extraction recipe"
        );
    }
    Ok(())
}

fn relocated_graph<'a>(
    model: &'a PythonLockModel,
    natives: &[NativeRuntime],
) -> Result<&'a LockedGraph> {
    let identities: BTreeSet<_> = natives
        .iter()
        .filter(|runtime| runtime.current_platform)
        .map(|runtime| runtime.key.fingerprint())
        .collect();
    let current = model
        .graphs()
        .iter()
        .filter(|graph| identities.contains(graph.runtime().content_tree()))
        .collect::<Vec<_>>();
    if current.len() == 1 {
        return Ok(current[0]);
    }
    ensure!(
        current.is_empty(),
        "old lock has several runtime graphs for this platform; cannot identify the previous active result"
    );
    ensure!(
        model.graphs().len() == 1,
        "old lock has several runtime graphs and no identifiable previous selection; lock update is ambiguous"
    );
    Ok(&model.graphs()[0])
}

fn registered_graph<'a>(
    context: &OperationContext,
    repository: &ProjectRepository,
    identity: &ProjectIdentity,
    model: &'a PythonLockModel,
    natives: &[NativeRuntime],
) -> Result<&'a LockedGraph> {
    let mut slots = repository.model().selection_slots().filter(|(key, stamp)| {
        key.project() == identity.as_uuid()
            && key.group() == DEFAULT_GROUP
            && (stamp.generation().is_some() || stamp.language().is_some())
    });
    let (slot, _) = slots
        .next()
        .context("registered project has no complete previous default selection")?;
    ensure!(
        slots.next().is_none(),
        "registered project has several active default selections"
    );
    let selection = repository
        .read_views()
        .language_selection(identity)
        .context("registered project has no complete previous language identity")?;
    let old_language = *selection.language();
    ensure!(
        natives
            .iter()
            .any(|runtime| runtime.key.fingerprint() == old_language
                && runtime.key.target() == *slot.target()),
        "old native runtime source conflicts with the committed project language or target"
    );
    let published = LanguageRepository::open_readonly(context)?
        .enumerate_published()
        .map_err(|error| eyre::eyre!("cannot verify the previous runtime metadata: {error:?}"))?;
    let language = published.into_iter().find(|language| language.key().fingerprint() == old_language)
        .context("cannot verify the old runtime target: its recorded language metadata is missing; updating the lock would erase unverifiable old facts")?;
    ensure!(
        language.key().target() == *slot.target(),
        "committed project target differs from the old runtime metadata"
    );
    let target = language
        .recorded_target()
        .wrap_err("old runtime metadata is inconsistent")?;
    let expected_runtime = RuntimePin::new(old_language, 1, *target.fingerprint())?;
    let matching = model
        .graphs()
        .iter()
        .filter(|graph| graph.runtime() == expected_runtime)
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1,
        "old Python lock lacks the exact committed runtime/target graph"
    );
    let graph = matching[0];
    validate_old_roots(context, graph, &target)?;
    let portable = super::lock::portable_graph(model, graph)?;
    let catalog_graph = repository.read_views().package_graph(identity)?;
    let expected = catalog_graph
        .as_ref()
        .map(|graph| graph.fingerprint())
        .unwrap_or(
            PackageGraphRecord {
                roots: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            }
            .fingerprint()?,
        );
    ensure!(
        portable.fingerprint()? == expected,
        "old Python lock package roots, dependencies, sources or artifacts conflict with the committed project selection"
    );
    Ok(graph)
}

fn validate_old_roots(
    context: &OperationContext,
    graph: &LockedGraph,
    target: &FrozenRegistryTarget,
) -> Result<()> {
    let references: BTreeMap<_, _> = graph
        .roots()
        .iter()
        .map(|root| (root.declaration_index(), root.node()))
        .collect();
    let nodes: BTreeMap<_, _> = graph
        .nodes()
        .iter()
        .map(|node| (node.name(), node))
        .collect();
    let mut active = 0usize;
    for (index, declaration) in graph.root_declarations().iter().enumerate() {
        context.checkpoint()?;
        let syntax = parse_python_requirement(declaration, Default::default())?;
        ensure!(
            syntax.unresolved_reference().is_none(),
            "old direct declaration uses an unsupported source reference"
        );
        let version = syntax
            .raw_version_specifiers()
            .and_then(|raw| raw.trim().strip_prefix("=="))
            .filter(|version| !version.starts_with('='))
            .context("old direct declaration lacks a complete exact version")?;
        let version = Pep440::parse_version(version.trim())
            .context("old direct declaration has an invalid exact version")?;
        let enabled = match syntax.marker() {
            None => true,
            Some(marker) => evaluate_python_marker(
                marker,
                target.environment(),
                PythonMarkerScope::Environment,
                target.semantics(),
                Default::default(),
            )?
            .value(),
        };
        if !enabled {
            continue;
        }
        active += 1;
        let name = pakro_index::python_source::NormalizedPythonName::new(syntax.name())?;
        ensure!(
            references
                .get(&index)
                .is_some_and(|node| *node == name.as_str()),
            "old locked active roots differ from their original declarations"
        );
        ensure!(
            nodes
                .get(name.as_str())
                .is_some_and(
                    |node| Pep440::parse_version(node.version()).as_ref() == Some(&version)
                ),
            "old locked root version differs from its original exact declaration"
        );
    }
    ensure!(
        active == graph.roots().len(),
        "old locked roots contain an inactive or unrecognized declaration"
    );
    Ok(())
}

fn validate_root_syntax(context: &OperationContext, graph: &LockedGraph) -> Result<()> {
    let references: BTreeMap<_, _> = graph
        .roots()
        .iter()
        .map(|root| (root.declaration_index(), root.node()))
        .collect();
    let nodes: BTreeMap<_, _> = graph
        .nodes()
        .iter()
        .map(|node| (node.name(), node))
        .collect();
    for (index, declaration) in graph.root_declarations().iter().enumerate() {
        context.checkpoint()?;
        let syntax = parse_python_requirement(declaration, Default::default())?;
        ensure!(
            syntax.unresolved_reference().is_none(),
            "old direct declaration uses an unsupported source reference"
        );
        let version = syntax
            .raw_version_specifiers()
            .and_then(|raw| raw.trim().strip_prefix("=="))
            .filter(|version| !version.starts_with('='))
            .context("old direct declaration lacks a complete exact version")?;
        let version = Pep440::parse_version(version.trim())
            .context("old direct declaration has an invalid exact version")?;
        let name = pakro_index::python_source::NormalizedPythonName::new(syntax.name())?;
        if let Some(node) = references.get(&index) {
            ensure!(
                *node == name.as_str()
                    && nodes
                        .get(name.as_str())
                        .is_some_and(
                            |node| Pep440::parse_version(node.version()).as_ref() == Some(&version)
                        ),
                "old locked root does not match its original exact declaration"
            );
        } else {
            ensure!(
                syntax.marker().is_some(),
                "old lock omits an unconditional direct root"
            );
        }
    }
    Ok(())
}

fn pins_for_graph(model: &PythonLockModel, graph: &LockedGraph) -> Result<Vec<ContentPin>> {
    let pins: BTreeMap<_, _> = model.pins().iter().map(|pin| (pin.id(), pin)).collect();
    graph
        .nodes()
        .iter()
        .map(|node| {
            pins.get(&node.source_pin())
                .map(|pin| (*pin).clone())
                .context("old active runtime graph is missing its exact content pin")
        })
        .collect()
}

#[cfg(test)]
mod tests;
