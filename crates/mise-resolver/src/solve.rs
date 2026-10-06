//! The solver: pubgrub over an injected metadata source. Top-level requests
//! must be exact pins (ADR-0004); transitive constraints are ranges solved
//! here. A locked graph's choices are preferred while they still satisfy the
//! constraints, so re-installing does not churn unrelated versions.
//!
//! Ecosystem version semantics plug in through [`VersionAdapter`]: pep440 for
//! python, node-semver for node. Everything else (synthetic root carrying the
//! pins, memoized provider, graph assembly) is shared and generic.

use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;

use pubgrub::{DefaultStringReporter, PubGrubError, Ranges, Reporter};
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics, resolve,
};

use crate::error::ResolveFailure;
use crate::graph::{DependencyRequest, ResolvedGraph, ResolvedNode};

/// Metadata the solver needs, injected so this crate stays offline-testable.
/// Constraint strings use each ecosystem's own syntax; `dependencies` already
/// had environment markers filtered by the provider.
pub trait MetadataSource {
    /// All candidate versions of a package.
    fn versions(&self, name: &str) -> Result<Vec<String>, String>;
    /// Dependencies of one exact version as (name, constraint) pairs.
    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String>;
}

const ROOT: &str = "\u{0}mise-root";

/// How an ecosystem's versions and constraint strings map onto pubgrub
/// ranges.
pub trait VersionAdapter {
    type V: Clone + Ord + std::fmt::Debug + std::fmt::Display + std::cmp::Eq;
    /// the synthetic root's version; each ecosystem's grammar has its own
    /// minimal version
    const ROOT_VERSION: &'static str;
    fn parse_version(text: &str) -> Option<Self::V>;
    /// An exact top-level pin; `Err` carries the human reason.
    fn pin_range(text: &str) -> Result<Ranges<Self::V>, String>;
    /// A package-author constraint; unparseable becomes empty (unsatisfiable)
    /// rather than failing the whole solve.
    fn constraint_range(text: &str) -> Ranges<Self::V>;
}

/// pep440 (python) semantics via pep440_rs.
pub struct Pep440;
impl VersionAdapter for Pep440 {
    type V = pep440_rs::Version;
    const ROOT_VERSION: &'static str = "0";
    fn parse_version(text: &str) -> Option<Self::V> {
        pep440_rs::Version::from_str(text).ok()
    }
    fn pin_range(text: &str) -> Result<Ranges<Self::V>, String> {
        Self::parse_version(text)
            .map(Ranges::singleton)
            .ok_or_else(|| "must be an exact version".to_string())
    }
    fn constraint_range(text: &str) -> Ranges<Self::V> {
        crate::pyver::constraint_to_ranges(text).unwrap_or_else(|_| Ranges::empty())
    }
}

/// npm semver semantics via node-semver plus our range grammar adapter.
pub struct NpmSemver;
impl VersionAdapter for NpmSemver {
    type V = node_semver::Version;
    const ROOT_VERSION: &'static str = "0.0.0";
    fn parse_version(text: &str) -> Option<Self::V> {
        node_semver::Version::from_str(text).ok()
    }
    fn pin_range(text: &str) -> Result<Ranges<Self::V>, String> {
        Self::parse_version(text)
            .map(Ranges::singleton)
            .ok_or_else(|| "must be an exact version".to_string())
    }
    fn constraint_range(text: &str) -> Ranges<Self::V> {
        crate::npmver::range_to_ranges(text).unwrap_or_else(|_| Ranges::empty())
    }
}

pub fn solve(
    requests: &[DependencyRequest],
    locked: &ResolvedGraph,
    source: &dyn MetadataSource,
    eco: &str,
) -> Result<ResolvedGraph, ResolveFailure> {
    match eco {
        "python" => run_solve::<Pep440>(requests, locked, source, eco),
        "node" => run_solve::<NpmSemver>(requests, locked, source, eco),
        other => Err(ResolveFailure::NoSolution {
            report: format!("unsupported ecosystem {other:?}"),
        }),
    }
}

