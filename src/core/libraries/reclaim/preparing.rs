//! Bootstrap metadata is durable before any operation fragment exists. It
//! protects the fixed candidates, but never authorizes object deletion.
use super::{
    MAX_GROUP_OBJECTS, PendingReclaim, ReclaimEntry, ReclaimId, ReclaimMode, ReclaimReport,
    ReclaimStatus, object,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::lease::{LeaseMode, StableLock};
use pakro_store::control::{
    ControlNamespace, ControlSnapshot, ReadStore, StoreWriter, open_directory,
    open_trusted_store_with_deadline,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const PREFIX: &str = "reclaim-preparing-";
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_OBJECTS: usize = 1024;

fn name(operation: Uuid) -> String {
    format!("{PREFIX}{operation}.json")
}
fn lock_name(operation: Uuid) -> String {
    format!("reclaim-{operation}.lock")
}
fn record_name(operation: Uuid, index: usize) -> String {
    format!("reclaim-object-{operation}-{index}.json")
}
fn cursor_name(operation: Uuid, index: usize) -> String {
    format!("reclaim-cursor-{operation}-{index}.json")
}
fn state_name(operation: Uuid) -> String {
    format!("reclaim-state-{operation}.json")
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fragment {
    name: String,
    len: usize,
    sha256: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    #[serde(with = "super::uuid_serde")]
    operation: Uuid,
    central: [u64; 2],
    lock: [u64; 2],
    mode: ReclaimMode,
    groups: Vec<Vec<ReclaimId>>,
    fragments: Vec<Fragment>,
    aborted: bool,
}

impl Marker {
    fn objects(&self) -> Vec<ReclaimId> {
        self.groups.iter().flatten().copied().collect()
    }

    fn validate(&self, operation: Uuid, central: [u64; 2]) -> Result<()> {
        ensure!(
            self.schema == 1
                && !operation.is_nil()
                && self.operation == operation
                && self.central == central,
            "reclaim Preparing identity changed"
        );
        object::identity(self.central)?;
        object::identity(self.lock)?;
        let objects = self.objects();
        ensure!(
            !objects.is_empty()
                && objects.len() <= MAX_OBJECTS
                && self.groups.len() <= MAX_OBJECTS
                && self
                    .groups
                    .iter()
                    .all(|group| !group.is_empty() && group.len() <= MAX_GROUP_OBJECTS)
                && objects.iter().copied().collect::<BTreeSet<_>>().len() == objects.len(),
            "reclaim Preparing fixed groups are invalid"
        );
        let expected = expected_names(operation, objects.len());
        let actual = self
            .fragments
            .iter()
            .map(|fragment| fragment.name.clone())
            .collect::<BTreeSet<_>>();
        ensure!(
            self.fragments.len() == expected.len() && actual == expected,
            "reclaim Preparing fragment set changed"
        );
        let mut total = 0usize;
        for fragment in &self.fragments {
            ensure!(fragment.len != 0, "reclaim Preparing fragment is empty");
            total = total
                .checked_add(fragment.len)
                .filter(|total| *total <= MAX_BYTES)
                .context("reclaim Preparing payload capacity exceeded")?;
        }
        Ok(())
    }
}

fn expected_names(operation: Uuid, count: usize) -> BTreeSet<String> {
    (0..count)
        .flat_map(|index| [record_name(operation, index), cursor_name(operation, index)])
        .chain([state_name(operation)])
        .collect()
}

fn decode(pin: &ControlSnapshot, operation: Uuid, central: [u64; 2]) -> Result<Marker> {
    let marker: Marker = serde_json::from_slice(pin.bytes())?;
    marker.validate(operation, central)?;
    Ok(marker)
}

/// Kept by the initializer until the formal Prepared header and all initial
/// fragments have been durably installed.
pub(super) struct Preparing {
    marker: Marker,
    pin: ControlSnapshot,
}

impl Preparing {
    pub(super) fn create(
        writer: &StoreWriter,
        operation: Uuid,
        lock: [u64; 2],
        mode: ReclaimMode,
        groups: &[Vec<ReclaimId>],
        payloads: &[(String, Vec<u8>)],
    ) -> Result<Self> {
        writer.check_binding()?;
        let marker = Marker {
            schema: 1,
            operation,
            central: object::pair(writer.root_handle())?,
            lock,
            mode,
            groups: groups.to_vec(),
            fragments: payloads
                .iter()
                .map(|(name, bytes)| Fragment {
                    name: name.clone(),
                    len: bytes.len(),
                    sha256: hash(bytes),
                })
                .collect(),
            aborted: false,
        };
        marker.validate(operation, marker.central)?;
        validate_initial_payloads(&marker, payloads)?;
        let operation_lock = StableLock::open_existing(
            writer.build_handle(),
            &lock_name(operation),
            object::uid(),
            object::identity(lock)?,
        )?;
        operation_lock.check_location(writer.build_handle(), &lock_name(operation))?;
        let bytes = serde_json::to_vec(&marker)?;
        ensure!(
            bytes.len() <= MAX_BYTES,
            "reclaim Preparing marker capacity exceeded"
        );
        let mut control = writer.control(Default::default())?;
        ensure!(
            control
                .read(
                    ControlNamespace::Journals,
                    &super::journal::header_name(operation)
                )?
                .is_none(),
            "reclaim Preparing operation already has a formal header"
        );
        for fragment in &marker.fragments {
            ensure!(
                control
                    .read(ControlNamespace::Journals, &fragment.name)?
                    .is_none(),
                "reclaim Preparing fragment already exists"
            );
        }
        let pin = control.replace(ControlNamespace::Journals, &name(operation), None, &bytes)?;
        writer.check_binding()?;
        operation_lock.check_location(writer.build_handle(), &lock_name(operation))?;
        Ok(Self { marker, pin })
    }

    pub(super) fn finish(self, writer: &StoreWriter) -> Result<()> {
        writer.check_binding()?;
        ensure!(
            object::pair(writer.root_handle())? == self.marker.central,
            "reclaim Preparing belongs to another physical project library"
        );
        let operation = self.marker.operation;
        let operation_lock = StableLock::open_existing(
            writer.build_handle(),
            &lock_name(operation),
            object::uid(),
            object::identity(self.marker.lock)?,
        )?;
        let mut control = writer.control(Default::default())?;
        let current = control
            .read(ControlNamespace::Journals, &name(operation))?
            .context("reclaim Preparing marker disappeared")?;
        ensure_same(&self.pin, &current)?;
        let header_pin = control
            .read(
                ControlNamespace::Journals,
                &super::journal::header_name(operation),
            )?
            .context("reclaim Preparing cannot finish before its formal header")?;
        let header: super::journal::Header = serde_json::from_slice(header_pin.bytes())?;
        ensure!(
            header.schema == 1
                && header.operation == operation
                && header.central == self.marker.central
                && header.lock == self.marker.lock
                && header.mode == self.marker.mode
                && header.groups == self.marker.groups,
            "reclaim Prepared header differs from its Preparing record"
        );
        let present = fragments(&self.marker, |name| {
            control.read(ControlNamespace::Journals, name)
        })?;
        ensure!(
            present.len() == self.marker.fragments.len(),
            "reclaim Prepared header has missing initial fragments"
        );
        let objects = self.marker.objects();
        ensure!(
            header.records.len() == objects.len(),
            "reclaim Prepared record set differs"
        );
        for (index, link) in header.records.iter().enumerate() {
            let record_name = record_name(operation, index);
            let identity = present
                .get(&record_name)
                .context("reclaim Prepared record disappeared")?;
            let fragment = self
                .marker
                .fragments
                .iter()
                .find(|fragment| fragment.name == record_name)
                .context("reclaim Preparing record descriptor disappeared")?;
            ensure!(
                link.object == objects[index]
                    && link.name == record_name
                    && link.sha256 == fragment.sha256
                    && link.identity == *identity,
                "reclaim Prepared immutable record differs from its Preparing record"
            );
        }
        operation_lock.check_location(writer.build_handle(), &lock_name(operation))?;
        control.remove(&current)?;
        writer.check_binding()
    }
}

fn validate_initial_payloads(marker: &Marker, payloads: &[(String, Vec<u8>)]) -> Result<()> {
    let payloads = payloads
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    for (index, object) in marker.objects().iter().enumerate() {
        let record: object::Record =
            serde_json::from_slice(payloads[record_name(marker.operation, index).as_str()])?;
        record.validate()?;
        ensure!(
            record.object == *object,
            "reclaim Preparing immutable record has another object"
        );
        let cursor: serde_json::Value =
            serde_json::from_slice(payloads[cursor_name(marker.operation, index).as_str()])?;
        ensure!(
            cursor == serde_json::to_value(super::journal::Cursor::default())?,
            "reclaim Preparing must precede all deletion progress"
        );
    }
    let state: InitialState =
        serde_json::from_slice(payloads[state_name(marker.operation).as_str()])?;
    ensure!(
        !state.complete && !state.claims_cleaned && !state.claims_ready && !state.retired_cleaned,
        "reclaim Preparing must precede all claim and deletion progress"
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitialState {
    complete: bool,
    claims_cleaned: bool,
    claims_ready: bool,
    retired_cleaned: bool,
}

fn ensure_same(expected: &ControlSnapshot, current: &ControlSnapshot) -> Result<()> {
    ensure!(
        expected.file_identity() == current.file_identity() && expected.bytes() == current.bytes(),
        "reclaim Preparing marker changed"
    );
    Ok(())
}

fn fragments(
    marker: &Marker,
    mut read: impl FnMut(&str) -> Result<Option<ControlSnapshot>>,
) -> Result<BTreeMap<String, [u64; 2]>> {
    let mut present = BTreeMap::new();
    for fragment in &marker.fragments {
        if let Some(pin) = read(&fragment.name)? {
            ensure!(
                pin.bytes().len() == fragment.len && hash(pin.bytes()) == fragment.sha256,
                "reclaim Preparing fragment bytes changed: {}",
                fragment.name
            );
            present.insert(fragment.name.clone(), object::pair_snapshot(&pin));
        }
    }
    Ok(present)
}

fn check_names(marker: &Marker, names: &[String]) -> Result<()> {
    let expected = expected_names(marker.operation, marker.objects().len());
    let prefixes = [
        format!("reclaim-object-{}-", marker.operation),
        format!("reclaim-cursor-{}-", marker.operation),
    ];
    for name in names {
        if prefixes.iter().any(|prefix| name.starts_with(prefix))
            || name == &state_name(marker.operation)
        {
            ensure!(
                expected.contains(name),
                "reclaim Preparing has an unrecorded fragment: {name}"
            );
        }
    }
    Ok(())
}

fn check_operation_lock(context: &OperationContext, marker: &Marker) -> Result<()> {
    let root = open_trusted_store_with_deadline(
        &context.requested_data_root().join("projects"),
        object::uid(),
        false,
        context.policy().deadline,
    )?;
    ensure!(
        object::pair(&root)? == marker.central,
        "reclaim Preparing central library was replaced"
    );
    let leases = open_directory(&root, ".leases")?;
    let build = open_directory(&leases, "build")?;
    let lock = StableLock::open_existing(
        &build,
        &lock_name(marker.operation),
        object::uid(),
        object::identity(marker.lock)?,
    )?;
    lock.check_location(&build, &lock_name(marker.operation))
}

/// A formal operation can coexist with its final bootstrap marker after a
/// crash. That projection contains only known metadata; callers merge it by
/// operation ID and retain the formal operation's roots and original edges.
pub(super) fn pending(context: &OperationContext) -> Result<Vec<PendingReclaim>> {
    context.checkpoint()?;
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
    let control = read.control(Default::default())?;
    let central = control.store_identity()?;
    let names = control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)?;
    let mut snapshots = Vec::new();
    for marker_name in &names {
        let Some(raw) = marker_name
            .strip_prefix(PREFIX)
            .and_then(|raw| raw.strip_suffix(".json"))
        else {
            continue;
        };
        let operation = Uuid::parse_str(raw)?;
        ensure!(
            *marker_name == name(operation),
            "reclaim Preparing marker name is not canonical"
        );
        let pin = control
            .read(ControlNamespace::Journals, marker_name)?
            .context("reclaim Preparing marker disappeared")?;
        let marker = decode(&pin, operation, [central.0, central.1])?;
        let formal = control
            .read(
                ControlNamespace::Journals,
                &super::journal::header_name(operation),
            )?
            .is_some();
        ensure!(
            !formal || !marker.aborted,
            "aborted reclaim Preparing has a formal operation header"
        );
        let mut journals = vec![("projects".into(), marker_name.clone())];
        if !formal {
            check_names(&marker, &names)?;
            journals.extend(
                fragments(&marker, |name| {
                    control.read(ControlNamespace::Journals, name)
                })?
                .into_keys()
                .map(|name| ("projects".into(), name)),
            );
        }
        snapshots.push((marker, formal, journals));
    }
    read.check_binding()?;
    drop(control);
    drop(read);
    let mut stores = object::Stores::new();
    let mut values = Vec::new();
    for (marker, formal, journals) in snapshots {
        check_operation_lock(context, &marker)?;
        let mut members = Vec::new();
        let objects = if formal { Vec::new() } else { marker.objects() };
        if !formal && !marker.aborted {
            // Preparing cannot revoke Ready. Preserve the actual outgoing
            // edges, rather than substituting an empty pending member.
            for &id in &objects {
                let store = object::store(context, &mut stores, id, false)?;
                let record = object::describe(context, &store, id)?;
                members.push((id, record.dependencies));
            }
        }
        values.push(PendingReclaim {
            operation_id: marker.operation,
            objects,
            members,
            directories: Vec::new(),
            journals,
            complete: formal || marker.aborted,
        });
    }
    context.checkpoint()?;
    Ok(values)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    schema: u32,
    #[serde(with = "super::uuid_serde")]
    operation: Uuid,
    object: ReclaimId,
    central: [u64; 2],
    record: [u8; 32],
}

fn no_claims(context: &OperationContext, operation: Uuid) -> Result<()> {
    for namespace in ["languages", "packages"] {
        let path = context.requested_data_root().join(namespace);
        let Some(read) = ReadStore::open_readonly(
            &path,
            object::uid(),
            Default::default(),
            context.policy().deadline,
        )?
        else {
            continue;
        };
        let control = read.control(Default::default())?;
        for name in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
            if !name.starts_with("deleting-") {
                continue;
            }
            let pin = control
                .read(ControlNamespace::Journals, &name)?
                .context("pending deletion claim disappeared")?;
            let claim: Claim = serde_json::from_slice(pin.bytes())?;
            ensure!(
                claim.schema == 1
                    && name == object::claim_name(claim.object)
                    && object::store_name(claim.object) == namespace,
                "reclaim Preparing found an unknown pending deletion claim"
            );
            object::identity(claim.central)?;
            let _ = claim.record;
            ensure!(
                claim.operation != operation,
                "reclaim Preparing already has a deletion claim; refusing bootstrap abort"
            );
        }
        read.check_binding()?;
    }
    Ok(())
}

/// An incomplete bootstrap has never obtained deletion permission. Resume may
/// mark it retained, but cannot infer authority to remove its fragments.
pub(super) fn abort(
    context: &OperationContext,
    operation: Uuid,
    dry_run: bool,
) -> Result<Option<ReclaimReport>> {
    context.checkpoint()?;
    ensure!(!operation.is_nil(), "reclaim operation ID must not be nil");
    if !dry_run {
        ensure!(
            context.policy().writes.libraries,
            "operation policy forbids reclaim Preparing writes"
        );
    }
    let path = context.requested_data_root().join("projects");
    let Some(read) = ReadStore::open_readonly(
        &path,
        object::uid(),
        Default::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(None);
    };
    let control = read.control(Default::default())?;
    if control
        .read(
            ControlNamespace::Journals,
            &super::journal::header_name(operation),
        )?
        .is_some()
    {
        return Ok(None);
    }
    let Some(pin) = control.read(ControlNamespace::Journals, &name(operation))? else {
        return Ok(None);
    };
    let central = control.store_identity()?;
    let mut marker = decode(&pin, operation, [central.0, central.1])?;
    drop(control);
    read.check_binding()?;
    if dry_run {
        let store = object::Store::open(context, "projects", false)?
            .context("reclaim Preparing central library disappeared")?;
        ensure!(
            object::pair(&store.root)? == marker.central,
            "reclaim Preparing central library was replaced"
        );
        let lock = StableLock::open_existing(
            &store.build,
            &lock_name(operation),
            object::uid(),
            object::identity(marker.lock)?,
        )?;
        let held = lock
            .try_acquire(LeaseMode::Exclusive, context.policy().deadline)?
            .ok_or(super::journal::OperationBusy(operation))?;
        ensure!(
            held.mode() == LeaseMode::Exclusive && held.identity() == lock.identity(),
            "reclaim Preparing lacks original operation protection"
        );
        for check in 0..2 {
            context.checkpoint()?;
            held.check_binding()?;
            lock.check_location(&store.build, &lock_name(operation))?;
            store.check()?;
            let control = read.control(Default::default())?;
            ensure!(
                control.store_identity()? == (marker.central[0], marker.central[1]),
                "reclaim Preparing central library was replaced"
            );
            let current = control
                .read(ControlNamespace::Journals, &name(operation))?
                .context("reclaim Preparing marker disappeared")?;
            ensure_same(&pin, &current)?;
            ensure!(
                control
                    .read(
                        ControlNamespace::Journals,
                        &super::journal::header_name(operation)
                    )?
                    .is_none(),
                "reclaim Preparing gained a formal header during preview"
            );
            check_names(
                &marker,
                &control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)?,
            )?;
            fragments(&marker, |name| {
                control.read(ControlNamespace::Journals, name)
            })?;
            drop(control);
            if check == 0 {
                no_claims(context, operation)?;
            }
        }
        read.check_binding()?;
        store.check()?;
        held.check_binding()?;
        lock.check_location(&store.build, &lock_name(operation))?;
        return Ok(Some(ReclaimReport {
            operation_id: Some(operation),
            dry_run: true,
            resumed: true,
            complete: true,
            entries: marker
                .objects()
                .into_iter()
                .map(|id| ReclaimEntry {
                    object: Some(id),
                    label: id.label(),
                    status: ReclaimStatus::Retained,
                    reasons: vec![
                        "incomplete bootstrap can be retained without deleting objects".into(),
                    ],
                })
                .collect(),
        }));
    }
    drop(read);

    // Only an authorized mutation reaches this existing-store writer.
    let writer = StoreWriter::open_existing(
        &path,
        object::uid(),
        Default::default(),
        context.policy().deadline,
    )?;
    ensure!(
        object::pair(writer.root_handle())? == marker.central,
        "reclaim Preparing central library was replaced"
    );
    let lock = StableLock::open_existing(
        writer.build_handle(),
        &lock_name(operation),
        object::uid(),
        object::identity(marker.lock)?,
    )?;
    let held = lock
        .try_acquire(LeaseMode::Exclusive, context.policy().deadline)?
        .ok_or(super::journal::OperationBusy(operation))?;
    ensure!(
        held.mode() == LeaseMode::Exclusive && held.identity() == lock.identity(),
        "reclaim Preparing lacks original operation protection"
    );
    let control = writer.control(Default::default())?;
    let current = control
        .read(ControlNamespace::Journals, &name(operation))?
        .context("reclaim Preparing marker disappeared")?;
    ensure_same(&pin, &current)?;
    ensure!(
        control
            .read(
                ControlNamespace::Journals,
                &super::journal::header_name(operation)
            )?
            .is_none(),
        "reclaim Preparing gained a formal header during admission"
    );
    let names = control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)?;
    check_names(&marker, &names)?;
    fragments(&marker, |name| {
        control.read(ControlNamespace::Journals, name)
    })?;
    drop(control);
    no_claims(context, operation)?;
    context.checkpoint()?;
    held.check_binding()?;
    lock.check_location(writer.build_handle(), &lock_name(operation))?;
    writer.check_binding()?;
    if !dry_run && !marker.aborted {
        let mut control = writer.control(Default::default())?;
        let current = control
            .read(ControlNamespace::Journals, &name(operation))?
            .context("reclaim Preparing marker disappeared")?;
        ensure_same(&pin, &current)?;
        ensure!(
            control
                .read(
                    ControlNamespace::Journals,
                    &super::journal::header_name(operation)
                )?
                .is_none(),
            "reclaim Preparing gained a formal header before abort"
        );
        check_names(
            &marker,
            &control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)?,
        )?;
        fragments(&marker, |name| {
            control.read(ControlNamespace::Journals, name)
        })?;
        marker.aborted = true;
        control.replace(
            ControlNamespace::Journals,
            &name(operation),
            Some(&current),
            &serde_json::to_vec(&marker)?,
        )?;
    }
    held.check_binding()?;
    lock.check_location(writer.build_handle(), &lock_name(operation))?;
    writer.check_binding()?;
    Ok(Some(ReclaimReport {
        operation_id: Some(operation),
        dry_run,
        resumed: true,
        complete: true,
        entries: marker
            .objects()
            .into_iter()
            .map(|id| ReclaimEntry {
                object: Some(id),
                label: id.label(),
                status: ReclaimStatus::Retained,
                reasons: vec![
                    if dry_run {
                        "incomplete bootstrap can be retained without deleting objects"
                    } else {
                        "incomplete bootstrap retained; no object or metadata fragment was deleted"
                    }
                    .into(),
                ],
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker() -> Marker {
        let operation = Uuid::now_v7();
        let groups = vec![vec![ReclaimId::Package([1; 32])]];
        Marker {
            schema: 1,
            operation,
            central: [1, 2],
            lock: [1, 3],
            mode: ReclaimMode::Prune,
            groups,
            fragments: expected_names(operation, 1)
                .into_iter()
                .map(|name| Fragment {
                    name,
                    len: 1,
                    sha256: hash(b"x"),
                })
                .collect(),
            aborted: false,
        }
    }

    #[test]
    fn p4_preparing_requires_the_entire_fixed_fragment_set() {
        let mut value = marker();
        value.validate(value.operation, value.central).unwrap();
        let removed = value.fragments.pop().unwrap();
        assert!(value.validate(value.operation, value.central).is_err());
        value.fragments.push(removed);
        value.fragments[0].name = "reclaim-object-another-operation-0.json".into();
        assert!(value.validate(value.operation, value.central).is_err());
    }

    #[test]
    fn p4_preparing_caps_all_fragments_before_any_write() {
        let mut value = marker();
        value.fragments[0].len = MAX_BYTES;
        assert!(value.validate(value.operation, value.central).is_err());
        value.fragments[0].len = 1;
        let duplicate = value.groups[0][0];
        value.groups[0].push(duplicate);
        assert!(value.validate(value.operation, value.central).is_err());
    }

    #[test]
    fn p4_preparing_rejects_other_roots_and_unrecorded_fragments() {
        let value = marker();
        assert!(value.validate(value.operation, [1, 9]).is_err());
        assert!(value.validate(Uuid::now_v7(), value.central).is_err());
        assert!(check_names(&value, &[cursor_name(value.operation, 10)]).is_err());
        assert!(check_names(&value, &[state_name(Uuid::now_v7())]).is_ok());
    }
}
