//! Whole-log validation with only bounded per-record payload allocations.

use super::*;

pub(super) fn read(
    control: &StoreControl<'_>,
    journal: &JournalRecord,
    journal_limits: JournalLimits,
    requested: OwnershipStreamLimits,
) -> Result<Option<OwnershipLog>> {
    let limits = binding::limits(control, requested)?;
    let header = binding::header(control, journal, journal_limits)?;
    let parent = &control.namespaces[ControlNamespace::Ownership.index()];
    let name = filename(header.nonce);
    control.validate_name(&name)?;
    let mut file = match openat(
        parent,
        name.as_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    control.validate_file(&file)?;
    ensure_entry_identity(parent, name.as_str(), FileIdentity::of(&file)?)?;
    let before = file.metadata()?;
    let (records, total) = pakro_store::control::ownership::read_frames(
        &mut file,
        &framing::header_bytes(&header),
        header.sha256,
        before.len(),
        limits,
    )?
    .into_parts();
    ensure!(
        binding::same_metadata(&before, &file.metadata()?),
        "ownership stream changed during read"
    );
    control.check_binding()?;
    control.validate_file(&file)?;
    ensure_entry_identity(parent, name.as_str(), FileIdentity::of(&file)?)?;
    Ok(Some(OwnershipLog {
        header,
        records,
        total_bytes: total,
        read_pin: file,
        observed: before,
    }))
}