fn run_solve<A: VersionAdapter>(
    requests: &[DependencyRequest],
    locked: &ResolvedGraph,
    source: &dyn MetadataSource,
    eco: &str,
) -> Result<ResolvedGraph, ResolveFailure> {
    let locked: HashMap<String, String> = locked
        .nodes
        .iter()
        .map(|node| (node.name.clone(), node.version.clone()))
        .collect();
    let provider = Provider::<A> {
        source,
        locked,
        versions_cache: RefCell::new(HashMap::new()),
        deps_cache: RefCell::new(HashMap::new()),
        first_error: RefCell::new(None),
    };
    // the synthetic root carries the user's exact pins as its dependency set
    let mut root_deps: Vec<(String, Ranges<A::V>, String)> = vec![];
    for req in requests {
        let range = A::pin_range(&req.constraint).map_err(|reason| ResolveFailure::NotPinned {
            name: req.name.clone(),
            constraint: format!("{reason} ({})", req.constraint),
        })?;
        root_deps.push((req.name.clone(), range, req.constraint.clone()));
    }
    let root_v = A::parse_version(A::ROOT_VERSION).ok_or_else(|| ResolveFailure::NoSolution {
        report: "adapter rejected the synthetic root version".to_string(),
    })?;
    provider
        .deps_cache
        .borrow_mut()
        .insert((ROOT.to_string(), root_v.to_string()), Arc::new(root_deps));
    let selected = match resolve(&provider, ROOT.to_string(), root_v) {
        Ok(selected) => selected,
        Err(err) => {
            // surface the provider's first real failure (network, registry)
            // instead of the bare "no version" pubgrub report
            let pubgrub_report = match map_error::<Provider<A>>(err) {
                ResolveFailure::NoSolution { report } => report,
                other => other.to_string(),
            };
            let report = match provider.first_error.borrow().as_ref() {
                Some(source_error) => format!("{source_error}\n(pubgrub: {pubgrub_report})"),
                None => pubgrub_report,
            };
            return Err(ResolveFailure::NoSolution { report });
        }
    };
    let mut nodes = vec![];
    for (name, version) in selected.iter() {
        if name == ROOT {
            continue;
        }
        let deps = provider
            .deps_cache
            .borrow()
            .get(&(name.clone(), version.to_string()))
            .map(|entries| {
                entries
                    .iter()
                    .map(|(dep_name, _range, constraint)| DependencyRequest {
                        eco: eco.to_string(),
                        name: dep_name.clone(),
                        constraint: constraint.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        nodes.push(ResolvedNode {
            eco: eco.to_string(),
            name: name.clone(),
            version: version.to_string(),
            // filled later by artifact selection, never by the solver
            digest: None,
            deps,
        });
    }
    Ok(ResolvedGraph { nodes }.sorted())
}

fn map_error<DP: DependencyProvider>(err: PubGrubError<DP>) -> ResolveFailure {
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

type DepEntry<V> = (String, Ranges<V>, String);
type DepsCache<V> = HashMap<(String, String), Arc<Vec<DepEntry<V>>>>;

struct Provider<'a, A: VersionAdapter> {
    source: &'a dyn MetadataSource,
    locked: HashMap<String, String>,
    versions_cache: RefCell<HashMap<String, Arc<Vec<String>>>>,
    deps_cache: RefCell<DepsCache<A::V>>,
    first_error: RefCell<Option<String>>,
}

impl<A: VersionAdapter> Provider<'_, A> {
    fn versions_of(&self, name: &str) -> Arc<Vec<String>> {
        if let Some(cached) = self.versions_cache.borrow().get(name) {
            return cached.clone();
        }
        let raw = match self.source.versions(name) {
            Ok(raw) => raw,
            Err(err) => {
                if self.first_error.borrow().is_none() {
                    *self.first_error.borrow_mut() =
                        Some(format!("listing versions of {name} failed: {err}"));
                }
                vec![]
            }
        };
        // versions the adapter cannot parse are invisible to the solver
        let parsed = raw
            .into_iter()
            .filter(|v| A::parse_version(v).is_some())
            .collect::<Vec<_>>();
        let arc = Arc::new(parsed);
        self.versions_cache
            .borrow_mut()
            .insert(name.to_string(), arc.clone());
        arc
    }

    fn deps_of(&self, name: &str, version: &str) -> Result<Arc<Vec<DepEntry<A::V>>>, String> {
        if let Some(cached) = self
            .deps_cache
            .borrow()
            .get(&(name.to_string(), version.to_string()))
        {
            return Ok(cached.clone());
        }
        let entries = self.source.dependencies(name, version).map(|deps| {
            Arc::new(
                deps.into_iter()
                    .map(|dep| {
                        let range = A::constraint_range(&dep.constraint);
                        (dep.name, range, dep.constraint)
                    })
                    .collect::<Vec<_>>(),
            )
        })?;
        self.deps_cache
            .borrow_mut()
            .insert((name.to_string(), version.to_string()), entries.clone());
        Ok(entries)
    }
}

impl<A: VersionAdapter> DependencyProvider for Provider<'_, A> {
    type P = String;
    type V = A::V;
    type VS = Ranges<A::V>;
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
            if let Some(root) = A::parse_version(A::ROOT_VERSION)
                && range.contains(&root)
            {
                return Ok(Some(root));
            }
            return Ok(None);
        }
        // a locked choice still inside the range wins, so unrelated pins
        // never churn
        if let Some(locked) = self.locked.get(package)
            && let Some(locked_v) = A::parse_version(locked)
            && range.contains(&locked_v)
        {
            return Ok(Some(locked_v));
        }
        let versions = self.versions_of(package);
        Ok(versions
            .iter()
            .filter_map(|v| A::parse_version(v))
            .filter(|v| range.contains(v))
            .max())
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        Ok(match self.deps_of(package, &version.to_string()) {
            Ok(deps) => Dependencies::Available(
                deps.iter()
                    .map(|(name, range, _)| (name.clone(), range.clone()))
                    .collect::<DependencyConstraints<Self::P, Self::VS>>(),
            ),
            Err(reason) => Dependencies::Unavailable(reason),
        })
    }
}
