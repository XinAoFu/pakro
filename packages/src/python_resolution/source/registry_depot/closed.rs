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
use crate::packages::python_resolution::candidate_policy::PolicyNotice;
use crate::packages::python_resolution::source::fresh::wheel::sidecar::RetainedFreshWheel;
use crate::packages::python_resolution::source::replay::metadata::RetainedLockedWheel;
use crate::packages::python_wheel_install::PreparedWheelSource;

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
    roots: Vec<PythonRequirementSyntax>,
    root_scopes: Option<Vec<RegistryRootScope>>,
    environment: RegistryEnvironment,
    root_scope: RegistryRootScope,
    semantics: MarkerSemantics,
    supported: SupportedWheelTags,
    deadline: Instant,
    marker_limits: PythonMarkerLimits,
    policy_notices: BTreeMap<CandidateKey, Vec<PolicyNotice>>,
    plan: NativePlan,
    checked: CheckedGraphData,
    owners: BTreeMap<CandidateKey, RetainedRegistryOwner<'root>>,
}
enum RetainedRegistryOwner<'root> {
    Fresh(RetainedFreshWheel<'root>),
    Locked(RetainedLockedWheel<'root>),
    Built(RetainedBuiltRegistryWheel<'root>),
    Published(PublishedRegistryWheel<'root>),
}
impl RetainedRegistryOwner<'_> {
    fn pin(&self) -> &ContentPin {
        match self {
            Self::Fresh(owner) => owner.pin(),
            Self::Locked(owner) => owner.pin(),
            Self::Built(owner) => owner.pin(),
            Self::Published(owner) => owner.pin(),
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
        }
    }
    fn output_filename(&self) -> &str {
        match self {
            Self::Built(owner) => &owner.record().output_filename,
            Self::Published(owner) => &owner.record().output_filename,
            _ => self.pin().file().filename(),
        }
    }
    fn output_digest(&self) -> Digest {
        match self {
            Self::Built(owner) => owner.output_digest(),
            Self::Published(owner) => owner.output_digest(),
            _ => self.pin().digest(),
        }
    }
    fn output_size(&self) -> u64 {
        match self {
            Self::Built(owner) => owner.record().output_size,
            Self::Published(owner) => owner.record().output_size,
            _ => self.pin().size(),
        }
    }
}
impl RetainedGraphEvidence<'_> {
    pub(crate) fn plan(&self) -> &NativePlan {
        &self.plan
    }
    pub(crate) fn checked(&self) -> &CheckedGraphData {
        &self.checked
    }
    pub(crate) fn policy_notices(&self, key: CandidateKey) -> &[PolicyNotice] {
        self.policy_notices
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn pin(&self, key: CandidateKey) -> Option<&ContentPin> {
        self.owners.get(&key).map(RetainedRegistryOwner::pin)
    }
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
    pub(crate) fn check_sources(
        &self,
        sources: &[PreparedWheelSource],
        deadline: tokio::time::Instant,
    ) -> Result<(), ResolvedBuildError> {
        if sources.len() != self.owners.len() || self.owners.len() != self.plan.selected.len() {
            return Err(ResolvedBuildError::SourceReceipt);
        }
        let mut seen = BTreeSet::new();
        for selected in &self.plan.selected {
            if tokio::time::Instant::now() >= deadline {
                return Err(ResolvedBuildError::Deadline);
            }
            let pin = self
                .pin(selected.key)
                .ok_or(ResolvedBuildError::SourceReceipt)?;
            let found = sources.iter().enumerate().find(|(_, source)| {
                source.owner().name() == pin.file().project()
                    && same_version(source.owner().version(), pin.file().version())
            });
            let Some((index, source)) = found else {
                return Err(ResolvedBuildError::SourceReceipt);
            };
            if !seen.insert(index)
                || source.filename() != self.owners[&selected.key].output_filename()
                || source.digest() != self.owners[&selected.key].output_digest()
                || source.size() != self.owners[&selected.key].output_size()
            {
                return Err(ResolvedBuildError::SourceReceipt);
            }
        }
        Ok(())
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
        let ClosedRegistryGraph { prepared, checked } = self;
        let RegistryPreparedPlan {
            root,
            roots,
            root_scopes,
            environment,
            root_scope,
            semantics,
            supported,
            deadline,
            marker_limits,
            policy_notices,
            plan,
            owners,
            usage: _,
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
            };
            archives.push(archive);
            retained.insert(key, evidence);
        }
        let evidence = RetainedGraphEvidence {
            root,
            roots,
            root_scopes,
            environment,
            root_scope,
            semantics,
            supported,
            deadline,
            marker_limits,
            policy_notices,
            plan,
            checked,
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
impl SharedRegistryPackageSource {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Archive(archive) => archive.manifest().name(),
            Self::Existing(package) => package.manifest().name(),
        }
    }
}
