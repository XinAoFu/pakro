//! Recompute the least dependency closure from original declarations.
//!
//! These checked rows are data, never source, lock or installation authority.
//! The actual owning depot must keep all observed wheel owners alive.

use super::metadata_input::{MetadataInputError, checked_metadata_context};
use super::root_scopes::OwnedRootMarkerScope;
use pakro_index::python_markers::{
    MarkerSemantics, PythonMarkerEnvironment, PythonMarkerExpression, PythonMarkerLimits,
    PythonMarkerScope, PythonMarkerWarning, evaluate_python_marker,
};
use pakro_index::python_metadata::PythonDependencyMetadataFacts;
use pakro_index::python_requirements::PythonRequirementSyntax;
use pakro_index::python_requires_matches_for_context;
use pakro_index::python_source::NormalizedPythonName;
use pakro_resolver::python_native::{CandidateKey, SelectedCandidate};
use pakro_resolver::pyver::{
    RawVersionConstraint, RawVersionConstraintError, RawVersionConstraintLimits,
};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphCheckLimits {
    pub max_roots: usize,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_extra_references: usize,
    pub max_total_utf8_bytes: usize,
    pub max_evaluation_nodes: usize,
}
impl Default for GraphCheckLimits {
    fn default() -> Self {
        Self {
            max_roots: 65_536,
            max_nodes: 65_536,
            max_edges: 1_048_576,
            max_extra_references: 1_048_576,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            max_evaluation_nodes: 16_777_216,
        }
    }
}
impl GraphCheckLimits {
    pub(crate) fn validate(self) -> Result<(), GraphCheckError> {
        if [
            self.max_roots,
            self.max_nodes,
            self.max_edges,
            self.max_extra_references,
            self.max_total_utf8_bytes,
            self.max_evaluation_nodes,
        ]
        .contains(&0)
        {
            return Err(GraphCheckError::InvalidLimits);
        }
        Ok(())
    }
}
pub(crate) struct GraphNodeObservation<'a> {
    pub selected: &'a SelectedCandidate,
    pub facts: &'a PythonDependencyMetadataFacts,
}
pub(crate) struct GraphCandidateObservation<'a> {
    pub key: CandidateKey,
    pub name: &'a str,
    pub version: &'a str,
    pub facts: &'a PythonDependencyMetadataFacts,
}
#[derive(Debug)]
pub(crate) struct DerivedGraphData {
    pub graph: CheckedGraphData,
    pub extras: BTreeMap<CandidateKey, Vec<String>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckedRoot {
    pub declaration: usize,
    pub to: CandidateKey,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckedEdge {
    pub from: CandidateKey,
    pub declaration: usize,
    pub to: CandidateKey,
}
#[derive(Debug)]
pub(crate) struct GraphMarkerWarnings {
    pub owner: Option<CandidateKey>,
    pub declaration: usize,
    pub warnings: Vec<PythonMarkerWarning>,
}
#[derive(Debug)]
pub(crate) struct CheckedGraphData {
    pub roots: Vec<CheckedRoot>,
    pub edges: Vec<CheckedEdge>,
    pub warnings: Vec<GraphMarkerWarnings>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphSourceLocation {
    Root {
        declaration: usize,
    },
    Dependency {
        owner: CandidateKey,
        declaration: usize,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum GraphCheckError {
    #[error("graph check limits must be positive")]
    InvalidLimits,
    #[error("graph check budget exceeded")]
    Limit,
    #[error("graph check inputs are inconsistent")]
    InvalidInput,
    #[error("graph does not satisfy an active original declaration")]
    Unsatisfied,
    #[error("graph contains unreachable distributions or unrequested extras")]
    NonMinimal,
    #[error("selected distribution has incompatible Requires-Python")]
    RequiresPython,
    #[error("active source reference requires its bound source adapter")]
    SourceReference(GraphSourceLocation),
    #[error("original marker could not be evaluated")]
    Marker,
}

/// Each declaration is evaluated in the context defined by its containing
/// layer. A dependency group does not inherit a project's selected extras.
#[derive(Clone, Copy)]
pub(crate) enum RootMarkerScopes<'a> {
    Uniform(PythonMarkerScope<'a>),
    PerDeclaration(&'a [PythonMarkerScope<'a>]),
    OwnedDeclarations(&'a [OwnedRootMarkerScope]),
}
impl<'a> RootMarkerScopes<'a> {
    pub(crate) fn with_scope<T>(
        self,
        index: usize,
        use_scope: impl FnOnce(PythonMarkerScope<'_>) -> T,
    ) -> Result<T, GraphCheckError> {
        match self {
            Self::Uniform(scope) => Ok(use_scope(scope)),
            Self::PerDeclaration(scopes) => scopes
                .get(index)
                .copied()
                .map(use_scope)
                .ok_or(GraphCheckError::InvalidInput),
            Self::OwnedDeclarations(scopes) => scopes
                .get(index)
                .map(|scope| scope.with_scope(use_scope))
                .ok_or(GraphCheckError::InvalidInput),
        }
    }
    pub(crate) fn check_limits(
        self,
        roots: usize,
        limits: GraphCheckLimits,
    ) -> Result<(), GraphCheckError> {
        self.preflight(roots, &mut Work::new(limits, roots, 0)?)
    }
    fn preflight(self, roots: usize, work: &mut Work) -> Result<(), GraphCheckError> {
        if let Self::OwnedDeclarations(scopes) = self {
            if scopes.len() != roots {
                return Err(GraphCheckError::InvalidInput);
            }
            for scope in scopes {
                for value in scope.values() {
                    work.text(value)?;
                    work.extra()?;
                    NormalizedPythonName::new(value).map_err(|_| GraphCheckError::InvalidInput)?;
                }
            }
            return Ok(());
        }
        let scopes = match &self {
            Self::Uniform(scope) => std::slice::from_ref(scope),
            Self::PerDeclaration(scopes) if scopes.len() == roots => scopes,
            Self::PerDeclaration(_) => return Err(GraphCheckError::InvalidInput),
            Self::OwnedDeclarations(_) => unreachable!(),
        };
        for scope in scopes {
            let (extras, groups): (&[&str], &[&str]) = match scope {
                PythonMarkerScope::Environment => (&[], &[]),
                PythonMarkerScope::Dependency { requested_extras } => (requested_extras, &[]),
                PythonMarkerScope::Lock {
                    selected_extras,
                    dependency_groups,
                } => (selected_extras, dependency_groups),
            };
            for value in extras.iter().chain(groups) {
                work.text(value)?;
                work.extra()?;
                NormalizedPythonName::new(value).map_err(|_| GraphCheckError::InvalidInput)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn derive_graph_scoped(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphCandidateObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    scopes: RootMarkerScopes<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    limits: GraphCheckLimits,
) -> Result<DerivedGraphData, GraphCheckError> {
    let mut work = Work::new(limits, roots.len(), nodes.len())?;
    scopes.preflight(roots.len(), &mut work)?;
    derive_with_work(
        roots,
        nodes,
        environment,
        scopes,
        semantics,
        marker_limits,
        &mut work,
    )
}

pub(crate) fn check_graph_scoped(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphNodeObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    scopes: RootMarkerScopes<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    limits: GraphCheckLimits,
) -> Result<CheckedGraphData, GraphCheckError> {
    let mut work = Work::new(limits, roots.len(), nodes.len())?;
    scopes.preflight(roots.len(), &mut work)?;
    check_with_work(
        roots,
        nodes,
        environment,
        scopes,
        semantics,
        marker_limits,
        &mut work,
    )
}

pub(crate) fn derive_graph(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphCandidateObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    root_scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    limits: GraphCheckLimits,
) -> Result<DerivedGraphData, GraphCheckError> {
    derive_graph_scoped(
        roots,
        nodes,
        environment,
        RootMarkerScopes::Uniform(root_scope),
        semantics,
        marker_limits,
        limits,
    )
}

pub(crate) fn check_graph(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphNodeObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    root_scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    limits: GraphCheckLimits,
) -> Result<CheckedGraphData, GraphCheckError> {
    check_graph_scoped(
        roots,
        nodes,
        environment,
        RootMarkerScopes::Uniform(root_scope),
        semantics,
        marker_limits,
        limits,
    )
}

fn check_with_work(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphNodeObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    scopes: RootMarkerScopes<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    work: &mut Work,
) -> Result<CheckedGraphData, GraphCheckError> {
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(nodes.len())
        .map_err(|_| GraphCheckError::Limit)?;
    for node in nodes {
        let mut claimed = BTreeSet::new();
        for extra in &node.selected.requested_extras {
            work.text(extra)?;
            work.extra()?;
            if !NormalizedPythonName::new(extra).is_ok_and(|name| name.as_str() == extra)
                || !claimed.insert(extra.as_str())
            {
                return Err(GraphCheckError::InvalidInput);
            }
        }
        observations.push(GraphCandidateObservation {
            key: node.selected.key,
            name: &node.selected.name,
            version: &node.selected.version,
            facts: node.facts,
        });
    }
    let derived = derive_with_work(
        roots,
        &observations,
        environment,
        scopes,
        semantics,
        marker_limits,
        work,
    )?;
    for node in nodes {
        let extras = derived
            .extras
            .get(&node.selected.key)
            .ok_or(GraphCheckError::InvalidInput)?;
        if extras.len() != node.selected.requested_extras.len()
            || node
                .selected
                .requested_extras
                .iter()
                .any(|extra| extras.binary_search(extra).is_err())
        {
            return Err(GraphCheckError::NonMinimal);
        }
    }
    Ok(derived.graph)
}

fn derive_with_work(
    roots: &[PythonRequirementSyntax],
    nodes: &[GraphCandidateObservation<'_>],
    environment: PythonMarkerEnvironment<'_>,
    scopes: RootMarkerScopes<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    work: &mut Work,
) -> Result<DerivedGraphData, GraphCheckError> {
    let limits = work.limits;
    let context =
        checked_metadata_context(environment, marker_limits).map_err(|error| match error {
            MetadataInputError::InvalidLimits => GraphCheckError::InvalidLimits,
            MetadataInputError::Limit => GraphCheckError::Limit,
            _ => GraphCheckError::InvalidInput,
        })?;
    let mut by_name = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for (index, node) in nodes.iter().enumerate() {
        work.text(node.name)?;
        work.text(node.version)?;
        if !NormalizedPythonName::new(node.name).is_ok_and(|n| n.as_str() == node.name)
            || !keys.insert(node.key)
            || by_name.insert(node.name, index).is_some()
            || node.facts.name().as_str() != node.name
            || !matches!((Pep440::parse_version(node.version),Pep440::parse_version(node.facts.version())),(Some(a),Some(b)) if a==b)
        {
            return Err(GraphCheckError::InvalidInput);
        }
        if let Some(spec) = node.facts.requires_python() {
            work.text(spec)?;
            if !python_requires_matches_for_context(spec, &context)
                .map_err(|_| GraphCheckError::InvalidInput)?
            {
                return Err(GraphCheckError::RequiresPython);
            }
        }
        for declaration in node.facts.requirements() {
            work.text(declaration.raw_for_semantic_resolution())?;
        }
    }
    let mut reached = vec![false; nodes.len()];
    let mut extras = vec![BTreeSet::<&str>::new(); nodes.len()];
    let mut queued = vec![false; nodes.len()];
    let mut queue = VecDeque::new();
    let mut output = CheckedGraphData {
        roots: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
    };
    for (index, root) in roots.iter().enumerate() {
        work.text(root.raw_for_semantic_resolution())?;
        let (active, warnings) = scopes.with_scope(index, |scope| {
            work.evaluate(root.marker(), environment, scope, semantics, marker_limits)
        })??;
        retain_warnings(&mut output, None, index, warnings);
        if !active {
            continue;
        }
        if root.unresolved_reference().is_some() {
            return Err(GraphCheckError::SourceReference(
                GraphSourceLocation::Root { declaration: index },
            ));
        }
        let target = *by_name
            .get(root.name())
            .ok_or(GraphCheckError::Unsatisfied)?;
        if !version_matches(root.raw_version_specifiers(), nodes[target].version, work)? {
            return Err(GraphCheckError::Unsatisfied);
        }
        propagate(
            target,
            root.requested_extras(),
            &mut reached,
            &mut extras,
            &mut queued,
            &mut queue,
            work,
        )?;
        output.roots.push(CheckedRoot {
            declaration: index,
            to: nodes[target].key,
        });
    }
    // Only actual root seeds introduce extras. This least fixed point rejects
    // self-supporting conditional cycles which a claimed final extra set hides.
    let mut enabled = BTreeSet::new();
    while let Some(index) = queue.pop_front() {
        queued[index] = false;
        let current = extras[index].iter().copied().collect::<Vec<_>>();
        for (declaration, dependency) in nodes[index].facts.requirements().iter().enumerate() {
            let (active, _) = work.evaluate(
                dependency.exact_marker(),
                environment,
                PythonMarkerScope::Dependency {
                    requested_extras: &current,
                },
                semantics,
                marker_limits,
            )?;
            if !active {
                continue;
            }
            if dependency.unresolved_reference().is_some() {
                return Err(GraphCheckError::SourceReference(
                    GraphSourceLocation::Dependency {
                        owner: nodes[index].key,
                        declaration,
                    },
                ));
            }
            let target = *by_name
                .get(dependency.name())
                .ok_or(GraphCheckError::Unsatisfied)?;
            if !version_matches(
                dependency.raw_version_specifiers(),
                nodes[target].version,
                work,
            )? {
                return Err(GraphCheckError::Unsatisfied);
            }
            if !enabled.contains(&(index, declaration)) {
                if enabled.len() >= limits.max_edges {
                    return Err(GraphCheckError::Limit);
                }
                enabled.insert((index, declaration));
            }
            propagate(
                target,
                dependency.requested_extras(),
                &mut reached,
                &mut extras,
                &mut queued,
                &mut queue,
                work,
            )?;
        }
    }
    let mut derived_extras = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if !reached[index] {
            return Err(GraphCheckError::NonMinimal);
        }
        let current = extras[index].iter().copied().collect::<Vec<_>>();
        // Retain diagnostics for the final real context, not intermediate
        // solver guesses or the union of all potential extra subsets.
        for (declaration, dependency) in node.facts.requirements().iter().enumerate() {
            let (active, warnings) = work.evaluate(
                dependency.exact_marker(),
                environment,
                PythonMarkerScope::Dependency {
                    requested_extras: &current,
                },
                semantics,
                marker_limits,
            )?;
            retain_warnings(&mut output, Some(node.key), declaration, warnings);
            if active {
                let target = *by_name
                    .get(dependency.name())
                    .ok_or(GraphCheckError::Unsatisfied)?;
                output.edges.push(CheckedEdge {
                    from: node.key,
                    declaration,
                    to: nodes[target].key,
                });
            }
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(extras[index].len())
            .map_err(|_| GraphCheckError::Limit)?;
        for extra in &extras[index] {
            work.text(extra)?;
            work.extra()?;
            owned.push((*extra).to_owned());
        }
        derived_extras.insert(node.key, owned);
    }
    Ok(DerivedGraphData {
        graph: output,
        extras: derived_extras,
    })
}

fn version_matches(
    raw: Option<&str>,
    candidate: &str,
    work: &Work,
) -> Result<bool, GraphCheckError> {
    let Some(raw) = raw else {
        return Ok(true);
    };
    let limits = RawVersionConstraintLimits {
        max_specifier_bytes: work.limits.max_total_utf8_bytes,
        max_clauses: work.limits.max_evaluation_nodes,
        max_literal_bytes: work.limits.max_total_utf8_bytes,
        max_candidate_bytes: work.limits.max_total_utf8_bytes,
    };
    let map_error = |error| match error {
        RawVersionConstraintError::InvalidLimits => GraphCheckError::InvalidLimits,
        RawVersionConstraintError::Limit(_) => GraphCheckError::Limit,
        RawVersionConstraintError::Syntax => GraphCheckError::InvalidInput,
    };
    RawVersionConstraint::parse(raw, limits)
        .map_err(map_error)?
        .matches(candidate)
        .map_err(map_error)
}

fn retain_warnings(
    output: &mut CheckedGraphData,
    owner: Option<CandidateKey>,
    declaration: usize,
    warnings: Vec<PythonMarkerWarning>,
) {
    if !warnings.is_empty() {
        output.warnings.push(GraphMarkerWarnings {
            owner,
            declaration,
            warnings,
        });
    }
}
struct Work {
    bytes: usize,
    nodes: usize,
    extra_references: usize,
    limits: GraphCheckLimits,
}
impl Work {
    fn new(limits: GraphCheckLimits, roots: usize, nodes: usize) -> Result<Self, GraphCheckError> {
        limits.validate()?;
        if roots > limits.max_roots || nodes > limits.max_nodes {
            return Err(GraphCheckError::Limit);
        }
        Ok(Self {
            bytes: 0,
            nodes: 0,
            extra_references: 0,
            limits,
        })
    }
    fn text(&mut self, text: &str) -> Result<(), GraphCheckError> {
        charge(
            &mut self.bytes,
            text.len(),
            self.limits.max_total_utf8_bytes,
        )
    }
    fn extra(&mut self) -> Result<(), GraphCheckError> {
        charge(
            &mut self.extra_references,
            1,
            self.limits.max_extra_references,
        )
    }
    fn evaluate(
        &mut self,
        expression: Option<&PythonMarkerExpression>,
        environment: PythonMarkerEnvironment<'_>,
        scope: PythonMarkerScope<'_>,
        semantics: MarkerSemantics,
        limits: PythonMarkerLimits,
    ) -> Result<(bool, Vec<PythonMarkerWarning>), GraphCheckError> {
        charge(
            &mut self.nodes,
            expression.map_or(1, PythonMarkerExpression::node_count),
            self.limits.max_evaluation_nodes,
        )?;
        let Some(expression) = expression else {
            return Ok((true, vec![]));
        };
        let evaluated = evaluate_python_marker(expression, environment, scope, semantics, limits)
            .map_err(|_| GraphCheckError::Marker)?;
        Ok((evaluated.value(), evaluated.warnings().to_vec()))
    }
}
fn charge(used: &mut usize, count: usize, maximum: usize) -> Result<(), GraphCheckError> {
    *used = used
        .checked_add(count)
        .filter(|n| *n <= maximum)
        .ok_or(GraphCheckError::Limit)?;
    Ok(())
}
fn propagate<'a>(
    index: usize,
    requested: impl Iterator<Item = &'a str>,
    reached: &mut [bool],
    extras: &mut [BTreeSet<&'a str>],
    queued: &mut [bool],
    queue: &mut VecDeque<usize>,
    work: &mut Work,
) -> Result<(), GraphCheckError> {
    let mut changed = !reached[index];
    reached[index] = true;
    for extra in requested {
        if !extras[index].contains(extra) {
            work.extra()?;
            extras[index].insert(extra);
            changed = true;
        }
    }
    if changed && !queued[index] {
        queued[index] = true;
        queue.push_back(index);
    }
    Ok(())
}

#[cfg(test)]
#[path = "graph_check/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "graph_check/scoped_tests.rs"]
mod scoped_tests;
