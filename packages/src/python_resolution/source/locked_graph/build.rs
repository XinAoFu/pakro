//! A source replay keeps the old source pin, build inputs and output receipt.
use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;
use crate::packages::python_source_build::CompletedSourceBuild;
#[cfg(test)]
use crate::packages::python_wheel::{WheelExpectation, WheelLimits};
#[cfg(test)]
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};

pub(super) fn expected<'a>(
    root: &'a BoundPythonLock,
    source: &ContentPin,
) -> Result<&'a SourceBuildRecord, LockedGraphError> {
    root.recheck().map_err(|_| LockedGraphError::Revoked)?;
    let record = root
        .source_build(source.id())
        .ok_or(LockedGraphError::MissingBuild)?;
    record
        .validate()
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    if source.file().kind()
        != crate::packages::python_resolution::lock_model::RegistryArtifactKind::Sdist
        || record.source_digest != source.digest().to_string()
    {
        return Err(LockedGraphError::BuildMismatch);
    }
    Ok(record)
}

#[cfg(test)]
pub(super) fn inspect_output(
    artifact: VerifiedArtifact,
    source: &ContentPin,
    record: &SourceBuildRecord,
    supported: &pakro_index::SupportedWheelTags,
    wheel_limits: WheelLimits,
) -> Result<InspectedWheelArchive, LockedGraphError> {
    let digest = record
        .output_digest
        .parse()
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    if artifact.filename() != record.output_filename
        || artifact.digest() != digest
        || artifact.size() != record.output_size
    {
        return Err(LockedGraphError::BuildMismatch);
    }
    let expectation = WheelExpectation {
        filename: &record.output_filename,
        name: source.file().project(),
        version: source.file().version(),
        supported,
    };
    let archive = inspect_owned_wheel(artifact, digest, &expectation, wheel_limits)
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    let metadata: [u8; 32] = sha2::Sha256::digest(archive.manifest().core_metadata().raw()).into();
    if metadata != record.metadata_sha256 {
        return Err(LockedGraphError::BuildMismatch);
    }
    Ok(archive)
}

impl<'lock, 'config> LockedGraphBuildContinuation<'lock, 'config> {
    pub(crate) async fn resume_with_published(
        mut self,
        package: crate::libraries::package::lease::PackageLease,
        language: crate::libraries::language::lease::LanguageLease,
    ) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
        self.expected_record()?;
        let source_id = self.source.pin().id();
        let owner =
            super::super::registry_depot::ReplayedPublishedRegistryWheel::from_locked_published(
                self.source,
                package,
                language,
                self.replay.root,
                &self.replay.target,
                self.replay.context.limits.wheel,
                self.replay.context.limits.metadata,
            )
            .map_err(|_| LockedGraphError::BuildMismatch)?;
        let key = self
            .replay
            .nodes
            .iter()
            .find(|node| node.pin.id() == source_id)
            .map(|node| node.key)
            .ok_or(LockedGraphError::GraphMismatch)?;
        let owner = RegistryMetadataOwner::Published(owner);
        prepare::retain_metadata(
            &mut self.replay.retained_utf8,
            &owner,
            self.replay.context.limits.max_retained_utf8_bytes,
        )?;
        if self.replay.owners.insert(key, owner).is_some() {
            return Err(LockedGraphError::GraphMismatch);
        }
        prepare::run(self.replay, || {}).await
    }

    pub(crate) fn expected_record(&self) -> Result<&SourceBuildRecord, LockedGraphError> {
        expected(self.replay.root, self.source.pin())
    }
    pub(crate) fn source_verified_copy(
        &mut self,
        scratch: &Path,
    ) -> Result<VerifiedArtifact, LockedGraphError> {
        self.expected_record()?;
        let pin = self.source.pin().clone();
        self.replay.retained_archive_bytes = self
            .replay
            .retained_archive_bytes
            .checked_add(pin.size())
            .filter(|total| *total <= self.replay.context.limits.max_retained_archive_bytes)
            .ok_or(LockedGraphError::Limit)?;
        copy_source(
            &mut self.source,
            scratch,
            self.replay.context.deadline,
            self.replay.context.limits.lock.max_artifact_bytes,
        )
    }
    pub(crate) async fn resume_with_built(
        mut self,
        completed: CompletedSourceBuild,
    ) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
        self.source
            .recheck_for(self.replay.root)
            .map_err(|_| LockedGraphError::Revoked)?;
        let expected = self.expected_record()?;
        if completed.target_fingerprint() != *self.replay.target.fingerprint()
            || completed
                .record()
                .fingerprint()
                .map_err(|_| LockedGraphError::BuildMismatch)?
                != expected
                    .fingerprint()
                    .map_err(|_| LockedGraphError::BuildMismatch)?
        {
            return Err(LockedGraphError::BuildMismatch);
        }
        let (archive, record) = completed
            .into_parts()
            .map_err(|error| {
                #[cfg(test)]
                eprintln!("P6_LOCKED_RESUME_COMPLETED_CAUSE {error:#}");
                let _ = error;
                LockedGraphError::BuildMismatch
            })?;
        let key = self
            .replay
            .nodes
            .iter()
            .find(|node| node.pin.id() == self.source.pin().id())
            .map(|node| node.key)
            .ok_or(LockedGraphError::GraphMismatch)?;
        let owner = super::super::registry_depot::ReplayedBuiltRegistryWheel::from_locked_output(
            self.source,
            archive,
            record,
            self.replay.context.limits.metadata,
        )
        .map_err(|error| {
            #[cfg(test)]
            eprintln!("P6_LOCKED_RESUME_OUTPUT_CAUSE {error:#}");
            let _ = error;
            LockedGraphError::BuildMismatch
        })?;
        let owner = RegistryMetadataOwner::Built(owner);
        prepare::retain_metadata(
            &mut self.replay.retained_utf8,
            &owner,
            self.replay.context.limits.max_retained_utf8_bytes,
        )?;
        if self.replay.owners.insert(key, owner).is_some() {
            return Err(LockedGraphError::GraphMismatch);
        }
        prepare::run(self.replay, || {}).await
    }
}

pub(super) fn copy_source(
    source: &mut LockedRegistryArtifact<'_>,
    scratch: &Path,
    deadline: Instant,
    max_bytes: u64,
) -> Result<VerifiedArtifact, LockedGraphError> {
    source.recheck().map_err(|_| LockedGraphError::Revoked)?;
    if Instant::now() >= deadline {
        return Err(LockedGraphError::Deadline);
    }
    let pin = source.pin().clone();
    source
        .rewind()
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    let copy = spool_verified_reader(
        super::super::caller_deadline::Reader::new(&mut *source, Some(deadline)),
        pin.file().filename(),
        pin.digest(),
        max_bytes,
        scratch,
    )
    .map_err(|_| LockedGraphError::BuildMismatch)?;
    if Instant::now() >= deadline {
        return Err(LockedGraphError::Deadline);
    }
    if copy.size() != pin.size() {
        return Err(LockedGraphError::BuildMismatch);
    }
    source.recheck().map_err(|_| LockedGraphError::Revoked)?;
    Ok(copy)
}
