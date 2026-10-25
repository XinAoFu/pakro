//! One held-wheel archive and dependency projection, without source authority.

use crate::artifact_snapshot::VerifiedArtifact;
use crate::packages::python_resolution::lock_model::{ContentPin, RegistryArtifactKind};
use crate::packages::python_wheel::{WheelExpectation, WheelLimits, WheelManifest};
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};
use pakro_index::SupportedWheelTags;
use pakro_index::python_metadata::{
    PythonDependencyMetadataFacts, PythonMetadataError, PythonMetadataLimits,
};
use pakro_index::python_source::NormalizedPythonName;
use thiserror::Error;

type Result<T> = std::result::Result<T, WheelMetadataError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum WheelMetadataError {
    #[error("wheel metadata limits must be positive")]
    InvalidLimits,
    #[error("wheel metadata byte identity is inconsistent")]
    IdentityMismatch,
    #[error("wheel archive inspection failed")]
    Archive,
    #[error("wheel dependency metadata failed: {0}")]
    Metadata(PythonMetadataError),
}

pub(super) fn validate_limits(wheel: WheelLimits, metadata: PythonMetadataLimits) -> Result<()> {
    crate::packages::python_wheel::validate_limits(wheel)
        .map_err(|_| WheelMetadataError::InvalidLimits)?;
    let PythonMetadataLimits {
        max_total_bytes,
        max_field_bytes,
        max_requirements,
        max_provided_extras,
        max_requested_extras,
        max_dynamic_fields,
        max_marker_depth,
        max_marker_terms,
        max_syntax_tokens,
    } = metadata;
    if [
        max_total_bytes,
        max_field_bytes,
        max_requirements,
        max_provided_extras,
        max_requested_extras,
        max_dynamic_fields,
        max_marker_depth,
        max_marker_terms,
        max_syntax_tokens,
    ]
    .contains(&0)
    {
        return Err(WheelMetadataError::InvalidLimits);
    }
    Ok(())
}

/// Callers retain their genuine source/lock proof. This private kernel only
/// consumes an already expected-verified byte owner and its explicit identity.
pub(super) fn inspect_projected_wheel(
    artifact: VerifiedArtifact,
    pin: &ContentPin,
    supported: &SupportedWheelTags,
    wheel_limits: WheelLimits,
    metadata_limits: PythonMetadataLimits,
) -> Result<(InspectedWheelArchive, PythonDependencyMetadataFacts)> {
    validate_limits(wheel_limits, metadata_limits)?;
    if pin.file().kind() != RegistryArtifactKind::Wheel
        || artifact.filename() != pin.file().filename()
        || artifact.digest() != pin.digest()
        || artifact.size() != pin.size()
    {
        return Err(WheelMetadataError::IdentityMismatch);
    }
    let expected = WheelExpectation {
        filename: pin.file().filename(),
        name: pin.file().project(),
        version: pin.file().version(),
        supported,
    };
    let archive = inspect_owned_wheel(artifact, pin.digest(), &expected, wheel_limits)
        .map_err(|_| WheelMetadataError::Archive)?;
    let facts = project_manifest_metadata(
        archive.manifest(),
        pin.file().project(),
        pin.file().version(),
        metadata_limits,
    )?;
    Ok((archive, facts))
}

/// The actual output wheel uses the same dependency projection without
/// pretending its generated filename/digest came from a registry wheel pin.
pub(crate) fn project_manifest_metadata(
    manifest: &WheelManifest,
    expected_project: &str,
    expected_version: &str,
    metadata_limits: PythonMetadataLimits,
) -> Result<PythonDependencyMetadataFacts> {
    let metadata = manifest.core_metadata();
    let name = match metadata.values("Name") {
        [value] => value.as_str(),
        _ => return Err(WheelMetadataError::Archive),
    };
    let version = match metadata.values("Version") {
        [value] => value.as_str(),
        _ => return Err(WheelMetadataError::Archive),
    };
    let requires_python = match metadata.values("Requires-Python") {
        [] => None,
        [value] => Some(value.as_str()),
        _ => return Err(WheelMetadataError::Archive),
    };
    // The expected normalized name also obeys the current projection budget
    // before constructing its owned wrapper from an older, wider lock input.
    if expected_project.len() > metadata_limits.max_field_bytes
        || expected_version.len() > metadata_limits.max_field_bytes
    {
        return Err(WheelMetadataError::Metadata(
            PythonMetadataError::LimitExceeded(
                pakro_index::python_metadata::PythonMetadataLimitKind::FieldBytes,
            ),
        ));
    }
    let expected_name = NormalizedPythonName::new(expected_project)
        .map_err(|_| WheelMetadataError::IdentityMismatch)?;
    let facts = pakro_index::python_metadata::project_python_metadata(
        pakro_index::python_metadata::PythonMetadataFields {
            metadata_version: metadata.metadata_version(),
            name,
            version,
            requires_python,
            requires_dist: metadata.values("Requires-Dist"),
            provides_extra: metadata.values("Provides-Extra"),
            dynamic: metadata.values("Dynamic"),
        },
        pakro_index::python_metadata::PythonMetadataExpectation {
            name: &expected_name,
            version: expected_version,
        },
        metadata_limits,
    )
    .map_err(WheelMetadataError::Metadata)?;
    Ok(facts)
}
