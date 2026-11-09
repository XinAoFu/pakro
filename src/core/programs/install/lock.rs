//! Portable exact shared-Python graph mapping. Descriptions grant no IO rights.
use std::collections::{BTreeMap, BTreeSet};

use eyre::{Result, ensure};

use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::model::{
    PackageGraphEdge, PackageGraphNode, PackageGraphRecord, PackageSourcePin, SourceBuildRecord,
};
use crate::packages::python_resolution::lock_model::{
    ContentPin, EdgeInput, GroupKey, LockGraphInput, LockMarkerSemantics, LockedGraph, NodeInput,
    PinEvidence, PinId, PythonLockModel, RootInput, RuntimePin,
};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::programs::shared::python_roots::{PythonLockIntent, PythonRoot};

/// Pure descriptor mapping. Runtime acquisition, query and commit boundaries
/// own complete lease validation; this result grants no object capability.
pub(crate) fn group_key(language: &LanguageLease) -> Result<GroupKey> {
    Ok(GroupKey::new(
        "",
        "shared-runtime",
        *language.target().fingerprint(),
        Default::default(),
    )?)
}

/// Portable data from the same held descriptor; not a new integrity proof.
pub(crate) fn runtime_pin(language: &LanguageLease) -> Result<RuntimePin> {
    Ok(RuntimePin::new(
        language.key().fingerprint(),
        1,
        *language.target().fingerprint(),
    )?)
}

pub(crate) fn validate_project_lock<'lock>(
    root: &'lock BoundPythonLock,
    language: &LanguageLease,
    roots: &[(String, String)],
) -> Result<&'lock LockedGraph> {
    let roots = plain_roots(roots)?;
    validate_project_lock_roots(root, language, &roots)
}

pub(crate) fn validate_project_lock_roots<'lock>(
    root: &'lock BoundPythonLock,
    language: &LanguageLease,
    roots: &[PythonRoot],
) -> Result<&'lock LockedGraph> {
    root.recheck()?;
    let model = root
        .model()
        .ok_or_else(|| eyre::eyre!("strict Python lock has no exact graph model"))?;
    let graph = validate_project_lock_model_roots(model, language, roots)?;
    root.recheck()?;
    Ok(graph)
}

pub(crate) fn validate_project_lock_model<'lock>(
    model: &'lock PythonLockModel,
    language: &LanguageLease,
    roots: &[(String, String)],
) -> Result<&'lock LockedGraph> {
    let roots = plain_roots(roots)?;
    validate_project_lock_model_roots(model, language, &roots)
}

pub(crate) fn validate_project_lock_model_roots<'lock>(
    model: &'lock PythonLockModel,
    language: &LanguageLease,
    roots: &[PythonRoot],
) -> Result<&'lock LockedGraph> {
    let key = group_key(language)?;
    let graph = model.graph(&key).ok_or_else(|| {
        eyre::eyre!("strict Python lock is missing the current complete target graph")
    })?;
    let declarations = crate::programs::shared::python_roots::declarations(roots);
    ensure!(
        graph.root_declarations() == declarations,
        "strict Python lock differs from the current complete direct roots"
    );
    let expected = runtime_pin(language)?;
    ensure!(
        graph.runtime() == expected,
        "strict Python lock differs from the actual selected language source or target"
    );
    let active = active_indices(roots, language)?;
    ensure!(
        graph.roots().len() == active.len(),
        "strict Python lock has a different active direct-root set"
    );
    for (index, root) in active {
        ensure!(
            graph
                .roots()
                .iter()
                .any(|locked| locked.declaration_index() == index && locked.node() == root.name),
            "strict Python lock differs from the active root declarations"
        );
        ensure!(graph.nodes().iter().any(|node| node.name() == root.name && same_version(node.version(), &root.version)),
            "strict Python lock differs from an exact active root version");
    }
    for node in graph.nodes() {
        if let Some(pin) = model
            .pins()
            .iter()
            .find(|pin| pin.id() == node.source_pin())
        {
            if pin.file().kind()
                == crate::packages::python_resolution::lock_model::RegistryArtifactKind::Sdist
            {
                ensure!(
                    model
                        .source_build(node.source_pin())
                        .is_some_and(|record| record.observation.is_some()),
                    "strict Python source lock is missing its original build observations"
                );
            }
        }
    }
    Ok(graph)
}

