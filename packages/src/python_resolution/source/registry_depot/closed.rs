//! Semantic/source closure for this explicit request and observed target.
//!
//! Configuration freshness, a real runtime binding and environment publication
//! are separate capabilities. This owner cannot publish or extract archives.

use super::*;
use crate::packages::python_resolution::graph_check::{
    CheckedGraphData, GraphCheckError, GraphCheckLimits, GraphNodeObservation, check_graph_scoped,
};
use crate::packages::python_resolution::lock_model::RegistryArtifactKind;
use pakro_resolver::solve::{Pep440, VersionAdapter};
use std::collections::BTreeSet;

pub(crate) struct ClosedRegistryGraph<'root> {
    prepared: RegistryPreparedPlan<'root>,
    checked: CheckedGraphData,
}

/// A mixed graph keeps Conda owners and registry owners distinct; it never
/// constructs a registry pin for an installed Conda distribution.
pub(crate) struct ClosedMixedRegistryGraph<'root> {
    prepared: MixedRegistryPreparedPlan<'root>,
    checked: CheckedGraphData,
}
impl ClosedMixedRegistryGraph<'_> {
    pub(crate) fn plan(&self) -> &NativePlan { &self.prepared.prepared.plan }
    pub(crate) fn checked(&self) -> &CheckedGraphData { &self.checked }
    pub(crate) fn registry_owner(&self, key: CandidateKey) -> Option<&RegistryMetadataOwner<'_>> {
        self.prepared.prepared.owners.get(&key)
    }
    pub(crate) fn conda_owner(&self, key: CandidateKey) -> Option<&conda_metadata::CondaDistribution<'_>> {
        self.prepared.conda.get(&key)
    }
    pub(crate) fn recheck(&self) -> Result<(), RegistryClosureError> {
        current_deadline(&self.prepared.prepared)?;
        self.prepared.prepared.root.recheck().map_err(|_| RegistryClosureError::Revoked)?;
        for owner in self.prepared.prepared.owners.values() {
            owner.recheck_for(self.prepared.prepared.root).map_err(|_| RegistryClosureError::Revoked)?;
        }
        for owner in self.prepared.conda.values() {
            owner.recheck().map_err(|_| RegistryClosureError::Revoked)?;
        }
        Ok(())
    }
}

