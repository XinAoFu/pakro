//! Actual high-water resource facts from the one original inspection.

use eyre::{Result, ensure, eyre};

use super::WheelLimits;

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct InspectionUsage {
    pub archive_bytes: u64,
    pub directory_bytes: u64,
    pub candidate_work_bytes: u64,
    pub members: u64,
    pub member_bytes: u64,
    pub total_uncompressed: u64,
    pub metadata_bytes: usize,
    pub record_rows: usize,
    pub record_field_bytes: usize,
    pub header_fields: usize,
    pub tag_triples: usize,
}

impl InspectionUsage {
    pub(super) fn check(&self, limits: WheelLimits) -> Result<()> {
        super::validate_limits(limits)?;
        let work = limits
            .max_directory_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(65_557))
            .ok_or_else(|| eyre!("wheel directory work byte limit overflow"))?;
        ensure!(
            self.archive_bytes <= limits.max_archive_bytes,
            "wheel archive byte limit exceeded"
        );
        ensure!(
            self.directory_bytes <= limits.max_directory_bytes,
            "wheel directory byte limit exceeded"
        );
        ensure!(
            self.candidate_work_bytes <= work,
            "wheel directory candidate work byte limit exceeded"
        );
        ensure!(
            self.members <= limits.max_members as u64,
            "wheel member count limit exceeded"
        );
        ensure!(
            self.member_bytes <= limits.max_member_bytes,
            "wheel member byte limit exceeded"
        );
        ensure!(
            self.total_uncompressed <= limits.max_total_uncompressed,
            "wheel expanded byte limit exceeded"
        );
        ensure!(
            self.metadata_bytes <= limits.max_metadata_bytes,
            "wheel metadata byte limit exceeded"
        );
        ensure!(
            self.record_rows <= limits.max_record_rows,
            "wheel RECORD row count limit exceeded"
        );
        ensure!(
            self.record_field_bytes <= limits.max_record_field_bytes,
            "wheel RECORD field byte limit exceeded"
        );
        ensure!(
            self.header_fields <= limits.max_header_fields,
            "wheel header count limit exceeded"
        );
        ensure!(
            self.tag_triples <= limits.max_tag_triples,
            "wheel Tag product limit exceeded"
        );
        Ok(())
    }
}
