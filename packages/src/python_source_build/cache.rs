//! Operation-local reuse of one actual, sealed source-build completion.
//!
//! A duplicate carries the historical request and inspected output. It does
//! not assert that any temporary build-input environment remains installed.

use super::*;

impl CompletedSourceBuild {
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
        retained.recheck()?;
        duplicate.recheck()?;
        Ok((retained, duplicate))
    }
}