pub(crate) fn close_mixed_registry_plan<'root>(
    prepared: MixedRegistryPreparedPlan<'root>,
    limits: GraphCheckLimits,
) -> Result<ClosedMixedRegistryGraph<'root>, RegistryClosureError> {
    let base = &prepared.prepared;
    current_deadline(base)?;
    limits.validate().map_err(RegistryClosureError::Graph)?;
    base.root.recheck().map_err(|_| RegistryClosureError::Revoked)?;
    if base.owners.len() + prepared.conda.len() != base.plan.selected.len() {
        return Err(RegistryClosureError::IdentityMismatch);
    }
    let mut keys = BTreeSet::new();
    let mut observations = Vec::new();
    for selected in &base.plan.selected {
        current_deadline(base)?;
        if !keys.insert(selected.key) { return Err(RegistryClosureError::IdentityMismatch); }
        let facts = if let Some(owner) = prepared.conda.get(&selected.key) {
            owner.recheck().map_err(|_| RegistryClosureError::Revoked)?;
            owner.facts()
        } else {
            let owner = base.owner(selected.key).ok_or(RegistryClosureError::IdentityMismatch)?;
            owner.recheck_for(base.root).map_err(|_| RegistryClosureError::Revoked)?;
            let file = owner.pin().file();
            let kind_matches = match file.kind() {
                RegistryArtifactKind::Wheel => owner.build_record().is_none(),
                RegistryArtifactKind::Sdist => owner.build_record().is_some(),
            };
            if !kind_matches || file.project() != selected.name || !same_version(file.version(), &selected.version) {
                return Err(RegistryClosureError::IdentityMismatch);
            }
            if let Some(manifest) = owner.manifest() {
                if manifest.name() != selected.name || !same_version(manifest.version(), &selected.version)
                    || !manifest.tags().iter().any(|tag| base.supported().ordered().contains(tag)) {
                    return Err(RegistryClosureError::TargetMismatch);
                }
            } else {
                let wheel = pakro_index::parse_wheel_filename(owner.output_filename())
                    .ok_or(RegistryClosureError::IdentityMismatch)?;
                if base.supported().best_rank(&wheel.tags).map_err(|_| RegistryClosureError::TargetMismatch)?.is_none() {
                    return Err(RegistryClosureError::TargetMismatch);
                }
            }
            owner.facts()
        };
        if facts.name().as_str() != selected.name || !same_version(facts.version(), &selected.version) {
            return Err(RegistryClosureError::IdentityMismatch);
        }
        observations.push(GraphNodeObservation { selected, facts });
    }
    let checked = base.with_root_scopes(|scopes| check_graph_scoped(
        base.roots(), &observations, base.environment(), scopes,
        base.semantics(), base.marker_limits(), limits,
    )).map_err(RegistryClosureError::Graph)?;
    let closed = ClosedMixedRegistryGraph { prepared, checked };
    closed.recheck()?;
    Ok(closed)
}
impl ClosedRegistryGraph<'_> {
    pub(crate) fn plan(&self) -> &NativePlan {
        self.prepared.plan()
    }
    pub(crate) fn checked(&self) -> &CheckedGraphData {
        &self.checked
    }
    pub(crate) fn owner(&self, key: CandidateKey) -> Option<&RegistryMetadataOwner<'_>> {
        self.prepared.owner(key)
    }
    /// URL from the held actual project observation, or the still-live prior lock.
    pub(crate) fn registry_url(&self, key: CandidateKey) -> Option<&str> {
        let owner = self.prepared.owners.get(&key)?;
        let observed = match owner {
            RegistryMetadataOwner::Fresh(value) => Some(value.wheel().file_facts().locator().url().as_str()),
            RegistryMetadataOwner::Locked(value) => value.file_facts().map(|file| file.locator().url().as_str()),
            _ => None,
        };
        observed.or_else(|| self.prepared.root.model().and_then(|model| model.locator(owner.pin().id())))
    }
    pub(crate) fn recheck_for(&self, root: &BoundPythonLock) -> Result<(), RegistryClosureError> {
        self.prepared
            .recheck_for(root)
            .map_err(|_| RegistryClosureError::Revoked)
    }
}
impl fmt::Debug for ClosedRegistryGraph<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ClosedRegistryGraph")
            .field("selected", &self.plan().selected.len())
            .field("effective_roots", &self.checked.roots.len())
            .field("effective_edges", &self.checked.edges.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum RegistryClosureError {
    #[error("registry graph closure deadline expired")]
    Deadline,
    #[error("the actual project lock or metadata owner was revoked")]
    Revoked,
    #[error("selected graph and actual metadata owners differ")]
    IdentityMismatch,
    #[error("actual wheel is incompatible with the frozen target tags")]
    TargetMismatch,
    #[error("original dependency graph validation failed: {0}")]
    Graph(GraphCheckError),
}

/// Consumes the entire prepared attempt; no caller supplies replacement roots,
/// target values, metadata facts, source pins or selected owners here.
pub(crate) fn close_registry_plan<'root>(
    prepared: RegistryPreparedPlan<'root>,
    limits: GraphCheckLimits,
) -> Result<ClosedRegistryGraph<'root>, RegistryClosureError> {
    close_with_finish(prepared, limits, || {})
}

fn current_deadline(prepared: &RegistryPreparedPlan<'_>) -> Result<(), RegistryClosureError> {
    if Instant::now() >= prepared.deadline() {
        return Err(RegistryClosureError::Deadline);
    }
    Ok(())
}
fn same_version(left: &str, right: &str) -> bool {
    matches!((Pep440::parse_version(left), Pep440::parse_version(right)),
        (Some(left), Some(right)) if left == right)
}

