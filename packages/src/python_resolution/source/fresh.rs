//! Fresh file pins require a live exact-file vacancy in an actually read lock.
//!
//! One consuming future owns the selected source, slot and actual transfer.
//! Neither an observation DTO nor a caller-supplied digest grants this right.

use super::*;
use crate::packages::python_resolution::lock_model::{
    ContentPin, LockModelLimits, PinEvidence, RegistryArtifactKind, RegistryFileRef,
    RegistryFileRefInput, canonical_registry_base_fingerprint,
};
use crate::packages::python_resolution::lock_session::{BoundPythonLock, FreshFileSlot};
use pakro_resolver::solve::{Pep440, VersionAdapter};

pub(crate) type FreshPinResult<T> = Result<T, FreshPinError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum FreshPinError {
    #[error("fresh pin limits must be positive and sufficient")]
    InvalidLimits,
    #[error("fresh file selection differs from the live exact-file slot")]
    IdentityMismatch,
    #[error("fresh file slot is no longer current")]
    Revoked,
    #[error("fresh source acquisition failed: {0}")]
    Source(SourceResolutionError),
    #[error("fresh content verification failed")]
    Snapshot,
    #[error("fresh portable pin data is invalid")]
    LockData,
}

/// A candidate pin and its verified bytes remain owned with the live slot.
/// This is neither a committed lock graph nor a wheel-writing capability.
pub(crate) struct FreshPinnedArtifact<'lock> {
    slot: FreshFileSlot<'lock>,
    pin: ContentPin,
    identity: RegistryFileIdentity,
    artifact: VerifiedArtifact,
}
impl FreshPinnedArtifact<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        &self.pin
    }
    pub(crate) fn filename(&self) -> &str {
        self.artifact.filename()
    }
    pub(crate) fn digest(&self) -> Digest {
        self.artifact.digest()
    }
    pub(crate) fn size(&self) -> u64 {
        self.artifact.size()
    }
    pub(crate) fn source_key(&self) -> SourceObservationKey {
        self.identity.source_key
    }
    pub(crate) fn recheck(&self) -> FreshPinResult<()> {
        self.slot.recheck().map_err(|_| FreshPinError::Revoked)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> FreshPinResult<()> {
        self.slot
            .recheck_for(owner)
            .map_err(|_| FreshPinError::Revoked)
    }
}
impl fmt::Debug for FreshPinnedArtifact<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("FreshPinnedArtifact")
            .field("pin", &self.pin)
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}
impl Read for FreshPinnedArtifact<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.artifact.read(output)
    }
}
impl Seek for FreshPinnedArtifact<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.artifact.seek(position)
    }
}

/// Pure data projection from a genuine, already-bound project selection. It
/// supplies no authority: only BoundPythonLock::lookup can issue a fresh slot.
pub(super) fn selected_file_ref(
    selected: &SelectedRegistryFile<'_>,
    limits: LockModelLimits,
) -> FreshPinResult<RegistryFileRef> {
    let project = &selected.project.data;
    let file = &project.project.files()[selected.file_index];
    let kind = match file.kind() {
        PythonDistributionKind::Wheel(_) => RegistryArtifactKind::Wheel,
        PythonDistributionKind::Sdist => RegistryArtifactKind::Sdist,
        _ => return Err(FreshPinError::LockData),
    };
    RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: project.registry.id().as_str(),
            canonical_base_fingerprint: canonical_registry_base_fingerprint(
                project.registry.canonical_base(),
            ),
            project: project.project.name().as_str(),
            version: file.version().ok_or(FreshPinError::LockData)?,
            filename: file.filename(),
            kind,
        },
        limits,
    )
    .map_err(|_| FreshPinError::LockData)
}

fn checked_limits(
    mut limits: SourceObservationLimits,
    lock_limits: LockModelLimits,
) -> FreshPinResult<SourceObservationLimits> {
    lock_limits
        .validate()
        .map_err(|_| FreshPinError::InvalidLimits)?;
    if limits.transfer.max_wire_bytes == 0
        || limits.transfer.max_header_bytes == 0
        || limits.transfer.max_headers == 0
        || limits.max_spool_bytes == 0
        || limits.max_identity_bytes == 0
        || limits.transfer.max_wire_bytes.checked_mul(2).is_none()
    {
        return Err(FreshPinError::InvalidLimits);
    }
    // Every actual transfer and spool uses the narrower lock/content budget.
    // Validate the original configured multiplication as well, before clamping.
    limits.transfer.max_wire_bytes = limits
        .transfer
        .max_wire_bytes
        .min(lock_limits.max_artifact_bytes);
    if limits
        .transfer
        .max_wire_bytes
        .checked_mul(2)
        .is_none_or(|required| required > limits.max_spool_bytes)
    {
        return Err(FreshPinError::InvalidLimits);
    }
    Ok(limits)
}

