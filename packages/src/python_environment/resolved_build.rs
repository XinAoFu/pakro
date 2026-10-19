//! One controlled handoff from actual graph/runtime owners into the local build.
//! This result has no configuration freshness or durable publication authority.

use std::path::Path;
use thiserror::Error;
use tokio::time::Instant;

use super::build::{EnvironmentBuildLimits, PreparedEnvironmentBuild, prepare_environment_build};
use super::prefix::UnpublishedPrefix;
use super::runtime_snapshot::OwnedRuntimeSnapshot;
use super::runtime_startup::{StartupAdapterBinding, installed::EnvironmentVerificationReceipt};
use super::runtime_target::{ObservedRegistryRuntimeTarget, freeze_observed_target};
use crate::packages::python_resolution::graph_check::{CheckedGraphData, GraphCheckLimits};
use crate::packages::python_resolution::lock_model::ContentPin;
use crate::packages::python_resolution::source::locked_graph::{
    FrozenRegistryTarget, FrozenTargetLimits,
};
use crate::packages::python_resolution::source::registry_depot::closed::{
    ClosedRegistryGraph, RetainedGraphEvidence,
};
use crate::packages::python_wheel_install::PreparedWheelSource;
use crate::packages::python_wheel_source::InspectedWheelArchive;
use pakro_resolver::python_native::{CandidateKey, NativePlan};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedBuildLimits {
    pub build: EnvironmentBuildLimits,
    pub graph: GraphCheckLimits,
    pub target: FrozenTargetLimits,
}
impl Default for ResolvedBuildLimits {
    fn default() -> Self {
        Self {
            build: Default::default(),
            graph: Default::default(),
            target: Default::default(),
        }
    }
}
impl ResolvedBuildLimits {
    fn validate(self) -> Result<(), ResolvedBuildError> {
        self.build
            .validate()
            .map_err(|_| ResolvedBuildError::InvalidLimits)?;
        self.graph
            .validate()
            .map_err(|_| ResolvedBuildError::InvalidLimits)?;
        self.target
            .validate()
            .map_err(|_| ResolvedBuildError::InvalidLimits)
    }
}

pub(crate) struct PreparedResolvedEnvironment<'lock> {
    // Environment/base cleanup precedes dropping the source/slot evidence.
    build: PreparedEnvironmentBuild,
    evidence: RetainedGraphEvidence<'lock>,
    target: FrozenRegistryTarget,
    limits: ResolvedBuildLimits,
    deadline: Instant,
}
impl PreparedResolvedEnvironment<'_> {
    pub(crate) fn final_prefix(&self) -> &Path {
        self.build.final_prefix()
    }
    pub(crate) fn executable(&self) -> &Path {
        self.build.executable()
    }
    pub(crate) fn runtime_content_tree_sha256(&self) -> &[u8; 32] {
        self.build.runtime_content_tree_sha256()
    }
    pub(crate) fn runtime_inventory_sha256(&self) -> &[u8; 32] {
        self.build.runtime_inventory_sha256()
    }
    pub(crate) fn verification(&self) -> &EnvironmentVerificationReceipt {
        self.build.verification()
    }
    pub(crate) fn sources(&self) -> &[PreparedWheelSource] {
        self.build.sources()
    }
    pub(crate) fn plan(&self) -> &NativePlan {
        self.evidence.plan()
    }
    pub(crate) fn checked(&self) -> &CheckedGraphData {
        self.evidence.checked()
    }
    pub(crate) fn target(&self) -> &FrozenRegistryTarget {
        &self.target
    }
    pub(crate) fn policy_notices(
        &self,
        key: CandidateKey,
    ) -> &[crate::packages::python_resolution::candidate_policy::PolicyNotice] {
        self.evidence.policy_notices(key)
    }
    pub(crate) fn source_pin(&self, key: CandidateKey) -> Option<&ContentPin> {
        self.evidence.pin(key)
    }
    pub(crate) fn recheck_current(&self) -> Result<(), ResolvedBuildError> {
        check_deadline(self.deadline)?;
        self.evidence.recheck(self.deadline)?;
        self.build
            .recheck(self.limits.build.inventory)
            .map_err(|_| ResolvedBuildError::Runtime)?;
        check_deadline(self.deadline)
    }
}
impl std::fmt::Debug for PreparedResolvedEnvironment<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("PreparedResolvedEnvironment")
            .field("sources", &self.sources().len())
            .finish_non_exhaustive()
    }
}

// This gate has no public constructor or general extraction/callback API.
pub(crate) struct ResolvedBuildPermit {
    runtime: ObservedRegistryRuntimeTarget,
    destination: UnpublishedPrefix,
    startup: StartupAdapterBinding,
    limits: ResolvedBuildLimits,
    deadline: Instant,
}
impl ResolvedBuildPermit {
    pub(crate) fn source_deadline(&self) -> std::time::Instant {
        self.deadline.into_std()
    }
    pub(crate) fn graph_limits(&self) -> GraphCheckLimits {
        self.limits.graph
    }
    pub(crate) fn target_limits(&self) -> FrozenTargetLimits {
        self.limits.target
    }
    pub(crate) async fn build_with_sources<'lock>(
        self,
        archives: Vec<InspectedWheelArchive>,
        evidence: RetainedGraphEvidence<'lock>,
        target: FrozenRegistryTarget,
    ) -> Result<PreparedResolvedEnvironment<'lock>, ResolvedBuildError> {
        let deadline = self.deadline.min(Instant::from_std(evidence.deadline()));
        check_deadline(deadline)?;
        evidence.recheck(deadline)?;
        self.runtime
            .target()
            .check_limits(self.limits.target)
            .map_err(|_| ResolvedBuildError::InvalidLimits)?;
        if !target.matches(self.runtime.target()) {
            return Err(ResolvedBuildError::TargetMismatch);
        }
        self.runtime
            .recheck(self.limits.build.inventory, deadline)
            .map_err(|_| ResolvedBuildError::Runtime)?;
        let permit = RuntimeBuildPermit {
            archives,
            evidence,
            target,
            destination: self.destination,
            startup: self.startup,
            limits: self.limits,
            deadline,
        };
        self.runtime.complete_resolved_build(permit).await
    }
}

