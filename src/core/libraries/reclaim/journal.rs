use super::object::{self, Record};
use super::*;
use eyre::{ContextCompat, ensure};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, ControlSnapshot, ReadStore, StoreWriter,
};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, sync::Arc};
const PREFIX: &str = "reclaim-operation-";
pub(super) fn header_name(id: Uuid) -> String {
    format!("{PREFIX}{id}.json")
}
pub(super) fn state_name(id: Uuid) -> String {
    format!("reclaim-state-{id}.json")
}
pub(super) fn cursor_name(id: Uuid, index: usize) -> String {
    format!("reclaim-cursor-{id}-{index}.json")
}
fn record_name(id: Uuid, index: usize) -> String {
    format!("reclaim-object-{id}-{index}.json")
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    pub revoke_pending: bool,
    pub revoked: bool,
    pub capture_pending: bool,
    pub captured: bool,
    pub next: usize,
    pub pending: bool,
    pub payload_pending: bool,
    pub payload_removed: bool,
    pub allocation_pending: bool,
    pub allocation_removed: bool,
}
impl Cursor {
    pub(super) fn validate(&self, count: usize, payload: bool) -> Result<()> {
        ensure!(
            self.next <= count && (!self.pending || self.next < count),
            "invalid reclaim entry cursor"
        );
        ensure!(
            !self.revoked || self.revoke_pending,
            "reclaim revocation has no intent"
        );
        ensure!(
            !self.capture_pending || self.revoked,
            "reclaim capture precedes revocation"
        );
        ensure!(
            !self.captured || self.capture_pending,
            "reclaim capture has no intent"
        );
        ensure!(
            (self.next == 0 && !self.pending) || self.captured,
            "reclaim entry progress precedes capture"
        );
        ensure!(
            !self.payload_pending
                || (payload && self.captured && self.next == count && !self.pending),
            "reclaim payload progress precedes entries"
        );
        ensure!(
            !self.payload_removed || self.payload_pending,
            "reclaim payload removal has no intent"
        );
        ensure!(
            !self.allocation_pending
                || (self.captured
                    && self.next == count
                    && !self.pending
                    && (!payload || self.payload_removed)),
            "reclaim allocation progress precedes its contents"
        );
        ensure!(
            !self.allocation_removed || self.allocation_pending,
            "reclaim allocation removal has no intent"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Link {
    pub object: ReclaimId,
    pub name: String,
    pub sha256: [u8; 32],
    pub identity: [u64; 2],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    pub schema: u32,
    #[serde(with = "super::uuid_serde")]
    pub operation: Uuid,
    pub central: [u64; 2],
    pub lock: [u64; 2],
    pub mode: ReclaimMode,
    pub groups: Vec<Vec<ReclaimId>>,
    pub records: Vec<Link>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    complete: bool,
    claims_cleaned: bool,
    claims_ready: bool,
    retired_cleaned: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    schema: u32,
    #[serde(with = "super::uuid_serde")]
    operation: Uuid,
    object: ReclaimId,
    central: [u64; 2],
    record: [u8; 32],
}
pub(super) struct Session {
    pub writer: Arc<StoreWriter>,
    lock: StableLock,
    held: ObjectLease,
    pub header: Header,
    state: State,
    state_pin: ControlSnapshot,
}
#[derive(Debug)]
pub(super) struct CreationInterrupted {
    pub operation: Uuid,
    pub error: eyre::Report,
}
impl std::fmt::Display for CreationInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "reclaim {} initialization interrupted: {:#}",
            self.operation, self.error
        )
    }
}
impl std::error::Error for CreationInterrupted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}
#[derive(Debug)]
pub(super) struct OperationBusy(pub Uuid);
impl std::fmt::Display for OperationBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reclaim operation {} is already running", self.0)
    }
}
impl std::error::Error for OperationBusy {}
fn decode_header(bytes: &[u8], operation: Uuid, central: [u64; 2]) -> Result<Header> {
    let value: Header = serde_json::from_slice(bytes)?;
    ensure!(
        value.schema == 1
            && !operation.is_nil()
            && value.operation == operation
            && value.central == central,
        "reclaim operation journal identity changed"
    );
    ensure!(
        value.records.len() <= 1024 && value.groups.len() <= 1024,
        "reclaim operation capacity exceeded"
    );
    let flat = value.groups.iter().flatten().copied().collect::<Vec<_>>();
    let unique = flat.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        unique.len() == flat.len()
            && flat.len() == value.records.len()
            && value
                .groups
                .iter()
                .all(|g| !g.is_empty() && g.len() <= MAX_GROUP_OBJECTS),
        "reclaim operation fixed groups changed"
    );
    for (index, link) in value.records.iter().enumerate() {
        ensure!(
            link.object == flat[index] && link.name == record_name(operation, index),
            "reclaim immutable record identity changed"
        );
        object::identity(link.identity)?;
    }
    object::identity(value.lock)?;
    Ok(value)
}
impl Session {
    pub(super) fn create(
        context: &OperationContext,
        writer: Arc<StoreWriter>,
        mode: ReclaimMode,
        groups: &[Vec<ReclaimId>],
        records: &[Record],
    ) -> Result<Self> {
        let operation = Uuid::now_v7();
        let state = State::default();
        let mut payloads = Vec::new();
        let mut total = 0usize;
        for (index, record) in records.iter().enumerate() {
            let bytes = serde_json::to_vec(record)?;
            total = total
                .checked_add(bytes.len())
                .filter(|n| *n <= 64 * 1024 * 1024)
                .context("reclaim immutable record capacity exceeded")?;
            payloads.push((record_name(operation, index), bytes));
            payloads.push((
                cursor_name(operation, index),
                serde_json::to_vec(&Cursor::default())?,
            ));
        }
        payloads.push((state_name(operation), serde_json::to_vec(&state)?));
        ensure!(
            payloads.iter().map(|(_, b)| b.len()).sum::<usize>() <= 64 * 1024 * 1024,
            "reclaim initialization capacity exceeded"
        );
        let lock = StableLock::create_new(
            writer.build_handle(),
            &format!("reclaim-{operation}.lock"),
            object::uid(),
        )?;
        let held = lock
            .try_acquire(LeaseMode::Exclusive, context.policy().deadline)?
            .context("new reclaim operation is busy")?;
        let id = lock.identity();
        let lock_identity = [id.device as _, id.inode as _];
        let setup = (|| {
            let preparing = super::preparing::Preparing::create(
                &writer,
                operation,
                lock_identity,
                mode,
                groups,
                &payloads,
            )?;
            cutpoint("preparing");
            let mut control = writer.control(Default::default())?;
            let mut links = Vec::new();
            let mut state_pin = None;
            for (name, bytes) in &payloads {
                let pin = control.replace(ControlNamespace::Journals, name, None, bytes)?;
                if let Some(index) = records.iter().enumerate().find_map(|(index, _)| {
                    (name == &record_name(operation, index)).then_some(index)
                }) {
                    links.push(Link {
                        object: records[index].object,
                        name: name.clone(),
                        sha256: hash(bytes),
                        identity: object::pair_snapshot(&pin),
                    });
                    cutpoint("payload-written");
                } else if name == &state_name(operation) {
                    state_pin = Some(pin);
                    cutpoint("state-written");
                } else {
                    cutpoint("cursor-written");
                }
            }
            let header = Header {
                schema: 1,
                operation,
                central: object::pair(writer.root_handle())?,
                lock: lock_identity,
                mode,
                groups: groups.to_vec(),
                records: links,
            };
            control.replace(
                ControlNamespace::Journals,
                &header_name(operation),
                None,
                &serde_json::to_vec(&header)?,
            )?;
            drop(control);
            cutpoint("header-prepared");
            preparing.finish(&writer)?;
            Ok::<_, eyre::Report>((
                header,
                state_pin.context("reclaim initial state was not persisted")?,
            ))
        })();
        let (header, state_pin) =
            setup.map_err(|error| CreationInterrupted { operation, error })?;
        let value = Self {
            writer,
            lock,
            held,
            header,
            state,
            state_pin,
        };
        value
            .check(context)
            .map_err(|error| CreationInterrupted { operation, error })?;
        cutpoint("prepared");
        Ok(value)
    }
    pub(super) fn open(
        context: &OperationContext,
        operation: Uuid,
        writer: Arc<StoreWriter>,
    ) -> Result<Self> {
        let repository =
            crate::libraries::project::repository::ProjectRepository::open_readonly(context)?;
        drop(repository);
        let path = context.requested_data_root().join("projects");
        let read = ReadStore::open_readonly(
            &path,
            object::uid(),
            Default::default(),
            context.policy().deadline,
        )?
        .context("reclaim operation store is missing")?;
        let control = read.control(Default::default())?;
        let central = control.store_identity()?;
        let pin = control
            .read(ControlNamespace::Journals, &header_name(operation))?
            .context("reclaim operation journal is missing")?;
        let header = decode_header(pin.bytes(), operation, [central.0, central.1])?;
        drop(control);
        drop(read);
        let lock = StableLock::open_existing(
            writer.build_handle(),
            &format!("reclaim-{operation}.lock"),
            object::uid(),
            object::identity(header.lock)?,
        )?;
        let held = lock
            .try_acquire(LeaseMode::Exclusive, context.policy().deadline)?
            .ok_or(OperationBusy(operation))?;
        let control = writer.control(Default::default())?;
        let current = control
            .read(ControlNamespace::Journals, &header_name(operation))?
            .context("reclaim operation journal disappeared")?;
        ensure!(
            current.bytes() == pin.bytes() && current.file_identity() == pin.file_identity(),
            "reclaim operation journal changed"
        );
        sync_namespace(
            context,
            &control,
            ControlNamespace::Journals,
            "progress-parent",
            None,
        )?;
        let state_pin = control
            .read(ControlNamespace::Journals, &state_name(operation))?
            .context("reclaim operation state is missing")?;
        let state = serde_json::from_slice(state_pin.bytes())?;
        drop(control);
        let value = Self {
            writer,
            lock,
            held,
            header,
            state,
            state_pin,
        };
        value.check(context)?;
        value.validate_progress()?;
        Ok(value)
    }
    pub(super) fn check(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.writer.check_binding()?;
        self.held.check_binding()?;
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("reclaim-{}.lock", self.header.operation),
        )?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive
                && object::pair(self.writer.root_handle())? == self.header.central,
            "reclaim operation protection changed"
        );
        Ok(())
    }
    pub(super) fn record(&self, index: usize) -> Result<Record> {
        let link = self
            .header
            .records
            .get(index)
            .context("reclaim record index exceeds original set")?;
        let control = self.writer.control(Default::default())?;
        let pin = control
            .read(ControlNamespace::Journals, &link.name)?
            .context("reclaim immutable record disappeared")?;
        decode_record(&self.header, link, &pin)
    }
    pub(super) fn cursor(&self, index: usize) -> Result<(Cursor, ControlSnapshot)> {
        let control = self.writer.control(Default::default())?;
        let pin = control
            .read(
                ControlNamespace::Journals,
                &cursor_name(self.header.operation, index),
            )?
            .context("reclaim progress disappeared")?;
        let cursor = serde_json::from_slice(pin.bytes())?;
        Ok((cursor, pin))
    }
    pub(super) fn save_cursor(
        &self,
        index: usize,
        cursor: &Cursor,
        pin: &mut ControlSnapshot,
    ) -> Result<()> {
        let mut control = self.writer.control(Default::default())?;
        *pin = control.replace(
            ControlNamespace::Journals,
            &cursor_name(self.header.operation, index),
            Some(pin),
            &serde_json::to_vec(cursor)?,
        )?;
        Ok(())
    }
    pub(super) fn complete(&self) -> bool {
        self.state.complete
    }
    pub(super) fn fully_cleaned(&self) -> bool {
        self.state.complete
            && self.state.claims_cleaned
            && self.state.claims_ready
            && self.state.retired_cleaned
    }
    fn validate_progress(&self) -> Result<()> {
        ensure!(
            !self.state.claims_cleaned || self.state.complete,
            "reclaim claim cleanup precedes completion"
        );
        ensure!(
            !self.state.complete || self.state.retired_cleaned,
            "reclaim completion precedes retirement cleanup"
        );
        ensure!(
            !self.state.retired_cleaned || self.state.claims_ready,
            "reclaim retirement precedes claims"
        );
        if self.state.complete || self.state.retired_cleaned {
            for index in 0..self.header.records.len() {
                let record = self.record(index)?;
                let (cursor, _) = self.cursor(index)?;
                cursor.validate(record.inventory.entries().len(), record.payload.is_some())?;
                ensure!(
                    cursor.allocation_removed,
                    "reclaim completion precedes physical deletion"
                );
            }
        }
        Ok(())
    }
    pub(super) fn claims_ready(&self) -> bool {
        self.state.claims_ready
    }
    pub(super) fn mark_claims_ready(&mut self) -> Result<()> {
        self.state.claims_ready = true;
        self.save_state()?;
        cutpoint("claims-ready");
        Ok(())
    }
    pub(super) fn mark_retired_cleaned(&mut self) -> Result<()> {
        self.state.retired_cleaned = true;
        self.validate_progress()?;
        self.save_state()?;
        cutpoint("retired-cleaned");
        Ok(())
    }
    pub(super) fn mark_complete(&mut self) -> Result<()> {
        self.state.complete = true;
        self.validate_progress()?;
        self.save_state()?;
        cutpoint("complete");
        Ok(())
    }
    fn save_state(&mut self) -> Result<()> {
        let mut control = self.writer.control(Default::default())?;
        self.state_pin = control.replace(
            ControlNamespace::Journals,
            &state_name(self.header.operation),
            Some(&self.state_pin),
            &serde_json::to_vec(&self.state)?,
        )?;
        Ok(())
    }
    pub(super) fn claim(&self, index: usize, owner: &object::OwnedObject) -> Result<()> {
        let link = &self.header.records[index];
        let mut control = owner.store.writer()?.control(Default::default())?;
        let name = object::claim_name(owner.record.object);
        if let Some(pin) = control.read(ControlNamespace::Journals, &name)? {
            self.validate_claim(index, pin.bytes())?;
        } else {
            ensure!(
                !self.state.claims_ready,
                "pending deletion claim disappeared"
            );
            let claim = Claim {
                schema: 1,
                operation: self.header.operation,
                object: owner.record.object,
                central: self.header.central,
                record: link.sha256,
            };
            control.replace(
                ControlNamespace::Journals,
                &name,
                None,
                &serde_json::to_vec(&claim)?,
            )?;
        }
        cutpoint("claim");
        Ok(())
    }
    fn validate_claim(&self, index: usize, bytes: &[u8]) -> Result<()> {
        let claim: Claim = serde_json::from_slice(bytes)?;
        let link = &self.header.records[index];
        ensure!(
            claim.schema == 1
                && claim.operation == self.header.operation
                && claim.object == link.object
                && claim.central == self.header.central
                && claim.record == link.sha256,
            "object belongs to another or damaged pending deletion"
        );
        Ok(())
    }
    pub(super) fn clean_claims(
        &mut self,
        context: &OperationContext,
        stores: &mut object::Stores,
    ) -> Result<()> {
        ensure!(
            self.complete(),
            "cannot release deletion claims before completion"
        );
        self.validate_progress()?;
        for index in 0..self.header.records.len() {
            let record = self.record(index)?;
            let store = object::store(context, stores, record.object, false)?;
            ensure!(
                object::pair(&store.root)? == record.store,
                "completed reclaim object library was replaced"
            );
            ensure!(
                object::stat_identity(&store.root, &record.allocation)?
                    != Some(record.allocation_identity)
                    && object::stat_identity(&store.root, &record.captured)?.is_none(),
                "completed reclaim still has an original or captured allocation"
            );
        }
        if self.state.claims_cleaned {
            super::completion::publish(self)?;
            return Ok(());
        }
        for index in 0..self.header.records.len() {
            let record = self.record(index)?;
            let id = record.object;
            let store = object::store(context, stores, id, true)?;
            ensure!(
                object::pair(&store.root)? == record.store,
                "completed reclaim object library was replaced"
            );
            let mut control = store.writer()?.control(Default::default())?;
            if let Some(pin) = control.read(ControlNamespace::Journals, &object::claim_name(id))? {
                let claim: Claim = serde_json::from_slice(pin.bytes())?;
                ensure!(
                    claim.schema == 1 && claim.object == id && claim.central == self.header.central,
                    "completed reclaim claim is damaged"
                );
                // A previous attempt may have removed our claim and crashed
                // before saving the small completion state. Another real
                // operation's new claim is never this completion's property.
                if claim.operation == self.header.operation {
                    ensure!(
                        control
                            .read(ControlNamespace::Catalog, &object::manifest_name(id))?
                            .is_none(),
                        "completed reclaim has a Ready object beneath its original claim"
                    );
                    self.validate_claim(index, pin.bytes())?;
                    control.remove_with_before_sync(&pin, || {
                        cutpoint("claim-unlink-unsynced");
                        Ok(())
                    })?;
                }
            }
            sync_namespace(
                context,
                &control,
                ControlNamespace::Journals,
                "claim-parent",
                Some(id),
            )?;
            drop(control);
            cutpoint("claim-cleaned");
        }
        self.state.claims_cleaned = true;
        self.save_state()?;
        super::completion::publish(self)?;
        cutpoint("completion-receipt");
        Ok(())
    }
}
fn decode_record(header: &Header, link: &Link, pin: &ControlSnapshot) -> Result<Record> {
    ensure!(
        object::pair_snapshot(pin) == link.identity && hash(pin.bytes()) == link.sha256,
        "reclaim immutable record changed"
    );
    let record: Record = serde_json::from_slice(pin.bytes())?;
    record.validate()?;
    ensure!(
        record.object == link.object && !header.operation.is_nil(),
        "reclaim record belongs to another object"
    );
    Ok(record)
}
pub(super) fn pending(context: &OperationContext) -> Result<Vec<PendingReclaim>> {
    let path = context.requested_data_root().join("projects");
    let Some(read) = ReadStore::open_readonly(
        &path,
        object::uid(),
        Default::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(Vec::new());
    };
    let control = read.control(ControlLimits::default())?;
    let central = control.store_identity()?;
    let mut snapshots = Vec::new();
    let mut bytes = 0usize;
    let mut completed = Vec::new();
    for name in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
        let Some(raw) = name
            .strip_prefix(PREFIX)
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        let operation = Uuid::parse_str(raw)?;
        ensure!(
            name == header_name(operation),
            "reclaim journal name is not canonical"
        );
        let pin = control
            .read(ControlNamespace::Journals, &name)?
            .context("pending reclaim journal disappeared")?;
        let header = decode_header(pin.bytes(), operation, [central.0, central.1])?;
        let state_pin = control
            .read(ControlNamespace::Journals, &state_name(operation))?
            .context("pending reclaim progress disappeared")?;
        let state: State = serde_json::from_slice(state_pin.bytes())?;
        ensure!(
            !state.claims_cleaned || state.complete,
            "reclaim claim cleanup precedes completion"
        );
        ensure!(
            !state.complete || state.retired_cleaned,
            "reclaim completion precedes retirement cleanup"
        );
        ensure!(
            !state.retired_cleaned || state.claims_ready,
            "reclaim retirement precedes original claims"
        );
        if state.complete && state.claims_cleaned && state.claims_ready && state.retired_cleaned {
            if let Some(value) = super::completion::project(&control, &header, pin.bytes())? {
                completed.push(value);
                continue;
            }
        }
        let mut records = Vec::new();
        let mut journals = vec![
            ("projects".into(), name),
            ("projects".into(), state_name(operation)),
        ];
        for (index, link) in header.records.iter().enumerate() {
            let pin = control
                .read(ControlNamespace::Journals, &link.name)?
                .context("pending reclaim immutable record disappeared")?;
            bytes = bytes
                .checked_add(pin.bytes().len())
                .filter(|n| *n <= 64 * 1024 * 1024)
                .context("pending reclaim record capacity exceeded")?;
            let record = decode_record(&header, link, &pin)?;
            let cursor_pin = control
                .read(ControlNamespace::Journals, &cursor_name(operation, index))?
                .context("pending reclaim cursor disappeared")?;
            let cursor: Cursor = serde_json::from_slice(cursor_pin.bytes())?;
            cursor.validate(record.inventory.entries().len(), record.payload.is_some())?;
            ensure!(
                !state.complete || cursor.allocation_removed,
                "reclaim completion precedes physical deletion"
            );
            journals.push(("projects".into(), link.name.clone()));
            journals.push(("projects".into(), cursor_name(operation, index)));
            records.push(record);
        }
        snapshots.push((header, state, records, journals));
    }
    read.check_binding()?;
    drop(control);
    drop(read);
    for (_, state, _, _) in &snapshots {
        ensure!(
            !state.complete || state.retired_cleaned,
            "reclaim completion precedes retirement cleanup"
        );
        ensure!(
            !state.retired_cleaned || state.claims_ready,
            "reclaim retirement precedes original claims"
        );
    }
    let mut values = completed;
    let mut stores = object::Stores::new();
    for (header, state, records, mut journals) in snapshots {
        let mut members = Vec::new();
        let mut directories = Vec::new();
        for (index, record) in records.iter().enumerate() {
            let store = object::store(context, &mut stores, record.object, false)?;
            ensure!(
                object::pair(&store.root)? == record.store,
                "pending reclaim belongs to another physical library"
            );
            let mut decoded = Record::decode(
                &store,
                record.object,
                record.manifest.clone(),
                record.manifest_identity,
            )?;
            decoded.captured = record.captured.clone();
            ensure!(
                decoded == *record,
                "pending reclaim original manifest projection changed"
            );
            if let Some(pin) = store.snapshot(
                ControlNamespace::Journals,
                &object::claim_name(record.object),
            )? {
                let claim: Claim = serde_json::from_slice(pin.bytes())?;
                ensure!(
                    claim.schema == 1
                        && claim.object == record.object
                        && claim.central == header.central,
                    "pending deletion claim is damaged"
                );
                if claim.operation == header.operation {
                    ensure!(
                        claim.record == header.records[index].sha256,
                        "pending deletion claim record changed"
                    );
                    journals.push((
                        object::store_name(record.object).into(),
                        object::claim_name(record.object),
                    ));
                } else {
                    ensure!(
                        state.complete,
                        "pending deletion claim belongs to another operation"
                    );
                }
            } else {
                ensure!(
                    !state.claims_ready || state.complete,
                    "pending deletion claim disappeared"
                );
            }
            members.push((record.object, record.dependencies.clone()));
            directories.push((record.object, record.allocation.clone()));
            directories.push((record.object, record.captured.clone()));
        }
        values.push(PendingReclaim {
            operation_id: header.operation,
            objects: header.records.iter().map(|r| r.object).collect(),
            members,
            directories,
            journals,
            complete: state.complete,
        });
    }
    Ok(values)
}
#[cfg(test)]
fn cutpoint(label: &str) {
    if std::env::var("PAKRO_TEST_RECLAIM_CUTPOINT").ok().as_deref() == Some(label) {
        unsafe { nix::libc::_exit(86) }
    }
}
#[cfg(not(test))]
fn cutpoint(_: &str) {}
pub(super) fn durable(label: &str) {
    cutpoint(label)
}
pub(super) fn sync_parent(
    context: &OperationContext,
    parent: &std::fs::File,
    phase: &str,
    object: Option<ReclaimId>,
) -> Result<()> {
    context.checkpoint()?;
    parent.sync_all()?;
    context.checkpoint()?;
    observed_sync(context, parent, phase, object)
}
pub(super) fn sync_namespace(
    context: &OperationContext,
    control: &pakro_store::control::WriterControl<'_>,
    namespace: ControlNamespace,
    phase: &str,
    object: Option<ReclaimId>,
) -> Result<()> {
    context.checkpoint()?;
    control.sync_namespace(namespace)?;
    context.checkpoint()?;
    observed_sync(context, control.namespace_handle(namespace), phase, object)
}
#[cfg(not(test))]
fn observed_sync(
    _: &OperationContext,
    _: &std::fs::File,
    _: &str,
    _: Option<ReclaimId>,
) -> Result<()> {
    Ok(())
}
#[cfg(test)]
fn observed_sync(
    context: &OperationContext,
    parent: &std::fs::File,
    phase: &str,
    object: Option<ReclaimId>,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if std::env::var("PAKRO_TEST_RECLAIM_SYNC_PROOF")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(());
    }
    let stage = context
        .requested_data_root()
        .parent()
        .context("sync proof requires owned fixture")?;
    ensure!(
        std::fs::read(stage.join(".reclaim-test-owner"))? == b"pakro-owned-reclaim-fixture-v1",
        "sync proof is outside owned fixture"
    );
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(stage.join(".reclaim-sync-proof"))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == object::uid()
            && meta.nlink() == 1
            && meta.mode() & 0o7777 == 0o600,
        "sync proof file identity is unsafe"
    );
    let bytes = serde_json::to_vec(&(phase, object, object::pair(parent)?))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
