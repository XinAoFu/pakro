//! Reuse a layout only when actual installed metadata proves the complete
//! current root intent has exactly this least dependency closure.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{ContextCompat, Result, ensure};
use pakro_index::python_markers::{MarkerSemantics, PythonMarkerEnvironment, PythonMarkerScope};
use pakro_index::python_requirements::{PythonRequirementSyntax, parse_python_requirement};
use pakro_resolver::python_native::CandidateKey;

use crate::libraries::package::lease::LayoutLease;
use crate::libraries::project::package_graph::CommittedGraph;
use crate::packages::python_resolution::graph_check::{
    GraphCandidateObservation, GraphCheckError, GraphCheckLimits, derive_graph,
};
use crate::programs::{OperationContext, shared::python_roots::PythonRoot};

pub(crate) fn matches_complete_roots(
    context: &OperationContext,
    graph: &CommittedGraph,
    layout: &LayoutLease,
    roots: &[PythonRoot],
) -> Result<bool> {
    context.checkpoint()?;
    crate::programs::query::snapshot::validate_layout_graph(graph, layout)?;
    let declarations = roots
        .iter()
        .map(|root| parse_python_requirement(&root.declaration, Default::default()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let packages = layout
        .packages()
        .iter()
        .map(|package| (package.manifest().key(), package))
        .collect::<BTreeMap<_, _>>();
    let limits = GraphCheckLimits::default();
    ensure!(
        graph.nodes().len() <= limits.max_nodes,
        "shared graph reuse node budget exceeded"
    );
    let mut remaining = limits.max_total_utf8_bytes;
    let mut facts = Vec::new();
    for node in graph.nodes() {
        context.checkpoint()?;
        let package = packages
            .get(node.instance())
            .context("shared graph node has no held package")?;
        let mut wheel_limits = crate::packages::python_wheel::WheelLimits::default();
        wheel_limits.max_metadata_bytes = wheel_limits.max_metadata_bytes.min(remaining);
        ensure!(
            wheel_limits.max_metadata_bytes > 0,
            "shared graph reuse metadata budget exceeded"
        );
        let bytes = package.read_wheel_metadata(wheel_limits.max_metadata_bytes)?;
        remaining = remaining
            .checked_sub(bytes.len())
            .context("shared graph reuse metadata budget exceeded")?;
        let metadata = crate::packages::python_wheel::parse_installed_core_metadata(
            bytes,
            node.name(),
            node.version(),
            wheel_limits,
        )?;
        facts.push(
            crate::packages::python_resolution::source::wheel_metadata::project_core_metadata(
                &metadata,
                node.name(),
                node.version(),
                Default::default(),
            )?,
        );
    }
    let observations = graph
        .nodes()
        .iter()
        .zip(&facts)
        .map(|(node, facts)| GraphCandidateObservation {
            key: CandidateKey(*node.instance()),
            name: node.name(),
            version: node.version(),
            facts,
        })
        .collect::<Vec<_>>();
    let expected_roots = graph
        .roots()
        .iter()
        .map(|index| {
            graph
                .nodes()
                .get(*index)
                .map(|node| CandidateKey(*node.instance()))
                .context("shared graph root node is missing")
        })
        .collect::<Result<Vec<_>>>()?;
    let expected_edges = graph
        .edges()
        .iter()
        .map(|edge| {
            Ok((
                CandidateKey(
                    *graph
                        .nodes()
                        .get(edge.from())
                        .context("shared graph edge source is missing")?
                        .instance(),
                ),
                CandidateKey(
                    *graph
                        .nodes()
                        .get(edge.to())
                        .context("shared graph edge target is missing")?
                        .instance(),
                ),
                edge.declaration(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let language = layout.language_lease();
    let result = matches_metadata(
        &declarations,
        &observations,
        &expected_roots,
        &expected_edges,
        language.target().environment(),
        language.target().semantics(),
    );
    // No metadata or graph descriptor can bypass full content verification.
    layout.check_binding()?;
    context.checkpoint()?;
    result
}

fn matches_metadata(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphCandidateObservation<'_>],
    expected_roots: &[CandidateKey],
    expected_edges: &[(CandidateKey, CandidateKey, &str)],
    environment: PythonMarkerEnvironment<'_>,
    semantics: MarkerSemantics,
) -> Result<bool> {
    let derived = match derive_graph(
        roots,
        nodes,
        environment,
        PythonMarkerScope::Environment,
        semantics,
        Default::default(),
        Default::default(),
    ) {
        Ok(derived) => derived,
        Err(GraphCheckError::Unsatisfied | GraphCheckError::NonMinimal) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let actual_roots = derived
        .graph
        .roots
        .iter()
        .map(|root| root.to)
        .collect::<BTreeSet<_>>();
    if actual_roots != expected_roots.iter().copied().collect::<BTreeSet<_>>() {
        return Ok(false);
    }
    let by_key = nodes
        .iter()
        .map(|node| (node.key, node.facts))
        .collect::<BTreeMap<_, _>>();
    let actual_edges = derived
        .graph
        .edges
        .iter()
        .map(|edge| {
            let declaration = by_key
                .get(&edge.from)
                .and_then(|facts| facts.requirements().get(edge.declaration))
                .context("derived shared graph dependency declaration is missing")?;
            Ok((
                edge.from,
                edge.to,
                declaration.raw_for_semantic_resolution(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    Ok(actual_edges == expected_edges.iter().copied().collect::<BTreeSet<_>>())
}

#[cfg(test)]
mod tests;
