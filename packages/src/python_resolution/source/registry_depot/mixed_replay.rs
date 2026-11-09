//! Strict replay of mixed lock facts without running a solver.
use super::*;
use crate::packages::python_resolution::graph_check::{
    GraphCandidateObservation, RootMarkerScopes, derive_graph_scoped,
};
use crate::packages::python_resolution::lock_model::{MixedLockGraph, MixedLockSource, PinId};
use crate::packages::python_resolution::source::replay::metadata::{
    LockedMetadataState, route_locked_metadata,
};
use crate::packages::python_resolution::source::replay::{LockedReplayContext, acquire_locked_url};
use eyre::{ContextCompat, Result, ensure};
use pakro_resolver::python_native::{NativePlan, SelectedCandidate};
use std::collections::BTreeSet;

pub(crate) async fn replay<'root>(
    root: &'root BoundPythonLock,
    declarations: &[String],
    context: RegistryDepotContext<'_>,
    fixed: Vec<conda_metadata::CondaDistribution<'root>>,
    warm_packages: &[crate::libraries::package::lease::PackageLease],
    language: [u8; 32],
    target: [u8; 32],
) -> Result<closed::ClosedMixedRegistryGraph<'root>> {
    root.recheck()?;
    let model = root.model().context("mixed lock is missing")?;
    ensure!(
        model.mixed().len() == 1,
        "mixed lock graph is missing or ambiguous"
    );
    let graph = &model.mixed()[0];
    ensure!(
        graph.language == language
            && graph.target_fingerprint == target
            && graph.root_declarations == declarations,
        "mixed lock differs from current declarations or target"
    );
    let roots = declarations
        .iter()
        .map(|raw| parse_python_requirement(raw, context.limits.requirements))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut available = admit_fixed(
        fixed
            .into_iter()
            .map(|owner| (owner.facts().name().as_str().to_owned(), owner)),
        graph,
    )?;
    let mut conda = BTreeMap::new();
    let mut owners = BTreeMap::new();
    let mut selected = Vec::new();
    for node in &graph.nodes {
        root.recheck()?;
        ensure!(
            Instant::now() < context.deadline,
            "mixed lock replay deadline expired"
        );
        let key = candidate_key(&node.name, &node.version);
        match &node.source {
            MixedLockSource::Conda {
                source_pin,
                metadata_path,
                metadata_sha256,
            } => {
                let owner = available
                    .remove(&node.name)
                    .context("locked Conda distribution is missing")?;
                owner.recheck()?;
                ensure!(
                    owner.facts().version() == node.version
                        && owner.path().to_str() == Some(metadata_path.as_str())
                        && <[u8; 32]>::from(Sha256::digest(owner.metadata().raw()))
                            == *metadata_sha256
                        && crate::libraries::package::conda::record_source(owner.tree().record())?
                            == *source_pin,
                    "locked Conda distribution metadata changed"
                );
                conda.insert(key, owner);
            }
            MixedLockSource::Registry { pin, url } => {
                ensure!(
                    url.is_some() || model.has_locators(),
                    "mixed lock is missing the registry artifact URL"
                );
                let id = PinId::parse_hex(pin)?;
                let slot = root
                    .existing_pin(id)?
                    .context("locked PyPI artifact pin is missing")?;
                let endpoint = context.endpoints.iter().find(|endpoint| {
                    endpoint.project.registry.id().as_str() == slot.pin().file().registry_id()
                        && crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(endpoint.project.registry.canonical_base())
                            == *slot.pin().file().canonical_base_fingerprint()
                }).context("locked PyPI registry is not configured")?;
                let mut held = warm_packages.iter().filter(|package| {
                    package.manifest().conda.is_none()
                        && package.manifest().language == Some(language)
                        && package.manifest().facts.source == pin.as_str()
                });
                if let Some(package) = held.next() {
                    ensure!(
                        held.next().is_none(),
                        "multiple held wheels match the locked pin"
                    );
                    let wheel =
                        held_wheel::HeldRegistryWheel::new(slot, package.clone(), language)?;
                    ensure!(
                        wheel.facts().name().as_str() == node.name
                            && wheel.facts().version() == node.version,
                        "held PyPI metadata differs from the locked distribution"
                    );
                    owners.insert(key, RegistryMetadataOwner::Held(wheel));
                    selected.push(SelectedCandidate {
                        name: node.name.clone(),
                        key,
                        version: node.version.clone(),
                        requested_extras: Vec::new(),
                    });
                    continue;
                }
                let source = acquire_locked_url(
                    root,
                    slot,
                    LockedReplayContext {
                        client: context.client,
                        project_session: project_session(&endpoint.project),
                        file_session: file_session(&endpoint.file),
                        policy: context.fetch,
                        project_limits: context.limits.project,
                        source_limits: context.limits.source,
                        lock_limits: context.limits.lock,
                        cache_root: context.cache_root,
                        scratch_root: context.scratch_root,
                        deadline: context.deadline,
                    },
                )
                .await?;
                let wheel = match route_locked_metadata(
                    source,
                    context.supported,
                    context.limits.wheel,
                    context.limits.metadata,
                )? {
                    LockedMetadataState::Wheel(wheel) => wheel,
                    LockedMetadataState::NeedsBuild(_) => {
                        eyre::bail!("mixed locked source builds are not supported yet")
                    }
                };
                ensure!(
                    wheel.facts().name().as_str() == node.name
                        && wheel.facts().version() == node.version,
                    "locked PyPI metadata differs from the selected distribution"
                );
                owners.insert(key, RegistryMetadataOwner::Locked(wheel));
            }
        }
        selected.push(SelectedCandidate {
            name: node.name.clone(),
            key,
            version: node.version.clone(),
            requested_extras: Vec::new(),
        });
    }
    let observations = selected
        .iter()
        .map(|selected| {
            let facts = conda
                .get(&selected.key)
                .map(|owner| owner.facts())
                .or_else(|| owners.get(&selected.key).map(|owner| owner.facts()))
                .expect("locked selected owner");
            GraphCandidateObservation {
                key: selected.key,
                name: &selected.name,
                version: &selected.version,
                facts,
            }
        })
        .collect::<Vec<_>>();
    let derived = validate_graph(
        graph,
        &roots,
        &observations,
        context.environment,
        context.semantics,
        context.limits.markers,
    )?;
    for item in &mut selected {
        item.requested_extras = derived.extras.get(&item.key).cloned().unwrap_or_default();
    }
    let prepared = RegistryPreparedPlan {
        root,
        roots,
        root_scopes: None,
        environment: RegistryEnvironment::from_checked(context.environment),
        root_scope: RegistryRootScope::Environment,
        semantics: context.semantics,
        supported: context.supported.clone(),
        deadline: context.deadline,
        marker_limits: context.limits.markers,
        plan: NativePlan { selected },
        owners,
        policy_notices: BTreeMap::new(),
        usage: RegistryDepotUsage {
            retained_bytes: 0,
            retained_utf8: 0,
            steps: 0,
            projects: 0,
            metadata: 0,
            policy_work: 0,
            policy_utf8: 0,
        },
    };
    Ok(closed::close_mixed_registry_plan(
        MixedRegistryPreparedPlan { prepared, conda },
        Default::default(),
    )?)
}

