//! Pure wheel identity validation and deterministic selection.
//! Explicit supported tags are caller input, not proof of runtime compatibility.

use std::str::FromStr;

use pep440_rs::Version;
use pep508_rs::PackageName;

use crate::wheel::canonical_wheel_tags;
use crate::{
    ArtifactDescriptor, ArtifactKind, IndexError, PackageMetadata, Result, SupportedWheelTags,
    WheelBuildTag, WheelTags, normalize_py_name, parse_wheel_filename,
};

struct Candidate<'a> {
    artifact: &'a ArtifactDescriptor,
    tags: WheelTags,
    build: Option<WheelBuildTag>,
    rank: usize,
}

fn invalid(artifact: &ArtifactDescriptor, reason: &str) -> IndexError {
    IndexError::InvalidWheelArtifact {
        filename: artifact.filename.clone(),
        reason: reason.to_string(),
    }
}

fn ambiguous(
    metadata: &PackageMetadata,
    left: &ArtifactDescriptor,
    right: &ArtifactDescriptor,
) -> IndexError {
    let (first, second) = if left.filename <= right.filename {
        (&left.filename, &right.filename)
    } else {
        (&right.filename, &left.filename)
    };
    IndexError::AmbiguousWheelArtifacts {
        package: normalize_py_name(&metadata.name),
        version: metadata.version.clone(),
        first: first.clone(),
        second: second.clone(),
    }
}

/// Select a wheel for exactly the requested metadata identity.
///
/// This validates every wheel before choosing. It performs no network or
/// filesystem operation and does not establish digest trust or target provenance.
pub fn select_wheel_artifact<'a>(
    metadata: &'a PackageMetadata,
    supported: &SupportedWheelTags,
) -> Result<Option<&'a ArtifactDescriptor>> {
    if metadata.name.is_empty() {
        return Err(IndexError::BadResponse {
            package: metadata.name.clone(),
            reason: "expected distribution name must be nonempty".into(),
        });
    }
    PackageName::from_str(&metadata.name).map_err(|_| IndexError::BadResponse {
        package: metadata.name.clone(),
        reason: "expected distribution name is invalid".into(),
    })?;
    let expected_name = normalize_py_name(&metadata.name);
    let expected_version =
        Version::from_str(&metadata.version).map_err(|_| IndexError::BadResponse {
            package: expected_name.clone(),
            reason: "expected distribution version is not PEP 440".into(),
        })?;
    let mut candidates = Vec::new();
    for artifact in &metadata.artifacts {
        let supplied = match &artifact.kind {
            ArtifactKind::Sdist => {
                if artifact.filename.ends_with(".whl") {
                    return Err(invalid(artifact, "wheel filename is marked as sdist"));
                }
                continue;
            }
            ArtifactKind::Wheel(tags) => tags,
        };
        let parsed = parse_wheel_filename(&artifact.filename)
            .ok_or_else(|| invalid(artifact, "invalid wheel filename"))?;
        if normalize_py_name(&parsed.name) != expected_name {
            return Err(invalid(
                artifact,
                "filename distribution differs from metadata",
            ));
        }
        let version = Version::from_str(&parsed.version)
            .map_err(|_| invalid(artifact, "filename version is not PEP 440"))?;
        if version != expected_version {
            return Err(invalid(artifact, "filename version differs from metadata"));
        }
        let tags = canonical_wheel_tags(supplied)
            .map_err(|_| invalid(artifact, "ArtifactKind contains invalid wheel tags"))?;
        if tags != parsed.tags {
            return Err(invalid(artifact, "ArtifactKind tags differ from filename"));
        }
        if let Some(rank) = supported.best_rank(&tags)? {
            candidates.push(Candidate {
                artifact,
                tags,
                build: parsed.build,
                rank,
            });
        }
    }

    let Some(best_rank) = candidates.iter().map(|candidate| candidate.rank).min() else {
        return Ok(None);
    };
    candidates.retain(|candidate| candidate.rank == best_rank);
    let Some(first) = candidates.first() else {
        return Ok(None);
    };
    for candidate in &candidates {
        if candidate.tags != first.tags {
            return Err(ambiguous(metadata, first.artifact, candidate.artifact));
        }
    }
    let best_build = candidates.iter().map(|candidate| &candidate.build).max();
    let mut selected: Option<&ArtifactDescriptor> = None;
    for candidate in &candidates {
        if Some(&candidate.build) != best_build {
            continue;
        }
        match selected {
            None => selected = Some(candidate.artifact),
            Some(previous) => {
                let next = candidate.artifact;
                if previous.filename != next.filename
                    || previous.url != next.url
                    || previous.digest != next.digest
                {
                    return Err(ambiguous(metadata, previous, next));
                }
            }
        }
    }
    Ok(selected)
}