pub(super) fn close_with_finish<'root>(
    prepared: RegistryPreparedPlan<'root>,
    limits: GraphCheckLimits,
    finish: impl FnOnce(),
) -> Result<ClosedRegistryGraph<'root>, RegistryClosureError> {
    current_deadline(&prepared)?;
    limits.validate().map_err(RegistryClosureError::Graph)?;
    if prepared.roots.len() > limits.max_roots || prepared.plan.selected.len() > limits.max_nodes {
        return Err(RegistryClosureError::Graph(GraphCheckError::Limit));
    }
    prepared
        .root
        .recheck()
        .map_err(|_| RegistryClosureError::Revoked)?;
    if prepared.owners.len() != prepared.plan.selected.len() {
        return Err(RegistryClosureError::IdentityMismatch);
    }
    let mut keys = BTreeSet::new();
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(prepared.plan.selected.len())
        .map_err(|_| RegistryClosureError::Graph(GraphCheckError::Limit))?;
    for selected in &prepared.plan.selected {
        current_deadline(&prepared)?;
        if !keys.insert(selected.key) {
            return Err(RegistryClosureError::IdentityMismatch);
        }
        let owner = prepared
            .owner(selected.key)
            .ok_or(RegistryClosureError::IdentityMismatch)?;
        owner
            .recheck_for(prepared.root)
            .map_err(|_| RegistryClosureError::Revoked)?;
        let file = owner.pin().file();
        let source_kind_matches = match file.kind() {
            RegistryArtifactKind::Wheel => owner.build_record().is_none(),
            RegistryArtifactKind::Sdist => owner.build_record().is_some(),
        };
        if !source_kind_matches
            || file.project() != selected.name
            || !same_version(file.version(), &selected.version)
            || owner.facts().name().as_str() != selected.name
            || !same_version(owner.facts().version(), &selected.version)
        {
            return Err(RegistryClosureError::IdentityMismatch);
        }
        if let Some(manifest) = owner.manifest() {
            if manifest.name() != selected.name
                || !same_version(manifest.version(), &selected.version)
            {
                return Err(RegistryClosureError::IdentityMismatch);
            }
            if !manifest
                .tags()
                .iter()
                .any(|tag| prepared.supported().ordered().contains(tag))
            {
                return Err(RegistryClosureError::TargetMismatch);
            }
        } else {
            let wheel = pakro_index::parse_wheel_filename(owner.output_filename())
                .ok_or(RegistryClosureError::IdentityMismatch)?;
            if prepared
                .supported()
                .best_rank(&wheel.tags)
                .map_err(|_| RegistryClosureError::TargetMismatch)?
                .is_none()
            {
                return Err(RegistryClosureError::TargetMismatch);
            }
        }
        current_deadline(&prepared)?;
        observations.push(GraphNodeObservation {
            selected,
            facts: owner.facts(),
        });
    }
    let checked = prepared
        .with_root_scopes(|scopes| {
            check_graph_scoped(
                prepared.roots(),
                &observations,
                prepared.environment(),
                scopes,
                prepared.semantics(),
                prepared.marker_limits(),
                limits,
            )
        })
        .map_err(RegistryClosureError::Graph)?;
    finish();
    current_deadline(&prepared)?;
    prepared
        .root
        .recheck()
        .map_err(|_| RegistryClosureError::Revoked)?;
    current_deadline(&prepared)?;
    Ok(ClosedRegistryGraph { prepared, checked })
}

// Source-owned retention. Every field comes from the consumed real closed
// attempt; no DTO constructor or archive extraction is exposed.
use crate::packages::python_resolution::source::fresh::wheel::sidecar::RetainedFreshWheel;
use crate::packages::python_resolution::source::replay::metadata::RetainedLockedWheel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ResolvedBuildError {
    #[error("resolved build deadline expired")]
    Deadline,
    #[error("resolved source scope is no longer current")]
    Revoked,
    #[error("installed source receipt differs from the closed graph")]
    SourceReceipt,
}

