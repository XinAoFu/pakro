//! Frozen call inputs, not observations, verified target facts or IO permits.
//! Later context binding retains real owners alongside this explicit snapshot.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use pakro_index::python_source::{
    PythonRegistry, PythonSourcePolicy, ordered_python_source_indices,
};
use tokio_util::sync::CancellationToken;

use crate::config::Settings;
use crate::libraries::RequestedLibraryLocations;
use crate::python_runtime::PythonRuntimeDescriptor;

use super::errors::{MissingOperationInput, OperationError};
use super::policy::OperationPolicy;

#[derive(Clone)]
pub struct PythonSourceInputs {
    pub registries: Vec<PythonRegistry>,
    pub policy: PythonSourcePolicy,
}

pub struct OperationContextInputs<'a> {
    pub settings: &'a Settings,
    pub policy: OperationPolicy,
    pub cancellation: CancellationToken,
    pub data_root: PathBuf,
    pub cache_root: PathBuf,
    pub python_sources: Option<PythonSourceInputs>,
    pub python_target_description: Option<PythonRuntimeDescriptor>,
}

struct FrozenOperationInputs {
    settings: Settings,
    policy: OperationPolicy,
    cancellation: CancellationToken,
    locations: RequestedLibraryLocations,
    python_sources: Option<PythonSourceInputs>,
    python_target_description: Option<PythonRuntimeDescriptor>,
}

/// Clones share the same frozen policy and cancellation signal. No method can
/// reset the deadline or reload settings, credentials, sources or the host target.
#[derive(Clone)]
pub struct OperationContext {
    inner: Arc<FrozenOperationInputs>,
}

pub fn capture_operation_context(
    inputs: OperationContextInputs<'_>,
) -> Result<OperationContext, OperationError> {
    check_control(&inputs.policy, &inputs.cancellation)?;
    let locations = RequestedLibraryLocations::new(inputs.data_root, inputs.cache_root)
        .map_err(|_| OperationError::InvalidLibraryLocations)?;
    if let Some(sources) = &inputs.python_sources {
        let registries: Vec<_> = sources.registries.iter().collect();
        ordered_python_source_indices(&registries, &sources.policy)
            .map_err(|_| OperationError::InvalidSourceConfiguration)?;
    }
    let context = OperationContext {
        inner: Arc::new(FrozenOperationInputs {
            settings: inputs.settings.clone(),
            policy: inputs.policy,
            cancellation: inputs.cancellation,
            locations,
            python_sources: inputs.python_sources,
            python_target_description: inputs.python_target_description,
        }),
    };
    context.checkpoint()?;
    Ok(context)
}

impl OperationContext {
    pub(crate) fn same_invocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation.clone()
    }
    pub fn policy(&self) -> &OperationPolicy {
        &self.inner.policy
    }
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }
    pub fn requested_data_root(&self) -> &Path {
        self.inner.locations.data_root()
    }
    pub fn requested_cache_root(&self) -> &Path {
        self.inner.locations.cache_root()
    }

    pub fn checkpoint(&self) -> Result<(), OperationError> {
        check_control(&self.inner.policy, &self.inner.cancellation)
    }

    pub fn python_sources(&self) -> Result<&PythonSourceInputs, OperationError> {
        self.checkpoint()?;
        self.inner
            .python_sources
            .as_ref()
            .ok_or(OperationError::MissingInput(
                MissingOperationInput::PythonSources,
            ))
    }

    pub fn python_target_description(&self) -> Result<&PythonRuntimeDescriptor, OperationError> {
        self.checkpoint()?;
        self.inner
            .python_target_description
            .as_ref()
            .ok_or(OperationError::MissingInput(
                MissingOperationInput::PythonTargetDescription,
            ))
    }
}

fn check_control(
    policy: &OperationPolicy,
    cancellation: &CancellationToken,
) -> Result<(), OperationError> {
    if cancellation.is_cancelled() {
        return Err(OperationError::Cancelled);
    }
    if Instant::now() >= policy.deadline {
        return Err(OperationError::DeadlineExceeded);
    }
    Ok(())
}

impl fmt::Debug for OperationContext {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("OperationContext")
            .field("has_python_sources", &self.inner.python_sources.is_some())
            .field(
                "has_target_description",
                &self.inner.python_target_description.is_some(),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
