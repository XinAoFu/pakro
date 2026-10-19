//! Private continuation for an actually authenticated cleanup session.

use super::super::super::observer::cleanup::CleanupWitness;
use super::*;

impl StoreControl<'_> {
    pub(in super::super::super) fn recovery_store_anchor(&self) -> Result<(File, PathBuf, u32)> {
        self.check_binding()?;
        Ok((
            self.store.root.try_clone()?,
            self.store.root_path.clone(),
            self.store.expected_uid,
        ))
    }

    pub(in super::super::super) fn resume_cleanup_stream(
        &self,
        journal: &JournalRecord,
        journal_limits: JournalLimits,
        log: &OwnershipLog,
        requested: OwnershipStreamLimits,
        witness: &CleanupWitness<'_, '_, '_>,
    ) -> Result<(OwnershipStream, ControlLimits)> {
        let limits = binding::limits(self, requested)?;
        let header = binding::header(self, journal, journal_limits)?;
        ensure!(
            header == log.header,
            "cleanup resume requires the exact freshly observed header"
        );
        ensure!(
            log.total_bytes >= HEADER_BYTES && log.total_bytes <= limits.max_total_bytes,
            "cleanup resume stream byte limit exceeded"
        );
        let count = u64::try_from(log.records.len())
            .map_err(|_| eyre::eyre!("cleanup resume record count is not representable"))?;
        ensure!(
            count <= limits.max_records,
            "cleanup resume record limit exceeded"
        );
        for record in &log.records {
            ensure!(
                u64::try_from(record.payload().len())? <= limits.max_payload_bytes,
                "cleanup resume payload limit exceeded"
            );
        }
        let next_sequence = count
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("cleanup resume sequence overflow"))?;
        witness.check_stream(header.nonce, &header.identity, &self.store.root)?;
        self.validate_file(&log.read_pin)?;
        ensure!(
            binding::same_metadata(&log.observed, &log.read_pin.metadata()?),
            "cleanup resume read observation changed"
        );
        let name = filename(header.nonce);
        self.validate_name(&name)?;
        let parent = &self.namespaces[ControlNamespace::Ownership.index()];
        let file = File::from(openat(
            parent,
            name.as_str(),
            OFlag::O_WRONLY
                | OFlag::O_APPEND
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        self.validate_file(&file)?;
        binding::owned_regular(&file, self.store.expected_uid)?;
        ensure!(
            FileIdentity::of(&file)? == FileIdentity::of(&log.read_pin)?
                && binding::same_metadata(&log.observed, &file.metadata()?),
            "cleanup resume file differs from its pinned read observation"
        );
        ensure_entry_identity(parent, name.as_str(), FileIdentity::of(&file)?)?;
        let observed = file.metadata()?;
        let files = BoundFiles {
            store: self.store.root.try_clone()?,
            control: self.root.try_clone()?,
            parent: parent.try_clone()?,
            file,
            identity: None,
            uid: self.store.expected_uid,
            observed,
        };
        files.check_io(&header, log.total_bytes, true)?;
        witness.check_stream(header.nonce, &header.identity, &files.store)?;
        self.check_binding()?;
        let stream = OwnershipStream {
            binding: Some(files),
            header,
            limits,
            next_sequence,
            head: *log.head(),
            total_bytes: log.total_bytes,
            poisoned: false,
        };
        Ok((stream, self.limits))
    }
}