pub(crate) struct RetainedGraphEvidence<'root> {
    root: &'root BoundPythonLock,
    deadline: Instant,
    owners: BTreeMap<CandidateKey, RetainedRegistryOwner<'root>>,
}
enum RetainedRegistryOwner<'root> {
    Fresh(RetainedFreshWheel<'root>),
    Locked(RetainedLockedWheel<'root>),
    Built(RetainedBuiltRegistryWheel<'root>),
    Published(PublishedRegistryWheel<'root>),
    Held(held_wheel::HeldRegistryWheel<'root>),
}

pub(crate) struct RetainedMixedGraphEvidence<'root> {
    root: &'root BoundPythonLock,
    deadline: Instant,
    plan: NativePlan,
    checked: CheckedGraphData,
    registry: BTreeMap<CandidateKey, RetainedRegistryOwner<'root>>,
    registry_urls: BTreeMap<CandidateKey, String>,
    conda: BTreeMap<CandidateKey, conda_metadata::CondaDistribution<'root>>,
}
impl RetainedMixedGraphEvidence<'_> {
    pub(crate) fn plan(&self) -> &NativePlan { &self.plan }
    pub(crate) fn checked(&self) -> &CheckedGraphData { &self.checked }
    pub(crate) fn conda_owner(&self, key: CandidateKey) -> Option<&conda_metadata::CondaDistribution<'_>> { self.conda.get(&key) }
    pub(crate) fn registry_pin(&self, key: CandidateKey) -> Option<&ContentPin> { self.registry.get(&key).map(RetainedRegistryOwner::pin) }
    pub(crate) fn registry_url(&self, key: CandidateKey) -> Option<&str> { self.registry_urls.get(&key).map(String::as_str) }
    pub(crate) fn recheck(&self) -> Result<(), ResolvedBuildError> {
        if Instant::now() >= self.deadline { return Err(ResolvedBuildError::Deadline); }
        self.root.recheck().map_err(|_| ResolvedBuildError::Revoked)?;
        for owner in self.registry.values() { owner.recheck_for(self.root)?; }
        for owner in self.conda.values() { owner.recheck().map_err(|_| ResolvedBuildError::Revoked)?; }
        Ok(())
    }
}
impl<'root> ClosedMixedRegistryGraph<'root> {
    /// Transfer actual wheel sources without turning Conda records into wheel pins.
    pub(crate) fn into_shared_package_sources(
        self,
    ) -> Result<(Vec<(CandidateKey, SharedRegistryPackageSource)>, RetainedMixedGraphEvidence<'root>), ResolvedBuildError> {
        self.recheck().map_err(|_| ResolvedBuildError::Revoked)?;
        let ClosedMixedRegistryGraph { prepared, checked } = self;
        let MixedRegistryPreparedPlan { prepared, conda } = prepared;
        let root = prepared.root;
        let mut sources = Vec::new();
        let mut registry = BTreeMap::new();
        let mut registry_urls = BTreeMap::new();
        for (key, owner) in prepared.owners {
            owner.recheck_for(root).map_err(|_| ResolvedBuildError::Revoked)?;
            let url = match &owner {
                RegistryMetadataOwner::Fresh(value) => Some(value.wheel().file_facts().locator().url().as_str()),
                RegistryMetadataOwner::Locked(value) => value.file_facts().map(|file| file.locator().url().as_str()),
                _ => None,
            }.or_else(|| root.model().and_then(|model| model.locator(owner.pin().id()).or_else(|| model.mixed().iter().flat_map(|graph| &graph.nodes).find_map(|node| {
                match &node.source {
                    crate::packages::python_resolution::lock_model::MixedLockSource::Registry { pin, url }
                        if pin == &hex::encode(owner.pin().id().as_bytes()) => url.as_deref(),
                    _ => None,
                }
            }))));
            if let Some(url) = url {
                if !crate::packages::python_resolution::lock_model::valid_registry_url(url, owner.pin().file().filename()) {
                    return Err(ResolvedBuildError::SourceReceipt);
                }
                registry_urls.insert(key, url.to_owned());
            }
            let (source, retained) = match owner {
                RegistryMetadataOwner::Fresh(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (SharedRegistryPackageSource::Archive(archive), RetainedRegistryOwner::Fresh(retained))
                }
                RegistryMetadataOwner::Locked(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (SharedRegistryPackageSource::Archive(archive), RetainedRegistryOwner::Locked(retained))
                }
                RegistryMetadataOwner::Built(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (SharedRegistryPackageSource::Archive(archive), RetainedRegistryOwner::Built(retained))
                }
                RegistryMetadataOwner::Published(owner) => {
                    let (package, retained) = owner.into_parts();
                    (SharedRegistryPackageSource::Existing(package), RetainedRegistryOwner::Published(retained))
                }
                RegistryMetadataOwner::Held(owner) => {
                    let package = owner.package().clone();
                    (SharedRegistryPackageSource::Existing(package), RetainedRegistryOwner::Held(owner))
                }
            };
            sources.push((key, source));
            registry.insert(key, retained);
        }
        let evidence = RetainedMixedGraphEvidence {
            root, deadline: prepared.deadline, plan: prepared.plan, checked, registry, registry_urls, conda,
        };
        evidence.recheck()?;
        Ok((sources, evidence))
    }
}
impl RetainedRegistryOwner<'_> {
    fn pin(&self) -> &ContentPin {
        match self {
            Self::Fresh(owner) => owner.pin(),
            Self::Locked(owner) => owner.pin(),
            Self::Built(owner) => owner.pin(),
            Self::Published(owner) => owner.pin(),
            Self::Held(owner) => owner.pin(),
        }
    }
    fn recheck_for(&self, root: &BoundPythonLock) -> Result<(), ResolvedBuildError> {
        match self {
            Self::Fresh(owner) => owner
                .recheck_for(root)
                .map_err(|_| ResolvedBuildError::Revoked),
            Self::Locked(owner) => owner
                .recheck_for(root)
                .map_err(|_| ResolvedBuildError::Revoked),
            Self::Built(owner) => owner
                .recheck_for(root)
                .map_err(|_| ResolvedBuildError::Revoked),
            Self::Published(owner) => owner
                .recheck_for(root)
                .map_err(|_| ResolvedBuildError::Revoked),
            Self::Held(owner) => owner.recheck_for(root).map_err(|_| ResolvedBuildError::Revoked),
        }
    }
}
impl RetainedGraphEvidence<'_> {
    pub(crate) fn recheck(
        &self,
        caller_deadline: tokio::time::Instant,
    ) -> Result<(), ResolvedBuildError> {
        let check = || {
            if Instant::now() >= self.deadline || tokio::time::Instant::now() >= caller_deadline {
                Err(ResolvedBuildError::Deadline)
            } else {
                Ok(())
            }
        };
        check()?;
        self.root
            .recheck()
            .map_err(|_| ResolvedBuildError::Revoked)?;
        for owner in self.owners.values() {
            check()?;
            owner.recheck_for(self.root)?;
        }
        check()?;
        self.root
            .recheck()
            .map_err(|_| ResolvedBuildError::Revoked)?;
        check()
    }
}
impl<'root> ClosedRegistryGraph<'root> {
    /// A Ready tree retains its actual lease and original artifact identity;
    /// it is not re-encoded as a ZIP archive.
    pub(crate) fn into_shared_package_sources(
        self,
    ) -> Result<
        (
            Vec<SharedRegistryPackageSource>,
            RetainedGraphEvidence<'root>,
        ),
        ResolvedBuildError,
    > {
        self.into_mixed_sources()
    }

