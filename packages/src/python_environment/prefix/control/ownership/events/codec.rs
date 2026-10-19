//! Bounded binary scalars and raw-byte paths; no intermediate JSON or DOM.
use super::*;

#[derive(Debug, Default)]
pub(super) struct Budget {
    path_bytes: usize,
    creates: usize,
    finished_bytes: u64,
}
impl Budget {
    fn create(&mut self, limits: OwnershipEventLimits) -> Result<()> {
        self.creates = self
            .creates
            .checked_add(1)
            .filter(|total| {
                *total <= limits.prefix.max_entries && *total <= limits.prefix.max_cleanup_entries
            })
            .ok_or_else(|| eyre::eyre!("ownership entry count limit exceeded"))?;
        Ok(())
    }
    fn path(&mut self, length: usize, limits: OwnershipEventLimits) -> Result<()> {
        self.path_bytes = self
            .path_bytes
            .checked_add(length)
            .filter(|total| *total <= limits.max_total_path_bytes)
            .ok_or_else(|| eyre::eyre!("ownership total path byte limit exceeded"))?;
        Ok(())
    }
    fn finish(&mut self, size: u64, mode: u32, limits: OwnershipEventLimits) -> Result<()> {
        ensure!(mode & !0o777 == 0, "invalid ownership file mode");
        self.finished_bytes = self
            .finished_bytes
            .checked_add(size)
            .filter(|total| *total <= limits.prefix.max_total_bytes)
            .ok_or_else(|| eyre::eyre!("ownership finished byte limit exceeded"))?;
        Ok(())
    }
}
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Cursor<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| eyre::eyre!("ownership event length limit overflow"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| eyre::eyre!("truncated ownership event"))?;
        self.offset = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("fixed u32"),
        ))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("fixed u64"),
        ))
    }
    fn identity(&mut self) -> Result<[u64; 2]> {
        Ok([self.u64()?, self.u64()?])
    }
    fn kind(&mut self) -> Result<OwnershipEntryKind> {
        match self.byte()? {
            1 => Ok(OwnershipEntryKind::Directory),
            2 => Ok(OwnershipEntryKind::File),
            _ => eyre::bail!("invalid ownership entry kind"),
        }
    }
    fn path(
        &mut self,
        budget: &mut Budget,
        limits: OwnershipEventLimits,
        quarantine: bool,
    ) -> Result<Vec<u8>> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| eyre::eyre!("ownership path length limit overflow"))?;
        if quarantine {
            ensure!(
                length == 34,
                "ownership quarantine name length limit exceeded"
            );
        } else {
            path::length(length, limits)?;
        }
        budget.path(length, limits)?;
        let bytes = self.take(length)?;
        if quarantine {
            path::quarantine(bytes)?;
        } else {
            path::validate(bytes, limits)?;
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| eyre::eyre!("ownership path allocation limit failed"))?;
        output.extend_from_slice(bytes);
        Ok(output)
    }
}
pub(super) fn decode(
    bytes: &[u8],
    limits: OwnershipEventLimits,
    budget: &mut Budget,
) -> Result<OwnershipEvent> {
    path::limits(limits)?;
    ensure!(
        bytes.len() <= limits.max_event_bytes,
        "ownership event byte limit exceeded"
    );
    let mut cursor = Cursor { bytes, offset: 0 };
    ensure!(cursor.byte()? == 1, "unsupported ownership event version");
    let tag = cursor.byte()?;
    ensure!((1..=9).contains(&tag), "unknown ownership event tag");
    if tag == 1 {
        budget.create(limits)?;
    }
    let op = cursor.u64()?;
    ensure!(op != 0, "ownership operation must be nonzero");
    let event = match tag {
        1 => OwnershipEvent::CreateIntent {
            op,
            kind: cursor.kind()?,
            relative: cursor.path(budget, limits, false)?,
            parent: cursor.identity()?,
        },
        2 => OwnershipEvent::Created {
            op,
            identity: cursor.identity()?,
        },
        3 => {
            let size = cursor.u64()?;
            let mode = cursor.u32()?;
            budget.finish(size, mode, limits)?;
            OwnershipEvent::FileFinished { op, size, mode }
        }
        4 => OwnershipEvent::CaptureIntent {
            op,
            quarantine: cursor.path(budget, limits, true)?,
            root: cursor.identity()?,
        },
        5 => OwnershipEvent::Captured { op },
        6 => OwnershipEvent::RemoveIntent {
            op,
            kind: cursor.kind()?,
            relative: cursor.path(budget, limits, false)?,
            identity: cursor.identity()?,
        },
        7 => OwnershipEvent::Removed { op },
        8 => OwnershipEvent::RootRemoved { capture_op: op },
        9 => OwnershipEvent::Complete {
            root_removed_sequence: op,
        },
        _ => unreachable!("validated event tag"),
    };
    ensure!(
        cursor.offset == bytes.len(),
        "trailing ownership event bytes"
    );
    Ok(event)
}

