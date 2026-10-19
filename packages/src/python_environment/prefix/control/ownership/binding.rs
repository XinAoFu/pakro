//! Independent inode pins. The caller must still present the actual ready prefix.

use super::*;
use crate::packages::python_environment::journal::read_journal_metadata;

pub(super) fn limits(
    control: &StoreControl<'_>,
    requested: OwnershipStreamLimits,
) -> Result<OwnershipStreamLimits> {
    ensure!(
        requested.max_payload_bytes != 0
            && requested.max_records != 0
            && requested.max_total_bytes != 0,
        "ownership stream limits must be nonzero"
    );
    let limits = OwnershipStreamLimits {
        max_total_bytes: requested.max_total_bytes.min(control.limits.max_file_bytes),
        ..requested
    };
    ensure!(
        limits.max_total_bytes >= HEADER_BYTES,
        "ownership stream header byte limit exceeded"
    );
    Ok(limits)
}

pub(super) fn header(
    control: &StoreControl<'_>,
    journal: &JournalRecord,
    limits: JournalLimits,
) -> Result<Header> {
    control.check_binding()?;
    let generation = journal
        .generation()
        .ok_or_else(|| eyre::eyre!("clear selection has no ownership stream"))?;
    let records = read_journal_metadata(control, limits)?;
    ensure!(
        records.iter().any(|record| record == journal),
        "ownership stream requires the exact durable journal record"
    );
    let name = format!("{}.json", journal.nonce().simple());
    let maximum = u64::try_from(limits.max_document_bytes)
        .map_err(|_| eyre::eyre!("journal byte limit is not representable"))?;
    let snapshot = control
        .read_bounded(ControlNamespace::Journals, &name, maximum)?
        .ok_or_else(|| eyre::eyre!("ownership stream durable journal disappeared"))?;
    let mut header = Header {
        nonce: journal.nonce(),
        journal_sha256: *snapshot.sha256(),
        identity: generation.identity().clone(),
        sha256: [0; 32],
    };
    header.sha256 = framing::header_hash(&header);
    Ok(header)
}

pub(super) fn owned_regular(file: &File, uid: u32) -> Result<()> {
    validate_owned_regular(file, uid)?;
    ensure!(
        file.metadata()?.mode() & 0o7777 == 0o600,
        "ownership stream permissions changed"
    );
    Ok(())
}

impl BoundFiles {
    pub(super) fn check(
        &self,
        prefix: &UnpublishedPrefix,
        header: &Header,
        total: u64,
        observed: bool,
    ) -> Result<()> {
        prefix.check_write_ready()?;
        ensure!(
            prefix.allocation_id() == header.identity.allocation
                && FileIdentity::of(&prefix.binding.store_root)? == FileIdentity::of(&self.store)?
                && pair(&prefix.binding.root)? == header.identity.generation
                && pair(&prefix.binding._build_lease)? == header.identity.lock,
            "ownership stream requires the same ready prefix identity"
        );
        ensure!(
            self.identity
                .as_ref()
                .ok_or_else(|| eyre::eyre!("cleanup-only stream has no ready generation identity"))?
                .facts()?
                == header.identity,
            "ownership stream generation identity changed"
        );
        self.check_io(header, total, observed)
    }

    pub(super) fn check_io(&self, header: &Header, total: u64, observed: bool) -> Result<()> {
        for directory in [&self.store, &self.control, &self.parent] {
            validate_owned_directory(directory, self.uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "ownership stream namespace permissions changed"
            );
        }
        ensure_entry_identity(&self.store, ".control", FileIdentity::of(&self.control)?)?;
        ensure_entry_identity(&self.control, "ownership", FileIdentity::of(&self.parent)?)?;
        ensure_entry_identity(
            &self.parent,
            filename(header.nonce).as_str(),
            FileIdentity::of(&self.file)?,
        )?;
        owned_regular(&self.file, self.uid)?;
        let metadata = self.file.metadata()?;
        ensure!(metadata.len() == total, "ownership stream length changed");
        if observed {
            ensure!(
                same_metadata(&self.observed, &metadata),
                "ownership stream content observation changed"
            );
        }
        Ok(())
    }
}

fn pair(file: &File) -> Result<[u64; 2]> {
    let metadata = file.metadata()?;
    Ok([metadata.dev(), metadata.ino()])
}

pub(super) fn same_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
