//! The solver: pubgrub over an injected metadata source. Top-level requests
//! must be exact pins (ADR-0004); transitive constraints are ranges solved
//! here. A locked graph's choices are preferred while they still satisfy the
//! constraints, so re-installing does not churn unrelated versions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;

use pep440_rs::Version;
use pubgrub::{DefaultStringReporter, PubGrubError, Ranges, Reporter};
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics, resolve,
};

use crate::error::ResolveFailure;
use crate::graph::{DependencyRequest, ResolvedGraph, ResolvedNode};
use crate::pyver::PyRanges;

/// Metadata the solver needs, injected so this crate stays offline-testable.
/// Constraint strings use each ecosystem's own syntax; `dependencies` already
/// had environment markers filtered by the provider.
pub trait PyMetadataSource {
    /// All candidate versions of a package.
    fn versions(&self, name: &str) -> Result<Vec<String>, String>;
    /// Dependencies of one exact version as (name, constraint) pairs.
    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String>;
}

const ROOT: &str = "\u{0}mise-root";

type DepEntry = (String, PyRanges, String);
type DepsCache = HashMap<(String, Version), Arc<Vec<DepEntry>>>;

pub fn solve(
    requests: &[DependencyRequest],
    locked: &ResolvedGraph,
    source: &dyn PyMetadataSource,
) -> Result<ResolvedGraph, ResolveFailure> {
    let locked: HashMap<String, Version> = locked
        .nodes
        .iter()
        .filter_map(|node| {
            Version::from_str(&node.version)
                .ok()
                .map(|v| (node.name.clone(), v))
        })
        .collect();
    let provider = PyProvider {
        source,
        locked,
        versions_cache: RefCell::new(HashMap::new()),
        deps_cache: RefCell::new(HashMap::new()),
    };
    // the synthetic root carries the user's exact pins as its dependency set
    let mut root_deps: Vec<DepEntry> = vec![];
    for req in requests {
        let exact = Version::from_str(&req.constraint).map_err(|_| ResolveFailure::NotPinned {
            name: req.name.clone(),
            constraint: req.constraint.clone(),
        })?;
        root_deps.push((
            req.name.clone(),
            Ranges::singleton(exact),
            req.constraint.clone(),
        ));
    }
    provider
        .deps_cache
        .borrow_mut()
        .insert((ROOT.to_string(), Version::new([0])), Arc::new(root_deps));
    let selected = resolve(&provider, ROOT.to_string(), Version::new([0])).map_err(map_error)?;
    let mut nodes = vec![];
    for (name, version) in selected.iter() {
        if name == ROOT {
            continue;
        }
        let deps = provider
            .deps_cache
            .borrow()
            .get(&(name.clone(), version.clone()))
            .map(|entries| {
                entries
                    .iter()
                    .map(|(dep_name, _range, constraint)| DependencyRequest {
                        eco: "python".into(),
                        name: dep_name.clone(),
                        constraint: constraint.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        nodes.push(ResolvedNode {
            eco: "python".into(),
            name: name.clone(),
            version: version.to_string(),
            // filled later by artifact selection, never by the solver
            digest: None,
            deps,
        });
    }
    Ok(ResolvedGraph { nodes }.sorted())
}

fn map_error(err: PubGrubError<PyProvider<'_>>) -> ResolveFailure {
    match err {
        PubGrubError::NoSolution(tree) => ResolveFailure::NoSolution {
            report: DefaultStringReporter::report(&tree),
        },
        // the provider's Err type is Infallible, so no other variant can occur
        other => ResolveFailure::NoSolution {
            report: other.to_string(),
        },
    }
}

struct PyProvider<'a> {
    source: &'a dyn PyMetadataSource,
    locked: HashMap<String, Version>,
    versions_cache: RefCell<HashMap<String, Arc<Vec<Version>>>>,
    deps_cache: RefCell<DepsCache>,
}

impl PyProvider<'_> {
    fn versions_of(&self, name: &str) -> Arc<Vec<Version>> {
        if let Some(cached) = self.versions_cache.borrow().get(name) {
            return cached.clone();
        }
        let parsed = self
            .source
            .versions(name)
            .map(|versions| {
                versions
                    .iter()
                    .filter_map(|s| Version::from_str(s).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let arc = Arc::new(parsed);
        self.versions_cache
            .borrow_mut()
            .insert(name.to_string(), arc.clone());
        arc
    }

    fn deps_of(&self, name: &str, version: &Version) -> Result<Arc<Vec<DepEntry>>, String> {
        if let Some(cached) = self
            .deps_cache
            .borrow()
            .get(&(name.to_string(), version.clone()))
        {
            return Ok(cached.clone());
        }
        let entries = self
            .source
            .dependencies(name, &version.to_string())
            .map(|deps| {
                Arc::new(
                    deps.into_iter()
                        .map(|dep| {
                            // an unparseable constraint makes that edge unsatisfiable
                            // instead of failing the whole solve with a confusing error
                            let range = constraint_or_empty(&dep.constraint);
                            (dep.name, range, dep.constraint)
                        })
                        .collect::<Vec<_>>(),
                )
            })?;
        self.deps_cache
            .borrow_mut()
            .insert((name.to_string(), version.clone()), entries.clone());
        Ok(entries)
    }
}

fn constraint_or_empty(constraint: &str) -> PyRanges {
    crate::pyver::constraint_to_ranges(constraint).unwrap_or_else(|_| Ranges::empty())
}

impl DependencyProvider for PyProvider<'_> {
    type P = String;
    type V = Version;
    type VS = Ranges<Version>;
    type M = String;
    type Err = Infallible;
    type Priority = ();

    fn prioritize(
        &self,
        _package: &Self::P,
        _range: &Self::VS,
        _stats: &PackageResolutionStatistics,
    ) -> Self::Priority {
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        // the synthetic root exists only as version 0 carrying the user's pins
        if package == ROOT {
            let root = Version::new([0]);
            return Ok(range.contains(&root).then_some(root));
        }
        if let Some(locked) = self.locked.get(package)
            && range.contains(locked)
        {
            return Ok(Some(locked.clone()));
        }
        let versions = self.versions_of(package);
        Ok(versions.iter().filter(|v| range.contains(v)).max().cloned())
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        Ok(match self.deps_of(package, version) {
            Ok(deps) => Dependencies::Available(
                deps.iter()
                    .map(|(name, range, _)| (name.clone(), range.clone()))
                    .collect::<DependencyConstraints<Self::P, Self::VS>>(),
            ),
            Err(reason) => Dependencies::Unavailable(reason),
        })
    }
}