pub(crate) fn project_lock_model(
    root: &BoundPythonLock,
    language: &LanguageLease,
    graph: &PackageGraphRecord,
    roots: &[(String, String)],
    builds: &[(PinId, SourceBuildRecord)],
) -> Result<PythonLockModel> {
    let roots = plain_roots(roots)?;
    project_lock_model_roots(
        root,
        language,
        graph,
        &roots,
        builds,
        PythonLockIntent::Preserve,
    )
}

pub(crate) fn project_lock_model_roots(
    root: &BoundPythonLock,
    language: &LanguageLease,
    graph: &PackageGraphRecord,
    roots: &[PythonRoot],
    builds: &[(PinId, SourceBuildRecord)],
    intent: PythonLockIntent,
) -> Result<PythonLockModel> {
    root.recheck()?;
    let key = group_key(language)?;
    let raw = crate::programs::shared::python_roots::declarations(roots);
    let active = active_indices(roots, language)?;
    ensure!(
        graph.roots.len() == active.len(),
        "resolved graph has a different active direct-root set"
    );
    let declarations: Vec<_> = raw.iter().map(String::as_str).collect();
    let mut pins: BTreeMap<_, ContentPin> = root
        .model()
        .into_iter()
        .flat_map(|m| m.pins())
        .map(|pin| (pin.id(), pin.clone()))
        .collect();
    let mut nodes = Vec::new();
    for node in &graph.nodes {
        let pin = node.content_pin()?;
        pins.insert(pin.id(), pin.clone());
        nodes.push(NodeInput {
            name: &node.name,
            version: &node.version,
            source_pin: pin.id(),
        });
    }
    let root_inputs: Vec<_> =
        active
            .into_iter()
            .map(|(index, root)| {
                ensure!(
                    graph.roots.iter().any(|i| graph
                        .nodes
                        .get(*i)
                        .is_some_and(|node| node.name == root.name
                            && same_version(&node.version, &root.version))),
                    "resolved graph differs from its exact direct roots"
                );
                Ok(RootInput {
                    declaration_index: index,
                    node: &root.name,
                })
            })
            .collect::<Result<_>>()?;
    let edges: Vec<_> = graph
        .edges
        .iter()
        .map(|edge| {
            let from = graph
                .nodes
                .get(edge.from)
                .ok_or_else(|| eyre::eyre!("lock graph edge source is missing"))?;
            let to = graph
                .nodes
                .get(edge.to)
                .ok_or_else(|| eyre::eyre!("lock graph edge destination is missing"))?;
            Ok(EdgeInput {
                from: &from.name,
                to: &to.name,
                declaration: &edge.declaration,
            })
        })
        .collect::<Result<_>>()?;
    let old: Vec<_> = root
        .model()
        .into_iter()
        .flat_map(|m| m.graphs())
        .filter(|g| intent == PythonLockIntent::Preserve && g.key() != &key)
        .map(GraphView::new)
        .collect();
    let mut groups: Vec<_> = old.iter().map(GraphView::input).collect();
    groups.push(LockGraphInput {
        key: &key,
        semantics: LockMarkerSemantics::Current2026,
        runtime: runtime_pin(language)?,
        root_declarations: &declarations,
        roots: &root_inputs,
        nodes: &nodes,
        edges: &edges,
    });
    let mut all_builds: BTreeMap<_, _> = root
        .model()
        .into_iter()
        .flat_map(|m| m.builds())
        .cloned()
        .collect();
    for node in &graph.nodes {
        if let Some(record) = &node.pin.build {
            all_builds.insert(PinId::parse_hex(&node.source_pin)?, record.clone());
        }
    }
    for (id, record) in builds {
        all_builds.insert(*id, record.clone());
    }
    let mut used: BTreeSet<_> = groups
        .iter()
        .flat_map(|g| g.nodes.iter().map(|node| node.source_pin))
        .collect();
    let mut pending: Vec<_> = used.iter().copied().collect();
    while let Some(id) = pending.pop() {
        if let Some(record) = all_builds.get(&id) {
            ensure!(
                record.observation.is_some(),
                "source build lacks original metadata observations; explicit preparation is required before producing a strict lock"
            );
            for input in record
                .inputs
                .iter()
                .chain(record.observation.iter().flat_map(|o| &o.initial_inputs))
            {
                let pin = input.content_pin()?;
                pins.insert(pin.id(), pin.clone());
                if used.insert(pin.id()) {
                    pending.push(pin.id());
                }
            }
        }
    }
    let builds: Vec<_> = all_builds
        .into_iter()
        .filter(|(id, _)| used.contains(id))
        .collect();
    let pins: Vec<_> = pins
        .into_values()
        .filter(|p| used.contains(&p.id()))
        .collect();
    let model =
        PythonLockModel::from_parts_with_builds(&pins, &groups, &builds, Default::default())?;
    root.recheck()?;
    Ok(model)
}

