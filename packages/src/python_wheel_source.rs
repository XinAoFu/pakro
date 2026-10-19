//! Owned, source-bound wheel inspection and bounded member operations.

use std::fmt;
use std::io::Write;

use eyre::{ContextCompat, Result, ensure};
use pakro_store::Digest;

use crate::artifact_snapshot::VerifiedArtifact;

use super::python_wheel::{
    WheelDirectoryReceipt, WheelExpectation, WheelLimits, WheelManifest, WheelMember,
    inspect_with_receipt,
};

pub(crate) struct InspectedWheelArchive {
    filename: String,
    digest: Digest,
    size: u64,
    manifest: WheelManifest,
    receipt: WheelDirectoryReceipt<VerifiedArtifact>,
}
impl InspectedWheelArchive {
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
    pub(crate) fn digest(&self) -> Digest {
        self.digest
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn actual_member_count(&self) -> u64 {
        self.receipt.actual_member_count()
    }
    pub(crate) fn actual_uncompressed_bytes(&self) -> u64 {
        self.receipt.actual_uncompressed_bytes()
    }
    pub(crate) fn manifest(&self) -> &WheelManifest {
        &self.manifest
    }

    /// Compare a caller's complete inspection budget against the actual usage
    /// retained from this archive's original inspection, without reopening it.
    pub(crate) fn check_limits(&self, limits: WheelLimits) -> Result<()> {
        self.receipt.check_limits(limits)
    }

    pub(crate) fn with_members<T>(
        &mut self,
        inspect: impl FnOnce(&mut WheelMemberSession<'_>) -> Result<T>,
    ) -> Result<T> {
        // The receipt owns the one checked archive. Repeated callbacks reuse it;
        // no source pathname is reopened and no directory is reinterpreted.
        let mut session = WheelMemberSession {
            manifest: &self.manifest,
            receipt: &mut self.receipt,
        };
        inspect(&mut session)
    }
}
impl fmt::Debug for InspectedWheelArchive {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("InspectedWheelArchive")
            .field("filename", &self.filename())
            .field("digest", &self.digest())
            .field("size", &self.size())
            .field("name", &self.manifest.name())
            .field("version", &self.manifest.version())
            .finish_non_exhaustive()
    }
}

pub(crate) struct WheelMemberSession<'a> {
    manifest: &'a WheelManifest,
    receipt: &'a mut WheelDirectoryReceipt<VerifiedArtifact>,
}
fn member(manifest: &WheelManifest, archive_index: usize) -> Result<&WheelMember> {
    // A preserves increasing original ZIP indexes while omitting directories.
    // Binary lookup keeps repeated batch reads O(log n), without index aliases.
    manifest
        .members()
        .binary_search_by_key(&archive_index, WheelMember::archive_index)
        .ok()
        .map(|position| &manifest.members()[position])
        .context("wheel member index is not a regular file in the inspection inventory")
}
impl WheelMemberSession<'_> {
    pub(crate) fn read_member(&mut self, archive_index: usize, max_bytes: u64) -> Result<Vec<u8>> {
        self.receipt
            .read_member(member(self.manifest, archive_index)?, max_bytes)
    }
    pub(crate) fn copy_member(
        &mut self,
        archive_index: usize,
        output: &mut dyn Write,
    ) -> Result<WheelMemberReadReceipt> {
        let (size, sha256) = self
            .receipt
            .copy_member(member(self.manifest, archive_index)?, output)?;
        Ok(WheelMemberReadReceipt {
            archive_index,
            size,
            sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelMemberReadReceipt {
    archive_index: usize,
    size: u64,
    sha256: [u8; 32],
}
impl WheelMemberReadReceipt {
    pub(crate) fn archive_index(&self) -> usize {
        self.archive_index
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

pub(crate) fn inspect_owned_wheel(
    artifact: VerifiedArtifact,
    expected_digest: Digest,
    expected: &WheelExpectation<'_>,
    limits: WheelLimits,
) -> Result<InspectedWheelArchive> {
    ensure!(
        artifact.filename() == expected.filename,
        "wheel artifact filename disagrees with selected source"
    );
    ensure!(
        artifact.digest() == expected_digest,
        "wheel artifact digest disagrees with selected source"
    );
    ensure!(
        artifact.size() <= limits.max_archive_bytes,
        "wheel artifact byte limit exceeded"
    );
    let filename = artifact.filename().to_owned();
    let digest = artifact.digest();
    let size = artifact.size();
    let (manifest, receipt) = inspect_with_receipt(artifact, expected, limits)?;
    Ok(InspectedWheelArchive {
        filename,
        digest,
        size,
        manifest,
        receipt,
    })
}

#[cfg(test)]
mod tests;