// Admit real fixed owners before accessing a registry or selecting candidates.
fn admit_fixed<T>(
    fixed: impl IntoIterator<Item = (String, T)>,
    graph: &MixedLockGraph,
) -> Result<BTreeMap<String, T>> {
    let mut available = BTreeMap::new();
    for (name, owner) in fixed {
        ensure!(
            available.insert(name, owner).is_none(),
            "multiple held Conda distributions have the same name"
        );
    }
    for node in &graph.nodes {
        if matches!(&node.source, MixedLockSource::Registry { .. }) {
            ensure!(
                !available.contains_key(&node.name),
                "locked PyPI distribution would replace a fixed Conda distribution"
            );
        }
    }
    Ok(available)
}

/// Stable local identity for deriving the locked graph from retained metadata.
pub(crate) fn candidate_key(name: &str, version: &str) -> CandidateKey {
    let mut hash = Sha256::new();
    hash_field(&mut hash, b"pakro.mixed-lock-candidate/v1");
    hash_field(&mut hash, name.as_bytes());
    hash_field(&mut hash, version.as_bytes());
    CandidateKey(hash.finalize().into())
}

/// Recompute marker, extras, root and dependency semantics from real metadata.
pub(crate) fn validate_graph(
    graph: &MixedLockGraph,
    roots: &[PythonRequirementSyntax],
    observations: &[GraphCandidateObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
) -> Result<crate::packages::python_resolution::graph_check::DerivedGraphData> {
    ensure!(
        graph.marker_semantics == "current2026",
        "unsupported mixed marker semantics"
    );
    ensure!(
        observations.len() == graph.nodes.len(),
        "mixed metadata node count differs"
    );
    let derived = derive_graph_scoped(
        roots,
        observations,
        environment,
        RootMarkerScopes::Uniform(PythonMarkerScope::Environment),
        semantics,
        marker_limits,
        Default::default(),
    )?;
    let names = observations
        .iter()
        .map(|item| (item.key, item.name))
        .collect::<BTreeMap<_, _>>();
    let actual_roots = derived
        .graph
        .roots
        .iter()
        .map(|item| (item.declaration, names[&item.to]))
        .collect::<BTreeSet<_>>();
    let expected_roots = graph
        .roots
        .iter()
        .map(|item| (item.declaration_index, item.node.as_str()))
        .collect::<BTreeSet<_>>();
    ensure!(
        expected_roots.len() == graph.roots.len() && actual_roots == expected_roots,
        "mixed locked roots differ from actual metadata"
    );
    let facts = observations
        .iter()
        .map(|item| (item.key, item.facts))
        .collect::<BTreeMap<_, _>>();
    let actual_edges = derived
        .graph
        .edges
        .iter()
        .map(|edge| {
            let declaration = facts[&edge.from]
                .requirements()
                .get(edge.declaration)
                .expect("derived edge declaration");
            (
                names[&edge.from],
                names[&edge.to],
                declaration.raw_for_semantic_resolution(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_edges = graph
        .edges
        .iter()
        .map(|edge| {
            (
                edge.from.as_str(),
                edge.to.as_str(),
                edge.declaration.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        expected_edges.len() == graph.edges.len() && actual_edges == expected_edges,
        "mixed locked edges differ from actual metadata"
    );
    Ok(derived)
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::packages::python_resolution::lock_model::MixedLockNode;

    fn graph(nodes: Vec<MixedLockNode>) -> MixedLockGraph {
        MixedLockGraph {
            config_scope: String::new(),
            group: "default".into(),
            target_fingerprint: [0; 32],
            language: [0; 32],
            marker_semantics: "current2026".into(),
            root_declarations: Vec::new(),
            roots: Vec::new(),
            nodes,
            edges: Vec::new(),
        }
    }

    #[test]
    fn duplicate_fixed_names_are_rejected_without_changing_the_lock() {
        let lock = graph(Vec::new());
        let original = lock.clone();
        let fixed = vec![("numpy".into(), 1), ("numpy".into(), 2)];
        let error = admit_fixed(fixed, &lock).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple held Conda distributions")
        );
        assert_eq!(lock, original);
    }

    #[test]
    fn registry_cannot_replace_a_fixed_distribution() {
        let lock = graph(vec![MixedLockNode {
            name: "numpy".into(),
            version: "2.3.3".into(),
            source: MixedLockSource::Registry {
                pin: "locked-pin".into(),
                url: None,
            },
        }]);
        let original = lock.clone();
        let error = admit_fixed(vec![("numpy".into(), 1)], &lock).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("replace a fixed Conda distribution")
        );
        assert_eq!(lock, original);
    }
}