fn same_file(left: &RegistryFileRef, right: &RegistryFileRef) -> bool {
    left.registry_id() == right.registry_id()
        && left.canonical_base_fingerprint() == right.canonical_base_fingerprint()
        && left.project() == right.project()
        && left.filename() == right.filename()
        && left.kind() == right.kind()
        && match (
            Pep440::parse_version(left.version()),
            Pep440::parse_version(right.version()),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
}

fn validate_slot_file(file: &RegistryFileRef, limits: LockModelLimits) -> FreshPinResult<()> {
    let checked = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: file.registry_id(),
            canonical_base_fingerprint: *file.canonical_base_fingerprint(),
            project: file.project(),
            version: file.version(),
            filename: file.filename(),
            kind: file.kind(),
        },
        limits,
    )
    .map_err(|_| FreshPinError::LockData)?;
    if &checked != file {
        return Err(FreshPinError::IdentityMismatch);
    }
    Ok(())
}

pub(crate) async fn acquire_fresh_registry_file<'lock>(
    slot: FreshFileSlot<'lock>,
    selected: SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    lock_limits: LockModelLimits,
    scratch_root: &Path,
) -> FreshPinResult<FreshPinnedArtifact<'lock>> {
    acquire_fresh_with_budget(
        slot,
        selected,
        client,
        session,
        network,
        limits,
        lock_limits,
        scratch_root,
        None,
    )
    .await
}

pub(super) async fn acquire_fresh_with_budget<'lock>(
    slot: FreshFileSlot<'lock>,
    selected: SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    lock_limits: LockModelLimits,
    scratch_root: &Path,
    deadline: Option<std::time::Instant>,
) -> FreshPinResult<FreshPinnedArtifact<'lock>> {
    let limits = checked_limits(limits, lock_limits)?;
    caller_deadline::check(deadline).map_err(FreshPinError::Source)?;
    slot.recheck().map_err(|_| FreshPinError::Revoked)?;
    // Both inputs may have been created under looser limits. Charge the entire
    // portable identity before creating scratch files or asking a provider.
    validate_slot_file(slot.file(), lock_limits)?;
    let actual = selected_file_ref(&selected, lock_limits)?;
    if !same_file(slot.file(), &actual) {
        return Err(FreshPinError::IdentityMismatch);
    }
    let acquired = acquire_registry_file_with_budget(
        &selected,
        client,
        session,
        network,
        limits,
        scratch_root,
        deadline,
    )
    .await
    .map_err(FreshPinError::Source)?;
    caller_deadline::check(deadline).map_err(FreshPinError::Source)?;
    slot.recheck().map_err(|_| FreshPinError::Revoked)?;
    let (artifact, identity, evidence) = match acquired {
        AcquiredRegistryFile::Expected(source) => {
            // A genuine advertised mismatch has already failed the expected
            // spool. It never reaches the observation branch below.
            (source.artifact, source.identity, PinEvidence::Advertised)
        }
        AcquiredRegistryFile::Observed(mut source) => {
            let expected = Digest::Sha256(source.observed_sha256());
            let size = source.size();
            source.rewind().map_err(|_| FreshPinError::Snapshot)?;
            // Revalidate the exact held anonymous observation, never an origin
            // or cache pathname and never an unchecked promotion conversion.
            let verified = spool_verified_reader(
                caller_deadline::Reader::new(&mut source, deadline),
                selected.filename(),
                expected,
                limits.transfer.max_wire_bytes,
                scratch_root,
            )
            .map_err(|_| match caller_deadline::check(deadline) {
                Err(error) => FreshPinError::Source(error),
                Ok(()) => FreshPinError::Snapshot,
            })?;
            caller_deadline::check(deadline).map_err(FreshPinError::Source)?;
            if verified.size() != size {
                return Err(FreshPinError::Snapshot);
            }
            (
                verified,
                source.identity,
                PinEvidence::ObservedDuringFreshResolution,
            )
        }
    };
    if artifact.filename() != selected.filename()
        || artifact.size() > limits.transfer.max_wire_bytes
    {
        return Err(FreshPinError::Snapshot);
    }
    let pin = ContentPin::new(
        slot.file(),
        artifact.digest(),
        artifact.size(),
        evidence,
        lock_limits,
    )
    .map_err(|_| FreshPinError::LockData)?;
    caller_deadline::check(deadline).map_err(FreshPinError::Source)?;
    slot.recheck().map_err(|_| FreshPinError::Revoked)?;
    caller_deadline::check(deadline).map_err(FreshPinError::Source)?;
    Ok(FreshPinnedArtifact {
        slot,
        pin,
        identity,
        artifact,
    })
}

pub(crate) mod wheel;

#[cfg(test)]
#[path = "fresh/tests.rs"]
mod tests;