// Only the actual runtime owner can supply its private snapshot to this
// fixed consumer. The permit cannot be constructed outside this module.
pub(super) struct RuntimeBuildPermit<'lock> {
    archives: Vec<InspectedWheelArchive>,
    evidence: RetainedGraphEvidence<'lock>,
    target: FrozenRegistryTarget,
    destination: UnpublishedPrefix,
    startup: StartupAdapterBinding,
    limits: ResolvedBuildLimits,
    deadline: Instant,
}
impl<'lock> RuntimeBuildPermit<'lock> {
    pub(super) async fn complete(
        self,
        snapshot: OwnedRuntimeSnapshot,
        observed: FrozenRegistryTarget,
    ) -> Result<PreparedResolvedEnvironment<'lock>, ResolvedBuildError> {
        check_deadline(self.deadline)?;
        if !self.target.matches(&observed) {
            return Err(ResolvedBuildError::TargetMismatch);
        }
        let content = *snapshot.content_tree_sha256();
        let inventory = *snapshot.inventory_sha256();
        let build = prepare_environment_build(
            snapshot,
            self.archives,
            self.destination,
            self.startup,
            self.limits.build,
            self.deadline,
        )
        .await
        .map_err(|_| ResolvedBuildError::Build)?;
        check_deadline(self.deadline)?;
        if build.runtime_content_tree_sha256() != &content
            || build.runtime_inventory_sha256() != &inventory
        {
            return Err(ResolvedBuildError::Runtime);
        }
        self.evidence
            .check_sources(build.sources(), self.deadline)?;
        let actual = self
            .target
            .with_root_scope(|scope| {
                freeze_observed_target(
                    build.verification().observed_runtime(),
                    scope,
                    self.target.semantics(),
                    self.limits.target,
                )
            })
            .map_err(|_| ResolvedBuildError::TargetMismatch)?;
        if !actual.matches(&self.target) {
            return Err(ResolvedBuildError::TargetMismatch);
        }
        let prepared = PreparedResolvedEnvironment {
            build,
            evidence: self.evidence,
            target: self.target,
            limits: self.limits,
            deadline: self.deadline,
        };
        Ok(prepared)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ResolvedBuildError {
    #[error("resolved build limits must be positive")]
    InvalidLimits,
    #[error("resolved build deadline expired")]
    Deadline,
    #[error("closed source target and observed runtime differ")]
    TargetMismatch,
    #[error("resolved source scope is no longer current")]
    Revoked,
    #[error("installed source receipt differs from the closed graph")]
    SourceReceipt,
    #[error("resolved runtime snapshot changed")]
    Runtime,
    #[error("resolved local environment build failed")]
    Build,
}
fn check_deadline(deadline: Instant) -> Result<(), ResolvedBuildError> {
    if Instant::now() >= deadline {
        return Err(ResolvedBuildError::Deadline);
    }
    Ok(())
}

pub(crate) async fn prepare_resolved_environment<'lock>(
    graph: ClosedRegistryGraph<'lock>,
    runtime: ObservedRegistryRuntimeTarget,
    destination: UnpublishedPrefix,
    startup: StartupAdapterBinding,
    limits: ResolvedBuildLimits,
    deadline: Instant,
) -> Result<PreparedResolvedEnvironment<'lock>, ResolvedBuildError> {
    prepare_with_finish(
        graph,
        runtime,
        destination,
        startup,
        limits,
        deadline,
        || {},
    )
    .await
}
async fn prepare_with_finish<'lock>(
    graph: ClosedRegistryGraph<'lock>,
    runtime: ObservedRegistryRuntimeTarget,
    destination: UnpublishedPrefix,
    startup: StartupAdapterBinding,
    limits: ResolvedBuildLimits,
    deadline: Instant,
    finish: impl FnOnce(),
) -> Result<PreparedResolvedEnvironment<'lock>, ResolvedBuildError> {
    limits.validate()?;
    check_deadline(deadline)?;
    let prepared = graph
        .build_with(ResolvedBuildPermit {
            runtime,
            destination,
            startup,
            limits,
            deadline,
        })
        .await?;
    finish();
    prepared.recheck_current()?;
    Ok(prepared)
}

#[cfg(all(test, unix))]
#[path = "resolved_build/tests.rs"]
mod tests;

#[path = "resolved_build/projection.rs"]
mod projection;

#[cfg(all(test, unix))]
pub(crate) use tests::{ActualRuntime, actual_clt_inputs};

pub(crate) use projection::ResolvedProjectionError;
