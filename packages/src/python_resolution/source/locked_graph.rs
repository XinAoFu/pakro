//! Strict replay of one actual old registry graph, never a new solve.

#[path = "locked_graph/begin.rs"]
mod begin;
#[path = "locked_graph/prepare.rs"]
mod prepare;
#[path = "locked_graph/target.rs"]
mod target;
pub(crate) use target::{FrozenRegistryTarget, FrozenTargetLimits};

use super::replay::metadata::{LockedMetadataError, LockedWheelCandidate};
use super::replay::{LockedRegistryArtifact, LockedReplayError};
use super::*;
use crate::packages::python_resolution::graph_check::{
    DerivedGraphData, GraphCheckError, GraphCheckLimits, GraphSourceLocation,
};
use crate::packages::python_resolution::lock_model::{
    ContentPin, GroupKey, LockModelLimits, LockedGraph, LockedNode, PinId, RuntimePin,
};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::root_scopes::OwnedRootMarkerScope;
use crate::packages::python_wheel::WheelLimits;
use pakro_index::python_metadata::PythonMetadataLimits;
use pakro_index::python_requirements::{
    PythonRequirementLimits, PythonRequirementSyntax, UnresolvedDependencyReference,
};
use pakro_index::python_source::PythonSourcePolicy;
use pakro_resolver::python_native::CandidateKey;
use std::collections::BTreeMap;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum LockedGraphError {
    #[error("strict locked graph limits must be positive")]
    InvalidLimits,
    #[error("strict locked graph budget exceeded")]
    Limit,
    #[error("the explicit locked graph target is invalid")]
    InvalidTarget,
    #[error("the actual locked registry graph is absent")]
    MissingGroup,
    #[error("the locked graph does not record the complete current original roots")]
    RootMismatch,
    #[error("an original locked root declaration is invalid")]
    InvalidRoot,
    #[error("the frozen target differs from the actual locked graph")]
    TargetMismatch,
    #[error("the configured locked registries are inconsistent")]
    InvalidConfiguration,
    #[error("a locked registry identity differs from its current configuration")]
    RegistryMismatch,
    #[error("the actual project lock is no longer current")]
    Revoked,
    #[error("the strict locked graph deadline expired")]
    Deadline,
    #[error("the locked graph differs from the actual original dependency closure")]
    GraphMismatch,
    #[error("strict locked replay is missing exact artifact inputs while offline")]
    MissingInputs(Vec<PinId>),
    #[error("locked artifact replay failed: {0}")]
    Replay(LockedReplayError),
    #[error("locked wheel metadata failed: {0}")]
    Metadata(LockedMetadataError),
    #[error("locked dependency closure failed: {0}")]
    Graph(GraphCheckError),
}