    /// Transfers inspected wheel owners to the shared package publisher while
    /// retaining every lock/source binding for checks through catalog commit.
    /// This is the same source-consumption kernel used by resolved builds; it
    /// grants neither runtime-copy nor publication authority.
    pub(crate) fn into_shared_sources(
        self,
    ) -> Result<
        (
            Vec<crate::packages::python_wheel_source::InspectedWheelArchive>,
            RetainedGraphEvidence<'root>,
        ),
        ResolvedBuildError,
    > {
        let (sources, evidence) = self.into_mixed_sources()?;
        let mut archives = Vec::new();
        for source in sources {
            match source {
                SharedRegistryPackageSource::Archive(archive) => archives.push(archive),
                SharedRegistryPackageSource::Existing(_) => {
                    return Err(ResolvedBuildError::SourceReceipt);
                }
            }
        }
        Ok((archives, evidence))
    }
    fn into_mixed_sources(
        self,
    ) -> Result<
        (
            Vec<SharedRegistryPackageSource>,
            RetainedGraphEvidence<'root>,
        ),
        ResolvedBuildError,
    > {
        current_deadline(&self.prepared).map_err(|_| ResolvedBuildError::Deadline)?;
        self.recheck_for(self.prepared.root)
            .map_err(|_| ResolvedBuildError::Revoked)?;
        let ClosedRegistryGraph { prepared, .. } = self;
        let RegistryPreparedPlan {
            root,
            deadline,
            owners,
            ..
        } = prepared;
        let mut archives = Vec::new();
        archives
            .try_reserve_exact(owners.len())
            .map_err(|_| ResolvedBuildError::SourceReceipt)?;
        let mut retained = BTreeMap::new();
        for (key, owner) in owners {
            owner
                .recheck_for(root)
                .map_err(|_| ResolvedBuildError::Revoked)?;
            let (archive, evidence) = match owner {
                RegistryMetadataOwner::Fresh(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (
                        SharedRegistryPackageSource::Archive(archive),
                        RetainedRegistryOwner::Fresh(retained),
                    )
                }
                RegistryMetadataOwner::Locked(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (
                        SharedRegistryPackageSource::Archive(archive),
                        RetainedRegistryOwner::Locked(retained),
                    )
                }
                RegistryMetadataOwner::Built(owner) => {
                    let (archive, retained) = owner.into_build_evidence();
                    (
                        SharedRegistryPackageSource::Archive(archive),
                        RetainedRegistryOwner::Built(retained),
                    )
                }
                RegistryMetadataOwner::Published(owner) => {
                    let (package, retained) = owner.into_parts();
                    (
                        SharedRegistryPackageSource::Existing(package),
                        RetainedRegistryOwner::Published(retained),
                    )
                }
                RegistryMetadataOwner::Held(owner) => {
                    let package = owner.package().clone();
                    (SharedRegistryPackageSource::Existing(package), RetainedRegistryOwner::Held(owner))
                }
            };
            archives.push(archive);
            retained.insert(key, evidence);
        }
        let evidence = RetainedGraphEvidence {
            root,
            deadline,
            owners: retained,
        };
        evidence.recheck(tokio::time::Instant::from_std(deadline))?;
        Ok((archives, evidence))
    }
}

pub(crate) enum SharedRegistryPackageSource {
    Archive(crate::packages::python_wheel_source::InspectedWheelArchive),
    Existing(crate::libraries::package::lease::PackageLease),
}
