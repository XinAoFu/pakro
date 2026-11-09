//! Actual wheel metadata remains owned with its fresh pin and source facts.
//!
//! Inspection does not close a dependency graph or authorize an environment
//! write. A pinned sdist remains an explicit build continuation.

use super::*;
use crate::packages::python_resolution::source::wheel_metadata::{self, WheelMetadataError};

pub(crate) mod sidecar;
use pakro_index::SupportedWheelTags;
use pakro_index::python_metadata::{
    PythonDependencyMetadataFacts, PythonMetadataError, PythonMetadataLimits,
};
use pakro_index::simple::SimpleFile;

use crate::packages::python_wheel::{WheelLimits, WheelManifest};
use crate::packages::python_wheel_source::InspectedWheelArchive;

pub(crate) type FreshWheelResult<T> = Result<T, FreshWheelError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum FreshWheelError {
    #[error("fresh metadata limits must be positive")]
    InvalidLimits,
    #[error("fresh metadata slot is no longer current")]
    Revoked,
    #[error("fresh wheel source identity is inconsistent")]
    IdentityMismatch,
    #[error("fresh wheel archive inspection failed: {0}")]
    Archive(String),
    #[error("fresh wheel dependency metadata failed: {0}")]
    Metadata(PythonMetadataError),
}

pub(crate) struct FreshWheelCandidate<'lock> {
    slot: FreshFileSlot<'lock>,
    pin: ContentPin,
    identity: RegistryFileIdentity,
    archive: InspectedWheelArchive,
    facts: PythonDependencyMetadataFacts,
}
impl FreshWheelCandidate<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        &self.pin
    }
    pub(crate) fn manifest(&self) -> &WheelManifest {
        self.archive.manifest()
    }
    pub(crate) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(crate) fn file_facts(&self) -> &SimpleFile {
        &self.identity.project.project.files()[self.identity.file_index]
    }
    pub(crate) fn source_key(&self) -> SourceObservationKey {
        self.identity.source_key
    }
    pub(crate) fn recheck(&self) -> FreshWheelResult<()> {
        self.slot.recheck().map_err(|_| FreshWheelError::Revoked)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> FreshWheelResult<()> {
        self.slot
            .recheck_for(owner)
            .map_err(|_| FreshWheelError::Revoked)
    }
}
impl fmt::Debug for FreshWheelCandidate<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("FreshWheelCandidate")
            .field("pin", &self.pin)
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub(crate) enum FreshMetadataState<'lock> {
    Wheel(FreshWheelCandidate<'lock>),
    /// The same legitimately pinned source; no build worker was authorized.
    NeedsBuild(FreshPinnedArtifact<'lock>),
}

fn observation_error(error: WheelMetadataError) -> FreshWheelError {
    match error {
        WheelMetadataError::InvalidLimits => FreshWheelError::InvalidLimits,
        WheelMetadataError::IdentityMismatch => FreshWheelError::IdentityMismatch,
        WheelMetadataError::Archive(error) => FreshWheelError::Archive(error),
        WheelMetadataError::Metadata(error) => FreshWheelError::Metadata(error),
    }
}

fn check_identity(source: &FreshPinnedArtifact<'_>) -> FreshWheelResult<()> {
    let identity = &source.identity;
    let project = &identity.project;
    let file = &project.project.files()[identity.file_index];
    let pin = source.pin.file();
    let version_matches = file.version().is_some_and(|version| {
        match (
            Pep440::parse_version(version),
            Pep440::parse_version(pin.version()),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    });
    let kind_matches = matches!(
        (file.kind(), pin.kind()),
        (
            PythonDistributionKind::Wheel(_),
            RegistryArtifactKind::Wheel
        ) | (PythonDistributionKind::Sdist, RegistryArtifactKind::Sdist)
    );
    if project.registry.id().as_str() != pin.registry_id()
        || &canonical_registry_base_fingerprint(project.registry.canonical_base())
            != pin.canonical_base_fingerprint()
        || project.project.name().as_str() != pin.project()
        || file.filename() != pin.filename()
        || !version_matches
        || !kind_matches
        || source.artifact.filename() != pin.filename()
        || source.artifact.digest() != source.pin.digest()
        || source.artifact.size() != source.pin.size()
    {
        return Err(FreshWheelError::IdentityMismatch);
    }
    Ok(())
}

pub(crate) fn route_fresh_metadata<'lock>(
    source: FreshPinnedArtifact<'lock>,
    supported: &SupportedWheelTags,
    wheel_limits: WheelLimits,
    metadata_limits: PythonMetadataLimits,
) -> FreshWheelResult<FreshMetadataState<'lock>> {
    wheel_metadata::validate_limits(wheel_limits, metadata_limits).map_err(observation_error)?;
    source.recheck().map_err(|_| FreshWheelError::Revoked)?;
    check_identity(&source)?;
    if source.pin.file().kind() == RegistryArtifactKind::Sdist {
        // Preserve the exact source owner for H4. A build continuation is not
        // final metadata, an empty dependency graph or execution permission.
        return Ok(FreshMetadataState::NeedsBuild(source));
    }
    let FreshPinnedArtifact {
        slot,
        pin,
        identity,
        artifact,
    } = source;
    let (archive, facts) = wheel_metadata::inspect_projected_wheel(
        artifact,
        &pin,
        supported,
        wheel_limits,
        metadata_limits,
    )
    .map_err(observation_error)?;
    slot.recheck().map_err(|_| FreshWheelError::Revoked)?;
    Ok(FreshMetadataState::Wheel(FreshWheelCandidate {
        slot,
        pin,
        identity,
        archive,
        facts,
    }))
}

#[cfg(test)]
#[path = "wheel/tests.rs"]
mod tests;