pub(super) fn encode(event: &OwnershipEvent, limits: OwnershipEventLimits) -> Result<Vec<u8>> {
    encode_ref(
        OwnershipEventRef::from(event),
        limits,
        &mut Budget::default(),
    )
}

pub(super) fn encode_ref(
    event: OwnershipEventRef<'_>,
    limits: OwnershipEventLimits,
    budget: &mut Budget,
) -> Result<Vec<u8>> {
    path::limits(limits)?;
    let (tag, op, extra) = match event {
        OwnershipEventRef::CreateIntent { op, relative, .. } => {
            budget.create(limits)?;
            budget.path(relative.len(), limits)?;
            path::validate(relative, limits)?;
            (1, op, 21usize.checked_add(relative.len()))
        }
        OwnershipEventRef::Created { op, .. } => (2, op, Some(16)),
        OwnershipEventRef::FileFinished { op, size, mode } => {
            budget.finish(size, mode, limits)?;
            (3, op, Some(12))
        }
        OwnershipEventRef::CaptureIntent { op, quarantine, .. } => {
            path::quarantine(quarantine)?;
            budget.path(quarantine.len(), limits)?;
            (4, op, 20usize.checked_add(quarantine.len()))
        }
        OwnershipEventRef::Captured { op } => (5, op, Some(0)),
        OwnershipEventRef::RemoveIntent { op, relative, .. } => {
            budget.path(relative.len(), limits)?;
            path::validate(relative, limits)?;
            (6, op, 21usize.checked_add(relative.len()))
        }
        OwnershipEventRef::Removed { op } => (7, op, Some(0)),
        OwnershipEventRef::RootRemoved { capture_op } => (8, capture_op, Some(0)),
        OwnershipEventRef::Complete {
            root_removed_sequence,
        } => (9, root_removed_sequence, Some(0)),
    };
    ensure!(op != 0, "ownership operation must be nonzero");
    let length = extra
        .and_then(|extra| extra.checked_add(10))
        .filter(|total| *total <= limits.max_event_bytes)
        .ok_or_else(|| eyre::eyre!("ownership event byte limit exceeded"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| eyre::eyre!("ownership event allocation limit failed"))?;
    bytes.extend_from_slice(&[1, tag]);
    bytes.extend_from_slice(&op.to_be_bytes());
    match event {
        OwnershipEventRef::CreateIntent {
            kind,
            relative,
            parent,
            ..
        } => {
            bytes.push(kind_byte(kind));
            push_path(&mut bytes, relative)?;
            push_identity(&mut bytes, parent);
        }
        OwnershipEventRef::Created { identity, .. } => push_identity(&mut bytes, identity),
        OwnershipEventRef::FileFinished { size, mode, .. } => {
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&mode.to_be_bytes());
        }
        OwnershipEventRef::CaptureIntent {
            quarantine, root, ..
        } => {
            push_path(&mut bytes, quarantine)?;
            push_identity(&mut bytes, root);
        }
        OwnershipEventRef::RemoveIntent {
            kind,
            relative,
            identity,
            ..
        } => {
            bytes.push(kind_byte(kind));
            push_path(&mut bytes, relative)?;
            push_identity(&mut bytes, identity);
        }
        _ => {}
    }
    Ok(bytes)
}
fn kind_byte(kind: OwnershipEntryKind) -> u8 {
    match kind {
        OwnershipEntryKind::Directory => 1,
        OwnershipEntryKind::File => 2,
    }
}
fn push_path(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| eyre::eyre!("ownership path length limit exceeded"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}
fn push_identity(output: &mut Vec<u8>, identity: [u64; 2]) {
    for number in identity {
        output.extend_from_slice(&number.to_be_bytes());
    }
}