/// Portable projection for warm catalog comparison; local instance IDs never
/// enter the project lock and are recomputed by the actual publisher.
pub(crate) fn portable_graph(
    model: &PythonLockModel,
    graph: &LockedGraph,
) -> Result<PackageGraphRecord> {
    let indices: BTreeMap<_, _> = graph
        .nodes()
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name(), i))
        .collect();
    let pins: BTreeMap<_, _> = model.pins().iter().map(|pin| (pin.id(), pin)).collect();
    let nodes = graph
        .nodes()
        .iter()
        .map(|node| {
            let pin = pins
                .get(&node.source_pin())
                .ok_or_else(|| eyre::eyre!("locked graph source pin is missing"))?;
            let build = model.source_build(pin.id()).cloned();
            ensure!(
                pin.file().kind()
                    != crate::packages::python_resolution::lock_model::RegistryArtifactKind::Sdist
                    || build.is_some(),
                "strict source graph is missing its build output receipt"
            );
            Ok(PackageGraphNode {
                name: node.name().into(),
                version: node.version().into(),
                instance: [0; 32],
                source_pin: hex::encode(pin.id().as_bytes()),
                artifact: build
                    .as_ref()
                    .map_or_else(|| pin.digest().to_string(), |b| b.output_digest.clone()),
                pin: PackageSourcePin {
                    registry: pin.file().registry_id().into(),
                    base: *pin.file().canonical_base_fingerprint(),
                    filename: pin.file().filename().into(),
                    size: pin.size(),
                    advertised: pin.evidence() == PinEvidence::Advertised,
                    build,
                    conda: None,
                },
            })
        })
        .collect::<Result<_>>()?;
    let roots = graph
        .roots()
        .iter()
        .map(|root| indices[root.node()])
        .collect();
    let edges = graph
        .edges()
        .iter()
        .map(|edge| PackageGraphEdge {
            from: indices[edge.from()],
            to: indices[edge.to()],
            declaration: edge.declaration().into(),
        })
        .collect();
    Ok(PackageGraphRecord {
        roots,
        nodes,
        edges,
    })
}

fn plain_roots(roots: &[(String, String)]) -> Result<Vec<PythonRoot>> {
    roots
        .iter()
        .map(|(name, version)| PythonRoot::plain(name, version))
        .collect()
}

fn active_indices<'a>(
    roots: &'a [PythonRoot],
    language: &LanguageLease,
) -> Result<Vec<(usize, &'a PythonRoot)>> {
    roots
        .iter()
        .enumerate()
        .filter_map(|(index, root)| match root.active(language) {
            Ok(true) => Some(Ok((index, root))),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn same_version(left: &str, right: &str) -> bool {
    use pakro_resolver::solve::{Pep440, VersionAdapter};
    match (Pep440::parse_version(left), Pep440::parse_version(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}
struct GraphView<'a> {
    graph: &'a LockedGraph,
    raw: Vec<&'a str>,
    roots: Vec<RootInput<'a>>,
    nodes: Vec<NodeInput<'a>>,
    edges: Vec<EdgeInput<'a>>,
}
impl<'a> GraphView<'a> {
    fn new(graph: &'a LockedGraph) -> Self {
        Self {
            graph,
            raw: graph
                .root_declarations()
                .iter()
                .map(String::as_str)
                .collect(),
            roots: graph
                .roots()
                .iter()
                .map(|r| RootInput {
                    declaration_index: r.declaration_index(),
                    node: r.node(),
                })
                .collect(),
            nodes: graph
                .nodes()
                .iter()
                .map(|n| NodeInput {
                    name: n.name(),
                    version: n.version(),
                    source_pin: n.source_pin(),
                })
                .collect(),
            edges: graph
                .edges()
                .iter()
                .map(|e| EdgeInput {
                    from: e.from(),
                    to: e.to(),
                    declaration: e.declaration(),
                })
                .collect(),
        }
    }
    fn input(&self) -> LockGraphInput<'_> {
        LockGraphInput {
            key: self.graph.key(),
            semantics: self.graph.semantics(),
            runtime: self.graph.runtime(),
            root_declarations: &self.raw,
            roots: &self.roots,
            nodes: &self.nodes,
            edges: &self.edges,
        }
    }
}

#[cfg(test)]
mod tests;
