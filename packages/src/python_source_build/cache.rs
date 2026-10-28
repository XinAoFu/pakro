//! Expected-output artifact reuse and operation-local sealed completions.
//!
//! Persistent entries retain original ZIP bytes under their actual digest;
//! every lookup verifies them again. A live old lock slot and actual source
//! preparation bind reused output to its fixed record without executing hooks.
//! Duplicates never claim a temporary build environment remains installed.

use super::*;

impl SourceBuildSession {
    fn original_record(
        &self,
        original: &crate::packages::python_resolution::lock_session::ExistingFileSlot<'_>,
    ) -> Result<(
        SourceBuildRecord,
        crate::libraries::package::identity::BuildRequestId,
    )> {
        self.checkpoint()?;
        ensure!(
            self.hook_calls == 0
                && self.initial_environment.is_none()
                && self.last_requirements.is_none(),
            "cached output reuse requires an unexecuted source preparation"
        );
        original.recheck()?;
        let record = original.source_build_record()?.clone();
        ensure!(
            self.source.filename() == original.pin().file().filename()
                && self.source.digest() == original.pin().digest()
                && self.source.size() == original.pin().size(),
            "cached source output preparation differs from its actual locked source slot"
        );
        let observation = record
            .observation
            .as_ref()
            .context("cached source output has no original build observation")?;
        ensure!(
            record.source_digest == self.source.digest().to_string()
                && record.backend == self.extracted.backend
                && observation.initial_requirements == self.extracted.initial_requirements,
            "cached source output differs from its actual original source declarations"
        );
        let request = self.request_id(&record.inputs)?;
        ensure!(
            request.fingerprint() == record.request_id,
            "cached source output request differs from its source, runtime, recipe or fixed build inputs"
        );
        original.recheck()?;
        Ok((record, request))
    }

    /// Validate the fixed receipt against the real source/runtime before a
    /// cold reconstruction executes hooks, using the same gate as ZIP reuse.
    pub(crate) fn check_locked_record(
        &self,
        original: &crate::packages::python_resolution::lock_session::ExistingFileSlot<'_>,
    ) -> Result<()> {
        self.original_record(original)?;
        Ok(())
    }

    /// Bind a fully verified original output to this actual source/runtime.
    /// Static source inputs are parsed again, but no backend hook is executed.
    pub(crate) fn reuse_output(
        self,
        original: &crate::packages::python_resolution::lock_session::ExistingFileSlot<'_>,
        archive: InspectedWheelArchive,
    ) -> Result<CompletedSourceBuild> {
        use pakro_resolver::solve::{Pep440, VersionAdapter};
        let (record, request) = self.original_record(original)?;
        archive.check_limits(WheelLimits::default())?;
        let version =
            Pep440::parse_version(&self.version).context("prepared source version is invalid")?;
        ensure!(
            archive.manifest().name() == self.name
                && Pep440::parse_version(archive.manifest().version()).as_ref() == Some(&version)
                && archive.filename() == record.output_filename
                && archive.digest().to_string() == record.output_digest
                && archive.size() == record.output_size
                && <[u8; 32]>::from(Sha256::digest(archive.manifest().core_metadata().raw()))
                    == record.metadata_sha256,
            "cached source output differs from its complete original artifact receipt"
        );
        let target_fingerprint = observed_target_fingerprint(&self.language)?;
        let language_key = self.language.key().fingerprint();
        let completed = CompletedSourceBuild {
            archive,
            record,
            source: self.source,
            language: self.language,
            request,
            deadline: self.deadline,
            target_fingerprint,
            language_key,
        };
        completed.recheck()?;
        original.recheck()?;
        Ok(completed)
    }
}

impl CompletedSourceBuild {
    /// Cache the original inspected archive, without turning a build request
    /// fingerprint into an output identity or trusting a cache pathname.
    pub(crate) fn publish_output(self, cache_root: &Path, maximum: u64) -> Result<Self> {
        self.recheck()?;
        let Self {
            archive,
            record,
            source,
            language,
            request,
            deadline,
            target_fingerprint,
            language_key,
        } = self;
        let archive =
            archive.publish_output(language.target().supported(), cache_root, deadline, maximum)?;
        let completed = Self {
            archive,
            record,
            source,
            language,
            request,
            deadline,
            target_fingerprint,
            language_key,
        };
        completed.recheck()?;
        Ok(completed)
    }

    /// Produce two independently owned, digest-verified copies of this actual
    /// completion. The caller retains one for this operation and resumes the
    /// original registry continuation with the other.
    pub(crate) fn duplicate_for_attempt(
        self,
        scratch: &Path,
        deadline: Instant,
        max_source_bytes: u64,
        max_output_bytes: u64,
    ) -> Result<(Self, Self)> {
        self.recheck()?;
        let deadline = deadline.min(self.deadline);
        ensure!(
            Instant::now() < deadline,
            "source-build duplication deadline expired"
        );
        ensure!(
            max_source_bytes > 0 && self.source.size() <= max_source_bytes,
            "source-build duplication source byte limit exceeded"
        );
        ensure!(
            max_output_bytes > 0 && self.archive.size() <= max_output_bytes,
            "source-build duplication output byte limit exceeded"
        );
        let Self {
            archive,
            record,
            mut source,
            language,
            request,
            target_fingerprint,
            language_key,
            ..
        } = self;
        let copied_source = crate::artifact_snapshot::clone_verified_artifact(
            &mut source,
            scratch,
            deadline,
            max_source_bytes,
        )?;
        let (archive, copied_archive) = archive.duplicate_for_attempt(
            language.target().supported(),
            WheelLimits::default(),
            scratch,
            deadline,
            max_output_bytes,
        )?;
        let retained = Self {
            archive,
            record: record.clone(),
            source,
            language: language.clone(),
            request,
            deadline,
            target_fingerprint,
            language_key,
        };
        let duplicate = Self {
            archive: copied_archive,
            record,
            source: copied_source,
            language,
            request,
            deadline,
            target_fingerprint,
            language_key,
        };
        retained.recheck_held_output()?;
        duplicate.recheck()?;
        Ok((retained, duplicate))
    }
}
