//! Caller-budget entry points for the existing source acquisition owners.
//!
//! Legacy entry points and these explicit caller-budget entries share the
//! same HTTP, binding, observation and expected-verification kernels.

use super::fresh::{FreshPinResult, FreshPinnedArtifact};
use super::*;
use crate::packages::python_resolution::lock_model::LockModelLimits;
use crate::packages::python_resolution::lock_session::FreshFileSlot;
use std::time::Instant;

pub(crate) async fn fetch_registry_project_at_deadline(
    client: &SimpleIndexClient,
    session: PythonSourceSession<'_>,
    project: &NormalizedPythonName,
    policy: SimpleFetchPolicy,
    limits: SimpleClientLimits,
    deadline: Instant,
) -> SourceResolutionResult<RegistryProjectLookup> {
    fetch_registry_project_with_budget(client, session, project, policy, limits, Some(deadline))
        .await
}

pub(crate) async fn acquire_registry_file_at_deadline(
    selected: &SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    scratch_root: &Path,
    deadline: Instant,
) -> SourceResolutionResult<AcquiredRegistryFile> {
    acquire_registry_file_with_budget(
        selected,
        client,
        session,
        network,
        limits,
        scratch_root,
        Some(deadline),
    )
    .await
}

pub(crate) async fn acquire_fresh_registry_file_at_deadline<'lock>(
    slot: FreshFileSlot<'lock>,
    selected: SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    lock_limits: LockModelLimits,
    scratch_root: &Path,
    deadline: Instant,
) -> FreshPinResult<FreshPinnedArtifact<'lock>> {
    super::fresh::acquire_fresh_with_budget(
        slot,
        selected,
        client,
        session,
        network,
        limits,
        lock_limits,
        scratch_root,
        Some(deadline),
    )
    .await
}

pub(super) fn check(deadline: Option<Instant>) -> SourceResolutionResult<()> {
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        Err(SourceResolutionError::Transport(SimpleClientError::Timeout))
    } else {
        Ok(())
    }
}
pub(super) fn error(
    deadline: Option<Instant>,
    fallback: SourceResolutionError,
) -> SourceResolutionError {
    check(deadline).err().unwrap_or(fallback)
}

/// Only the existing verifier owns the copy/hash loop. Each bounded file read
/// checks the same absolute limit; no pathname or writable capability escapes.
pub(super) struct Reader<R> {
    input: R,
    deadline: Option<Instant>,
}
impl<R> Reader<R> {
    pub(super) fn new(input: R, deadline: Option<Instant>) -> Self {
        Self { input, deadline }
    }
}
impl<R: Read> Read for Reader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check(self.deadline).map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?;
        let result = self.input.read(output);
        check(self.deadline).map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?;
        result
    }
}
