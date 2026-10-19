//! Metadata observations owned with a genuine existing-file project-lock slot.
//!
//! Warm expected cache bytes need no fabricated Simple project or locator.
//! A source archive remains an explicit build continuation.

use super::*;
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::source::wheel_metadata::{self, WheelMetadataError};
use crate::packages::python_wheel::{WheelLimits, WheelManifest};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use pakro_index::SupportedWheelTags;
use pakro_index::python_metadata::{
    PythonDependencyMetadataFacts, PythonMetadataError, PythonMetadataLimits,
};

pub(crate) type LockedMetadataResult<T> = Result<T, LockedMetadataError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum LockedMetadataError {
    #[error("locked metadata limits must be positive")]
    InvalidLimits,
    #[error("locked metadata slot is no longer current for this project")]
    Revoked,
    #[error("locked wheel byte identity is inconsistent")]
    IdentityMismatch,
    #[error("locked wheel archive inspection failed")]
    Archive,
    #[error("locked wheel dependency metadata failed: {0}")]
    Metadata(PythonMetadataError),
}

pub(crate) struct LockedWheelCandidate<'lock> {
    slot: ExistingFileSlot<'lock>,
    provenance: ReplayProvenance,
    archive: InspectedWheelArchive,
    facts: PythonDependencyMetadataFacts,
}
impl LockedWheelCandidate<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        self.slot.pin()
    }
    pub(crate) fn origin(&self) -> LockedReplayOrigin {
        match self.provenance {
            ReplayProvenance::Cache => LockedReplayOrigin::Cache,
            ReplayProvenance::Registry(_) => LockedReplayOrigin::Registry,
        }
    }
    pub(crate) fn source_key(&self) -> Option<SourceObservationKey> {
        match &self.provenance {
            ReplayProvenance::Cache => None,
            ReplayProvenance::Registry(identity) => Some(identity.source_key),
        }
    }
    pub(crate) fn file_facts(&self) -> Option<&SimpleFile> {
        match &self.provenance {
            ReplayProvenance::Cache => None,
            ReplayProvenance::Registry(identity) => {
                Some(&identity.project.project.files()[identity.file_index])
            }
        }
    }
    pub(crate) fn manifest(&self) -> &WheelManifest {
        self.archive.manifest()
    }
    pub(crate) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(crate) fn recheck(&self) -> LockedMetadataResult<()> {
        self.slot
            .recheck()
            .map_err(|_| LockedMetadataError::Revoked)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> LockedMetadataResult<()> {
        self.slot
            .recheck_for(owner)
            .map_err(|_| LockedMetadataError::Revoked)
    }
}
impl fmt::Debug for LockedWheelCandidate<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("LockedWheelCandidate")
            .field("pin", &self.pin())
            .field("origin", &self.origin())
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub(crate) enum LockedMetadataState<'lock> {
    Wheel(LockedWheelCandidate<'lock>),
    NeedsBuild(LockedRegistryArtifact<'lock>),
}

fn observation_error(error: WheelMetadataError) -> LockedMetadataError {
    match error {
        WheelMetadataError::InvalidLimits => LockedMetadataError::InvalidLimits,
        WheelMetadataError::IdentityMismatch => LockedMetadataError::IdentityMismatch,
        WheelMetadataError::Archive => LockedMetadataError::Archive,
        WheelMetadataError::Metadata(error) => LockedMetadataError::Metadata(error),
    }
}

fn check_provenance(source: &LockedRegistryArtifact<'_>) -> LockedMetadataResult<()> {
    let pin = source.pin();
    if source.artifact.filename() != pin.file().filename()
        || source.artifact.digest() != pin.digest()
        || source.artifact.size() != pin.size()
    {
        return Err(LockedMetadataError::IdentityMismatch);
    }
    if let ReplayProvenance::Registry(identity) = &source.provenance {
        let project = &identity.project;
        let file = &project.project.files()[identity.file_index];
        let expected = pin.file();
        let version_matches = match (
            Pep440::parse_version(expected.version()),
            file.version().and_then(Pep440::parse_version),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        let kind_matches = matches!(
            (expected.kind(), file.kind()),
            (
                RegistryArtifactKind::Wheel,
                PythonDistributionKind::Wheel(_)
            ) | (RegistryArtifactKind::Sdist, PythonDistributionKind::Sdist)
        );
        if !registry_matches(expected, &project.registry)
            || expected.project() != project.project.name().as_str()
            || expected.filename() != file.filename()
            || !version_matches
            || !kind_matches
        {
            return Err(LockedMetadataError::IdentityMismatch);
        }
    }
    Ok(())
}

pub(crate) fn route_locked_metadata<'lock>(
    source: LockedRegistryArtifact<'lock>,
    supported: &SupportedWheelTags,
    wheel_limits: WheelLimits,
    metadata_limits: PythonMetadataLimits,
) -> LockedMetadataResult<LockedMetadataState<'lock>> {
    wheel_metadata::validate_limits(wheel_limits, metadata_limits).map_err(observation_error)?;
    source.recheck().map_err(|_| LockedMetadataError::Revoked)?;
    check_provenance(&source)?;
    if source.pin().file().kind() == RegistryArtifactKind::Sdist {
        return Ok(LockedMetadataState::NeedsBuild(source));
    }
    let LockedRegistryArtifact {
        slot,
        artifact,
        provenance,
    } = source;
    let (archive, facts) = wheel_metadata::inspect_projected_wheel(
        artifact,
        slot.pin(),
        supported,
        wheel_limits,
        metadata_limits,
    )
    .map_err(observation_error)?;
    slot.recheck().map_err(|_| LockedMetadataError::Revoked)?;
    Ok(LockedMetadataState::Wheel(LockedWheelCandidate {
        slot,
        provenance,
        archive,
        facts,
    }))
}

#[cfg(test)]
#[path = "metadata/tests.rs"]
mod tests;

pub(in crate::packages::python_resolution::source) struct RetainedLockedWheel<'lock> {
    slot: ExistingFileSlot<'lock>,
    provenance: ReplayProvenance,
    facts: PythonDependencyMetadataFacts,
}
impl<'lock> LockedWheelCandidate<'lock> {
    pub(in crate::packages::python_resolution::source) fn into_build_evidence(
        self,
    ) -> (InspectedWheelArchive, RetainedLockedWheel<'lock>) {
        let LockedWheelCandidate {
            slot,
            provenance,
            archive,
            facts,
        } = self;
        (
            archive,
            RetainedLockedWheel {
                slot,
                provenance,
                facts,
            },
        )
    }
}
impl RetainedLockedWheel<'_> {
    pub(in crate::packages::python_resolution::source) fn pin(&self) -> &ContentPin {
        self.slot.pin()
    }
    pub(in crate::packages::python_resolution::source) fn facts(
        &self,
    ) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(in crate::packages::python_resolution::source) fn recheck_for(
        &self,
        root: &BoundPythonLock,
    ) -> LockedMetadataResult<()> {
        self.slot
            .recheck_for(root)
            .map_err(|_| LockedMetadataError::Revoked)
    }
}
