//! Read-only portable lock data from an actual local build and its retained inputs.
use super::*;
use crate::packages::python_resolution::lock_model::{
    GroupKey, LockModelError, LockModelLimits, PythonLockModel,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ResolvedProjectionError {
    #[error("resolved lock projection limits must be positive")]
    InvalidLimits,
    #[error("resolved lock projection deadline expired")]
    Deadline,
    #[error("resolved lock projection target differs from the actual build")]
    TargetMismatch,
    #[error("resolved lock projection original scope changed")]
    Revoked,
    #[error("resolved lock projection runtime or environment changed")]
    Runtime,
    #[error("resolved lock projection data failed: {0}")]
    Model(LockModelError),
}
impl PreparedResolvedEnvironment<'_> {
    pub(crate) fn merged_lock_model(
        &self,
        key: &GroupKey,
        limits: LockModelLimits,
        deadline: Instant,
    ) -> Result<PythonLockModel, ResolvedProjectionError> {
        project_with_finish(self, key, limits, deadline, || {})
    }
}
fn projection_error(error: ResolvedBuildError) -> ResolvedProjectionError {
    match error {
        ResolvedBuildError::InvalidLimits => ResolvedProjectionError::InvalidLimits,
        ResolvedBuildError::Deadline => ResolvedProjectionError::Deadline,
        ResolvedBuildError::TargetMismatch => ResolvedProjectionError::TargetMismatch,
        ResolvedBuildError::Revoked => ResolvedProjectionError::Revoked,
        ResolvedBuildError::SourceReceipt
        | ResolvedBuildError::Runtime
        | ResolvedBuildError::Build => ResolvedProjectionError::Runtime,
    }
}
fn project_with_finish(
    prepared: &PreparedResolvedEnvironment<'_>,
    key: &GroupKey,
    limits: LockModelLimits,
    deadline: Instant,
    finish: impl FnOnce(),
) -> Result<PythonLockModel, ResolvedProjectionError> {
    limits
        .validate()
        .map_err(|_| ResolvedProjectionError::InvalidLimits)?;
    let deadline = deadline.min(prepared.deadline);
    check_deadline(deadline).map_err(projection_error)?;
    if key.target() != prepared.target().fingerprint() {
        return Err(ResolvedProjectionError::TargetMismatch);
    }
    prepared.recheck_current().map_err(projection_error)?;
    check_deadline(deadline).map_err(projection_error)?;
    let runtime = crate::packages::python_resolution::lock_model::RuntimePin::new(
        *prepared.runtime_content_tree_sha256(),
        1,
        *prepared.target().fingerprint(),
    )
    .map_err(ResolvedProjectionError::Model)?;
    let model=prepared.evidence.project_model(key,runtime,limits,deadline).map_err(|error|match error {
        crate::packages::python_resolution::source::registry_depot::closed::RetainedProjectionError::Deadline=>ResolvedProjectionError::Deadline,
        crate::packages::python_resolution::source::registry_depot::closed::RetainedProjectionError::Model(error)=>ResolvedProjectionError::Model(error),
    })?;
    finish();
    check_deadline(deadline).map_err(projection_error)?;
    prepared.recheck_current().map_err(projection_error)?;
    check_deadline(deadline).map_err(projection_error)?;
    Ok(model)
}

#[cfg(all(test, unix))]
#[path = "projection/tests.rs"]
mod tests;
