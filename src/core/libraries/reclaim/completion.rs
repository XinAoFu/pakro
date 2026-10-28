//! Small terminal evidence for ordinary completed-operation projections.
//! This receipt grants no deletion/recovery authority. Explicit resume still
//! reads the original immutable manifests and checks the physical identities.
use super::journal::{Cursor, Header, Session, cursor_name, header_name, state_name};
use super::{PendingReclaim, ReclaimId, uuid_serde};
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::{ControlLimits, ControlNamespace, ControlSnapshot, ReadControl};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

const MAX_OBJECTS: usize = 1024;
const MAX_RECEIPT_BYTES: u64 = 1024 * 1024;
const MAX_CURSOR_BYTES: u64 = 16 * 1024;

pub(super) fn name(operation: Uuid) -> String {
    format!("reclaim-complete-{operation}.json")
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn identity(pin: &ControlSnapshot) -> [u64; 2] {
    let value = pin.file_identity();
    [value.device as u64, value.inode as u64]
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    schema: u32,
    #[serde(with = "uuid_serde")]
    operation: Uuid,
    central: [u64; 2],
    header_sha256: [u8; 32],
    cursors: Vec<TerminalCursor>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalCursor {
    object: ReclaimId,
    name: String,
    identity: [u64; 2],
    sha256: [u8; 32],
    entries: usize,
    payload: bool,
}

fn terminal(cursor: &Cursor, entries: usize, payload: bool) -> Result<()> {
    let limits =
        crate::packages::python_environment::prefix::verify::PrefixInventoryLimits::default();
    ensure!(
        entries <= limits.max_files.saturating_add(limits.max_directories),
        "completed reclaim inventory count exceeds capacity"
    );
    cursor.validate(entries, payload)?;
    ensure!(
        cursor.allocation_removed && cursor.next == entries && !cursor.pending,
        "reclaim completion does not describe a terminal physical deletion"
    );
    Ok(())
}

/// Stream each large original record before entering the final catalog scope.
/// Only small immutable terminal descriptions are retained across iterations.
pub(super) fn publish(session: &Session) -> Result<()> {
    ensure!(
        session.fully_cleaned(),
        "cannot publish reclamation completion before all cleanup gates"
    );
    ensure!(
        session.header.records.len() <= MAX_OBJECTS,
        "completed reclamation object capacity exceeded"
    );
    session.writer.check_binding()?;
    let mut cursors = Vec::with_capacity(session.header.records.len());
    for (index, link) in session.header.records.iter().enumerate() {
        let record = session.record(index)?;
        ensure!(
            record.object == link.object,
            "completed record object differs from original header"
        );
        let (cursor, pin) = session.cursor(index)?;
        let entries = record.inventory.entries().len();
        let payload = record.payload.is_some();
        terminal(&cursor, entries, payload)?;
        ensure!(
            payload == matches!(record.object, ReclaimId::Language(_)),
            "completed payload kind differs from object identity"
        );
        cursors.push(TerminalCursor {
            object: record.object,
            name: cursor_name(session.header.operation, index),
            identity: identity(&pin),
            sha256: *pin.sha256(),
            entries,
            payload,
        });
    }
    let mut control = session.writer.control(ControlLimits::default())?;
    let header = control
        .read(
            ControlNamespace::Journals,
            &header_name(session.header.operation),
        )?
        .context("completed reclamation header disappeared")?;
    let decoded: Header = serde_json::from_slice(header.bytes())?;
    ensure!(
        serde_json::to_vec(&decoded)? == serde_json::to_vec(&session.header)?,
        "completed reclamation header changed before receipt publication"
    );
    let central = control.store_identity()?;
    ensure!(
        session.header.central == [central.0, central.1],
        "completed reclamation central store changed"
    );
    // The original per-record scopes are gone. Rechecking small cursors through
    // this one existing control cannot recursively acquire the same catalog.
    for proof in &cursors {
        let pin = control
            .read_bounded(ControlNamespace::Journals, &proof.name, MAX_CURSOR_BYTES)?
            .context("completed reclamation cursor disappeared before receipt")?;
        ensure!(
            identity(&pin) == proof.identity && *pin.sha256() == proof.sha256,
            "completed reclamation cursor changed before receipt"
        );
        let cursor: Cursor = serde_json::from_slice(pin.bytes())?;
        terminal(&cursor, proof.entries, proof.payload)?;
    }
    let receipt = Completion {
        schema: 1,
        operation: session.header.operation,
        central: session.header.central,
        header_sha256: *header.sha256(),
        cursors,
    };
    let bytes = serde_json::to_vec(&receipt)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT_BYTES,
        "reclamation completion receipt capacity exceeded"
    );
    let filename = name(receipt.operation);
    if let Some(existing) =
        control.read_bounded(ControlNamespace::Journals, &filename, MAX_RECEIPT_BYTES)?
    {
        ensure!(
            existing.bytes() == bytes,
            "reclamation completion receipt conflicts with original terminal evidence"
        );
    } else {
        control.replace(ControlNamespace::Journals, &filename, None, &bytes)?;
    }
    session.writer.check_binding()?;
    Ok(())
}

/// The caller must first validate all four terminal State gates. This path
/// checks only bounded receipt/cursor bytes, never archived large Records.
pub(super) fn project(
    control: &ReadControl<'_>,
    header: &Header,
    header_bytes: &[u8],
) -> Result<Option<PendingReclaim>> {
    let Some(pin) = control.read_bounded(
        ControlNamespace::Journals,
        &name(header.operation),
        MAX_RECEIPT_BYTES,
    )?
    else {
        return Ok(None);
    };
    let receipt: Completion = serde_json::from_slice(pin.bytes())?;
    let central = control.store_identity()?;
    ensure!(
        header.schema == 1
            && receipt.schema == 1
            && receipt.operation == header.operation
            && !receipt.operation.is_nil()
            && receipt.central == header.central
            && receipt.central == [central.0, central.1]
            && receipt.header_sha256 == hash(header_bytes),
        "reclamation completion receipt identity or header digest differs"
    );
    let decoded: Header = serde_json::from_slice(header_bytes)?;
    ensure!(
        serde_json::to_vec(&decoded)? == serde_json::to_vec(header)?,
        "reclamation completion header description differs from original bytes"
    );
    ensure!(
        header.records.len() <= MAX_OBJECTS && receipt.cursors.len() == header.records.len(),
        "reclamation completion fixed object set differs"
    );
    let expected: Vec<_> = header.groups.iter().flatten().copied().collect();
    ensure!(
        expected.len() == header.records.len()
            && expected.iter().copied().collect::<BTreeSet<_>>().len() == expected.len(),
        "reclamation completion header repeats or omits an original object"
    );
    let mut journals = vec![
        ("projects".into(), header_name(header.operation)),
        ("projects".into(), state_name(header.operation)),
        ("projects".into(), name(header.operation)),
    ];
    for (index, (link, proof)) in header.records.iter().zip(&receipt.cursors).enumerate() {
        ensure!(
            link.object == expected[index]
                && proof.object == link.object
                && proof.name == cursor_name(header.operation, index)
                && proof.payload == matches!(link.object, ReclaimId::Language(_)),
            "reclamation completion member identity or cursor name differs"
        );
        let cursor_pin = control
            .read_bounded(ControlNamespace::Journals, &proof.name, MAX_CURSOR_BYTES)?
            .context("reclamation completion terminal cursor disappeared")?;
        ensure!(
            identity(&cursor_pin) == proof.identity && *cursor_pin.sha256() == proof.sha256,
            "reclamation completion terminal cursor identity or digest changed"
        );
        let cursor: Cursor = serde_json::from_slice(cursor_pin.bytes())?;
        terminal(&cursor, proof.entries, proof.payload)?;
        journals.push(("projects".into(), link.name.clone()));
        journals.push(("projects".into(), proof.name.clone()));
    }
    control.check_binding()?;
    Ok(Some(PendingReclaim {
        operation_id: header.operation,
        objects: expected,
        members: Vec::new(),
        directories: Vec::new(),
        journals,
        complete: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn p4_completion_requires_actual_terminal_cursor_shape() {
        let mut cursor = Cursor::default();
        assert!(terminal(&cursor, 1, false).is_err());
        cursor.revoke_pending = true;
        cursor.revoked = true;
        cursor.capture_pending = true;
        cursor.captured = true;
        cursor.next = 1;
        cursor.allocation_pending = true;
        cursor.allocation_removed = true;
        terminal(&cursor, 1, false).unwrap();
        assert!(terminal(&cursor, 2, false).is_err());
        assert!(terminal(&cursor, 1, true).is_err());
        cursor.payload_pending = true;
        cursor.payload_removed = true;
        terminal(&cursor, 1, true).unwrap();
        cursor.pending = true;
        assert!(terminal(&cursor, 1, true).is_err());
    }

    #[test]
    fn p4_completed_projection_uses_small_receipt_and_binds_original_cursor() {
        use super::super::journal::Link;
        use pakro_store::control::{ReadStore, StoreWriter, lease::StableLock};
        use std::time::{Duration, Instant};
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap().join("projects");
        let deadline = Instant::now() + Duration::from_secs(30);
        let uid = nix::unistd::geteuid().as_raw();
        let writer = StoreWriter::open_writer(&root, uid, Default::default(), deadline).unwrap();
        let operation = Uuid::from_u128(101);
        let object = ReclaimId::Package([7; 32]);
        let lock = StableLock::create_new(
            writer.build_handle(),
            &format!("reclaim-{operation}.lock"),
            uid,
        )
        .unwrap();
        let mut cursor = Cursor::default();
        cursor.revoke_pending = true;
        cursor.revoked = true;
        cursor.capture_pending = true;
        cursor.captured = true;
        cursor.next = 3;
        cursor.allocation_pending = true;
        cursor.allocation_removed = true;
        let (header, header_bytes, receipt) = {
            let mut control = writer.control(Default::default()).unwrap();
            // Deliberately not a Record: this unit tests only the small read
            // projection, and must never parse or grant authority from it.
            let archived = vec![b'x'; 32 * 1024];
            let record_name = format!("reclaim-object-{operation}-0.json");
            let record = control
                .replace(ControlNamespace::Journals, &record_name, None, &archived)
                .unwrap();
            let cursor_name = cursor_name(operation, 0);
            let pin = control
                .replace(
                    ControlNamespace::Journals,
                    &cursor_name,
                    None,
                    &serde_json::to_vec(&cursor).unwrap(),
                )
                .unwrap();
            let store = control.store_identity().unwrap();
            let lock = lock.identity();
            let header = Header {
                schema: 1,
                operation,
                central: [store.0, store.1],
                lock: [lock.device as u64, lock.inode as u64],
                mode: super::super::ReclaimMode::Prune,
                groups: vec![vec![object]],
                records: vec![Link {
                    object,
                    name: record_name,
                    sha256: *record.sha256(),
                    identity: identity(&record),
                }],
            };
            let header_bytes = serde_json::to_vec(&header).unwrap();
            control
                .replace(
                    ControlNamespace::Journals,
                    &header_name(operation),
                    None,
                    &header_bytes,
                )
                .unwrap();
            let receipt = Completion {
                schema: 1,
                operation,
                central: header.central,
                header_sha256: hash(&header_bytes),
                cursors: vec![TerminalCursor {
                    object,
                    name: cursor_name,
                    identity: identity(&pin),
                    sha256: *pin.sha256(),
                    entries: 3,
                    payload: false,
                }],
            };
            (header, header_bytes, receipt)
        };
        let read_project = |header_bytes: &[u8]| {
            let read = ReadStore::open_readonly(&root, uid, Default::default(), deadline)
                .unwrap()
                .unwrap();
            let control = read.control(Default::default()).unwrap();
            project(&control, &header, header_bytes)
        };
        assert!(read_project(&header_bytes).unwrap().is_none());
        writer
            .control(Default::default())
            .unwrap()
            .replace(
                ControlNamespace::Journals,
                &name(operation),
                None,
                &serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
        let projected = read_project(&header_bytes).unwrap().unwrap();
        assert!(projected.complete);
        assert_eq!(projected.objects, vec![object]);
        assert!(projected.members.is_empty() && projected.directories.is_empty());
        assert!(
            projected
                .journals
                .contains(&("projects".into(), header.records[0].name.clone()))
        );
        let mut changed_header = header_bytes.clone();
        changed_header.push(b' ');
        assert!(read_project(&changed_header).is_err());
        let mut control = writer.control(Default::default()).unwrap();
        let filename = cursor_name(operation, 0);
        let original = control
            .read(ControlNamespace::Journals, &filename)
            .unwrap()
            .unwrap();
        control
            .replace(
                ControlNamespace::Journals,
                &filename,
                Some(&original),
                &serde_json::to_vec(&cursor).unwrap(),
            )
            .unwrap();
        drop(control);
        assert!(
            read_project(&header_bytes).is_err(),
            "even equal cursor bytes on a replacement inode must be rejected"
        );
    }
}