pub(crate) struct LockedRegistryEndpoint<'config> {
    pub project: PythonSourceSession<'config>,
    pub file: SourceFileSession<'config>,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct LockedGraphLimits {
    pub max_nodes: usize,
    pub max_registries: usize,
    pub max_retained_archive_bytes: u64,
    pub max_retained_utf8_bytes: usize,
    pub requirements: PythonRequirementLimits,
    pub target: FrozenTargetLimits,
    pub lock: LockModelLimits,
    pub project: SimpleClientLimits,
    pub source: SourceObservationLimits,
    pub wheel: WheelLimits,
    pub metadata: PythonMetadataLimits,
    pub graph: GraphCheckLimits,
}
impl Default for LockedGraphLimits {
    fn default() -> Self {
        Self {
            max_nodes: 65_536,
            max_registries: 4096,
            max_retained_archive_bytes: 4 * 1024 * 1024 * 1024,
            max_retained_utf8_bytes: 64 * 1024 * 1024,
            requirements: Default::default(),
            target: Default::default(),
            lock: Default::default(),
            project: Default::default(),
            source: Default::default(),
            wheel: Default::default(),
            metadata: Default::default(),
            graph: Default::default(),
        }
    }
}
pub(crate) struct LockedGraphContext<'config> {
    pub client: &'config SimpleIndexClient,
    pub endpoints: &'config [LockedRegistryEndpoint<'config>],
    pub source_policy: &'config PythonSourcePolicy,
    pub fetch: SimpleFetchPolicy,
    pub limits: LockedGraphLimits,
    pub cache_root: &'config Path,
    pub scratch_root: &'config Path,
    pub deadline: Instant,
}
pub(crate) struct LockedRegistryReplay<'lock, 'config> {
    root: &'lock BoundPythonLock,
    graph: &'lock LockedGraph,
    roots: Vec<PythonRequirementSyntax>,
    root_scopes: Option<Vec<OwnedRootMarkerScope>>,
    target: FrozenRegistryTarget,
    context: LockedGraphContext<'config>,
    owners: BTreeMap<CandidateKey, LockedWheelCandidate<'lock>>,
    bindings: BTreeMap<String, CandidateKey>,
    nodes: Vec<LockedGraphNode<'lock>>,
    retained_utf8: usize,
    root_work: usize,
    builds: BTreeMap<CandidateKey, LockedRegistryArtifact<'lock>>,
}
struct LockedGraphNode<'lock> {
    key: CandidateKey,
    node: &'lock LockedNode,
    pin: &'lock ContentPin,
    endpoint: usize,
}
impl<'lock, 'config> LockedRegistryReplay<'lock, 'config> {
    pub(crate) fn new_scoped(
        root: &'lock BoundPythonLock,
        key: &GroupKey,
        raw_roots: &[&str],
        root_scopes: &[OwnedRootMarkerScope],
        target: &FrozenRegistryTarget,
        context: LockedGraphContext<'config>,
    ) -> Result<Self, LockedGraphError> {
        begin::new_scoped(root, key, raw_roots, root_scopes, target, context)
    }
    pub(crate) fn new(
        root: &'lock BoundPythonLock,
        key: &GroupKey,
        raw_roots: &[&str],
        target: &FrozenRegistryTarget,
        context: LockedGraphContext<'config>,
    ) -> Result<Self, LockedGraphError> {
        begin::new(root, key, raw_roots, target, context)
    }
    pub(crate) async fn prepare(
        self,
    ) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
        self.prepare_with_finish(|| {}).await
    }
    async fn prepare_with_finish(
        self,
        finish: impl FnOnce(),
    ) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
        prepare::run(self, finish).await
    }
}
fn with_root_scopes<T>(
    scopes: Option<&[OwnedRootMarkerScope]>,
    target: &FrozenRegistryTarget,
    use_scopes: impl FnOnce(crate::packages::python_resolution::graph_check::RootMarkerScopes<'_>) -> T,
) -> T {
    use crate::packages::python_resolution::graph_check::RootMarkerScopes;
    match scopes {
        Some(scopes) => use_scopes(RootMarkerScopes::OwnedDeclarations(scopes)),
        None => target.with_root_scope(|scope| use_scopes(RootMarkerScopes::Uniform(scope))),
    }
}
impl fmt::Debug for LockedRegistryReplay<'_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("LockedRegistryReplay")
            .field("nodes", &self.graph.nodes().len())
            .finish_non_exhaustive()
    }
}
pub(crate) struct LockedPreparedPlan<'lock> {
    root: &'lock BoundPythonLock,
    graph: &'lock LockedGraph,
    roots: Vec<PythonRequirementSyntax>,
    root_scopes: Option<Vec<OwnedRootMarkerScope>>,
    target: FrozenRegistryTarget,
    owners: BTreeMap<CandidateKey, LockedWheelCandidate<'lock>>,
    bindings: BTreeMap<String, CandidateKey>,
    derived: DerivedGraphData,
    deadline: Instant,
}
impl LockedPreparedPlan<'_> {
    pub(crate) fn root(&self) -> &BoundPythonLock {
        self.root
    }
    pub(crate) fn graph(&self) -> &LockedGraph {
        self.graph
    }
    pub(crate) fn roots(&self) -> &[PythonRequirementSyntax] {
        &self.roots
    }
    pub(crate) fn root_scopes(&self) -> Option<&[OwnedRootMarkerScope]> {
        self.root_scopes.as_deref()
    }
    pub(crate) fn target(&self) -> &FrozenRegistryTarget {
        &self.target
    }
    pub(crate) fn runtime_pin(&self) -> RuntimePin {
        self.graph.runtime()
    }
    pub(crate) fn owner(&self, name: &str) -> Option<&LockedWheelCandidate<'_>> {
        self.bindings.get(name).and_then(|key| self.owners.get(key))
    }
    pub(crate) fn node_key(&self, name: &str) -> Option<CandidateKey> {
        self.bindings.get(name).copied()
    }
    pub(crate) fn derived(&self) -> &DerivedGraphData {
        &self.derived
    }
    pub(crate) fn recheck_for(&self, root: &BoundPythonLock) -> Result<(), LockedGraphError> {
        if !std::ptr::eq(self.root, root) {
            return Err(LockedGraphError::Revoked);
        }
        self.root.recheck().map_err(|_| LockedGraphError::Revoked)?;
        for owner in self.owners.values() {
            owner
                .recheck_for(root)
                .map_err(|_| LockedGraphError::Revoked)?;
        }
        Ok(())
    }
}
impl fmt::Debug for LockedPreparedPlan<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("LockedPreparedPlan")
            .field("nodes", &self.owners.len())
            .field("original_roots", &self.roots.len())
            .finish_non_exhaustive()
    }
}
pub(crate) struct LockedGraphBuildContinuation<'lock, 'config> {
    replay: LockedRegistryReplay<'lock, 'config>,
    source: LockedRegistryArtifact<'lock>,
}
impl LockedGraphBuildContinuation<'_, '_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        self.source.pin()
    }
}
pub(crate) struct LockedGraphSourceContinuation<'lock, 'config> {
    replay: LockedRegistryReplay<'lock, 'config>,
    location: GraphSourceLocation,
}
impl LockedGraphSourceContinuation<'_, '_> {
    pub(crate) fn location(&self) -> GraphSourceLocation {
        self.location
    }
    pub(crate) fn reference_for_source_binding(&self) -> Option<&UnresolvedDependencyReference> {
        match self.location {
            GraphSourceLocation::Root { declaration } => {
                self.replay.roots.get(declaration)?.unresolved_reference()
            }
            GraphSourceLocation::Dependency { owner, declaration } => self
                .replay
                .owners
                .get(&owner)?
                .facts()
                .requirements()
                .get(declaration)?
                .unresolved_reference(),
        }
    }
}
pub(crate) enum LockedGraphPreparation<'lock, 'config> {
    Ready(LockedPreparedPlan<'lock>),
    NeedsBuild(LockedGraphBuildContinuation<'lock, 'config>),
    NeedsSourceAdapter(LockedGraphSourceContinuation<'lock, 'config>),
}
impl fmt::Debug for LockedGraphPreparation<'_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Ready(_) => "LockedGraphPreparation::Ready",
            Self::NeedsBuild(_) => "LockedGraphPreparation::NeedsBuild",
            Self::NeedsSourceAdapter(_) => "LockedGraphPreparation::NeedsSourceAdapter",
        })
    }
}

#[cfg(test)]
#[path = "locked_graph/tests.rs"]
mod tests;
