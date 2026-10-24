//! An opaque inspected archive owns its reader and bounded Zip8 metadata.

use std::fmt;
use std::io::{Read, Seek, Write};

use eyre::{Result, ensure, eyre};
use sha2::{Digest as _, Sha256};

use super::usage::InspectionUsage;
use super::zip_directory::ValidatedZipView;
use super::{WheelLimits, WheelMember};

/// This receipt retains the exact archive used by inspection. It provides no
/// raw reader, path or writable source capability and cannot be cloned.
pub(crate) struct WheelDirectoryReceipt<R: Read + Seek> {
    archive: zip::ZipArchive<ValidatedZipView<R>>,
    limits: WheelLimits,
    usage: InspectionUsage,
}
impl<R: Read + Seek> WheelDirectoryReceipt<R> {
    pub(super) fn new(
        archive: zip::ZipArchive<ValidatedZipView<R>>,
        limits: WheelLimits,
        usage: InspectionUsage,
    ) -> Self {
        Self {
            archive,
            limits,
            usage,
        }
    }

    pub(crate) fn check_limits(&self, limits: WheelLimits) -> Result<()> {
        self.usage.check(limits)
    }

    pub(crate) fn actual_member_count(&self) -> u64 {
        self.usage.members
    }

    pub(crate) fn actual_uncompressed_bytes(&self) -> u64 {
        self.usage.total_uncompressed
    }

    pub(crate) fn read_member(&mut self, member: &WheelMember, max_bytes: u64) -> Result<Vec<u8>> {
        ensure!(max_bytes != 0, "wheel member read limit must be nonzero");
        let limit = max_bytes.min(self.limits.max_metadata_bytes as u64);
        ensure!(
            member.size() <= limit,
            "wheel member read byte limit exceeded"
        );
        let capacity = usize::try_from(member.size())
            .map_err(|_| eyre!("wheel member allocation limit exceeded"))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| eyre!("wheel member bounded allocation failed"))?;
        self.copy_verified(member, &mut output, Some(limit))?;
        Ok(output)
    }

    pub(crate) fn copy_member(
        &mut self,
        member: &WheelMember,
        output: &mut dyn Write,
    ) -> Result<(u64, [u8; 32])> {
        self.copy_verified(member, output, None)
    }

    fn copy_verified(
        &mut self,
        member: &WheelMember,
        output: &mut dyn Write,
        read_limit: Option<u64>,
    ) -> Result<(u64, [u8; 32])> {
        let mut source = self
            .archive
            .by_index(member.archive_index())
            .map_err(|_| eyre!("ZIP wheel member cannot be opened"))?;
        ensure!(
            source.name_raw() == member.archive_path().as_bytes() && source.size() == member.size(),
            "wheel member does not match inspected directory"
        );
        let mut sha256 = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|_| eyre!("ZIP wheel member read or CRC failed"))?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .ok_or_else(|| eyre!("wheel member byte limit overflow"))?;
            ensure!(
                size <= self.limits.max_member_bytes
                    && read_limit.is_none_or(|limit| size <= limit),
                "wheel member byte limit exceeded"
            );
            ensure!(
                size <= member.size(),
                "wheel member size changed from inspection"
            );
            sha256.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|_| eyre!("wheel member write failed"))?;
        }
        ensure!(
            size == member.size(),
            "wheel member size changed from inspection"
        );
        let sha256: [u8; 32] = sha256.finalize().into();
        ensure!(
            &sha256 == member.observed_sha256(),
            "wheel member SHA256 changed from inspection"
        );
        Ok((size, sha256))
    }
}
impl<R: Read + Seek> fmt::Debug for WheelDirectoryReceipt<R> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("WheelDirectoryReceipt")
            .field("members", &self.archive.len())
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WheelDirectoryReceipt<crate::artifact_snapshot::VerifiedArtifact> {
    /// Keep the raw source confined to the wheel implementation: both results
    /// are re-inspected owners, never mutable paths or an unchecked reader.
    pub(crate) fn duplicate_inspected_wheel(
        self,
        expected: &super::WheelExpectation<'_>,
        limits: super::WheelLimits,
        scratch_root: &std::path::Path,
        deadline: std::time::Instant,
        max_archive_bytes: u64,
    ) -> Result<(
        crate::packages::python_wheel_source::InspectedWheelArchive,
        crate::packages::python_wheel_source::InspectedWheelArchive,
    )> {
        use crate::artifact_snapshot::clone_verified_artifact;
        use crate::packages::python_wheel_source::inspect_owned_wheel;

        self.usage.check(limits)?;
        // The validated ZIP view masks an EOCD comment for Zip8. Copy its
        // original source, preserving every comment/footer byte in the digest.
        let mut original = self.archive.into_inner().into_input();
        let digest = original.digest();
        let copy =
            clone_verified_artifact(&mut original, scratch_root, deadline, max_archive_bytes)?;
        let original = inspect_owned_wheel(original, digest, expected, limits)?;
        ensure!(
            std::time::Instant::now() < deadline,
            "inspected wheel duplication deadline expired"
        );
        let copy = inspect_owned_wheel(copy, digest, expected, limits)?;
        ensure!(
            std::time::Instant::now() < deadline,
            "inspected wheel duplication deadline expired"
        );
        Ok((original, copy))
    }
}
