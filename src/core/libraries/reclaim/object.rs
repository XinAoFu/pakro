use super::*;
use crate::libraries::language::manifest::LanguageManifest;
use crate::libraries::package::manifest::{LayoutManifest, PackageManifest};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, TreeInventory, observe_tree_inventory,
};
use eyre::{ContextCompat, ensure};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, ReadStore, StoreWriter, ensure_entry_identity,
    open_directory, open_trusted_store_with_deadline,
};
use std::{
    collections::BTreeMap,
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(test)]
thread_local! {
    static STORE_BINDING_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) fn store_binding_checks() -> usize {
    STORE_BINDING_CHECKS.with(|count| count.get())
}
#[derive(Debug)]
struct LibraryUnavailable(&'static str);
impl std::fmt::Display for LibraryUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for LibraryUnavailable {}
#[derive(Debug)]
struct LibraryBindingUnavailable {
    source: eyre::Report,
}
impl std::fmt::Display for LibraryBindingUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "reclaim library binding is unavailable: {:#}",
            self.source
        )
    }
}
impl std::error::Error for LibraryBindingUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(super) fn pair(file: &File) -> Result<[u64; 2]> {
    let id = FileIdentity::of(file)?;
    Ok([id.device as _, id.inode as _])
}
pub(super) fn identity(pair: [u64; 2]) -> Result<FileIdentity> {
    let id = FileIdentity {
        device: pair[0] as _,
        inode: pair[1] as _,
    };
    ensure!(
        [id.device as u64, id.inode as u64] == pair,
        "reclaim physical identity is not representable"
    );
    Ok(id)
}
pub(super) fn uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
pub(super) fn store_name(id: ReclaimId) -> &'static str {
    match id {
        ReclaimId::Language(_) => "languages",
        _ => "packages",
    }
}
pub(super) fn manifest_name(id: ReclaimId) -> String {
    format!("{}.json", id.label())
}
pub(super) fn claim_name(id: ReclaimId) -> String {
    format!("deleting-{}.json", id.label())
}

enum Backing {
    Read(ReadStore),
    Write(Arc<StoreWriter>),
}
pub(super) struct Store {
    backing: Backing,
    pub root: File,
    pub build: File,
    pub path: PathBuf,
}
impl Store {
    pub(super) fn open(
        context: &OperationContext,
        name: &str,
        writable: bool,
    ) -> Result<Option<Arc<Self>>> {
        let path = context.requested_data_root().join(name);
        let backing = if writable {
            let read = ReadStore::open_readonly(
                &path,
                uid(),
                Default::default(),
                context.policy().deadline,
            )?
            .ok_or(LibraryUnavailable(
                "reclaim existing object library is missing",
            ))?;
            let expected = {
                let control = read.control(Default::default())?;
                control.store_identity()?
            };
            let writer = StoreWriter::open_existing(
                &path,
                uid(),
                Default::default(),
                context.policy().deadline,
            )?;
            let actual = pair(writer.root_handle())?;
            ensure!(
                expected == (actual[0], actual[1]),
                "reclaim object library was replaced"
            );
            read.check_binding()?;
            Backing::Write(Arc::new(writer))
        } else {
            let Some(read) = ReadStore::open_readonly(
                &path,
                uid(),
                Default::default(),
                context.policy().deadline,
            )?
            else {
                return Ok(None);
            };
            Backing::Read(read)
        };
        let root =
            open_trusted_store_with_deadline(&path, uid(), false, context.policy().deadline)?;
        let leases = open_directory(&root, ".leases")?;
        let build = open_directory(&leases, "build")?;
        let store = Arc::new(Self {
            backing,
            root,
            build,
            path,
        });
        store.check()?;
        Ok(Some(store))
    }
    pub(super) fn check(&self) -> Result<()> {
        #[cfg(test)]
        STORE_BINDING_CHECKS.with(|count| count.set(count.get() + 1));
        let result = (|| -> Result<()> {
            match &self.backing {
                Backing::Read(read) => {
                    read.check_binding()?;
                    let control = read.control(Default::default())?;
                    let p = pair(&self.root)?;
                    ensure!(
                        control.store_identity()? == (p[0], p[1]),
                        "reclaim store identity changed"
                    );
                }
                Backing::Write(writer) => {
                    writer.check_binding()?;
                    ensure!(
                        pair(writer.root_handle())? == pair(&self.root)?,
                        "reclaim store identity changed"
                    );
                }
            }
            Ok(())
        })();
        result.map_err(|source| LibraryBindingUnavailable { source }.into())
    }
    pub(super) fn writer(&self) -> Result<&StoreWriter> {
        match &self.backing {
            Backing::Write(writer) => Ok(writer),
            _ => eyre::bail!("readonly reclaim view has no writer"),
        }
    }
    pub(super) fn snapshot(
        &self,
        ns: ControlNamespace,
        name: &str,
    ) -> Result<Option<pakro_store::control::ControlSnapshot>> {
        self.check()?;
        match &self.backing {
            Backing::Read(read) => read.control(Default::default())?.read(ns, name),
            Backing::Write(writer) => writer.control(Default::default())?.read(ns, name),
        }
    }
}
pub(super) type Stores = BTreeMap<&'static str, Arc<Store>>;

fn check_ready(store: &Store, record: &Record, cursor: &super::journal::Cursor) -> Result<()> {
    let snapshot = store.snapshot(ControlNamespace::Catalog, &manifest_name(record.object))?;
    match snapshot {
        Some(snapshot) => {
            ensure!(!cursor.revoked, "revoked reclaim object was republished");
            ensure!(
                snapshot.bytes() == record.manifest && pair_snapshot(&snapshot) == record.manifest_identity,
                "reclaim original manifest changed"
            );
        }
        None => ensure!(cursor.revoke_pending || cursor.revoked,
            "reclaim Ready manifest disappeared without an intent"),
    }
    Ok(())
}

pub(super) fn store(
    context: &OperationContext,
    stores: &mut Stores,
    id: ReclaimId,
    writable: bool,
) -> Result<Arc<Store>> {
    let name = store_name(id);
    if let Some(value) = stores.get(name).cloned() {
        if !writable || matches!(&value.backing, Backing::Write(_)) {
            return Ok(value);
        }
        context.checkpoint()?;
        ensure!(
            context.policy().writes.libraries,
            "operation policy forbids reclaim writes"
        );
        value.check()?;
        let original_root = pair(&value.root)?;
        let original_build = pair(&value.build)?;
        let upgraded = Store::open(context, name, true)?.ok_or(LibraryUnavailable(
            "reclaim library disappeared during writer upgrade",
        ))?;
        upgraded.check()?;
        value.check()?;
        if pair(&upgraded.root)? != original_root || pair(&upgraded.build)? != original_build {
            return Err(
                LibraryUnavailable("reclaim library was replaced during writer upgrade").into(),
            );
        }
        stores.insert(name, upgraded.clone());
        return Ok(upgraded);
    }
    let value = Store::open(context, name, writable)?
        .ok_or(LibraryUnavailable("reclaim library is missing"))?;
    stores.insert(name, value.clone());
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub object: ReclaimId,
    pub store: [u64; 2],
    pub manifest: Vec<u8>,
    pub manifest_identity: [u64; 2],
    pub allocation: String,
    pub allocation_identity: [u64; 2],
    pub payload: Option<[u64; 2]>,
    pub lock: [u64; 2],
    pub inventory: TreeInventory,
    pub captured: String,
    pub dependencies: Vec<ReclaimId>,
}
impl Record {
    pub(super) fn decode(
        store: &Store,
        id: ReclaimId,
        bytes: Vec<u8>,
        manifest_identity: [u64; 2],
    ) -> Result<Self> {
        let (allocation, allocation_identity, payload, lock, inventory, dependencies) = match id {
            ReclaimId::Language(key) => {
                let m: LanguageManifest = serde_json::from_slice(&bytes)?;
                ensure!(
                    matches!(m.schema, 1 | 2 | 3) && m.key == key,
                    "reclaim language identity differs"
                );
                let spec =
                    crate::libraries::language::repository::LanguageRepository::spec_from_manifest(
                        &m,
                    )?;
                crate::libraries::language::publication::validate_manifest(&spec, &m)?;
                crate::libraries::language::publication::validate_manifest_location(
                    &m,
                    &store.path.join(id.label()).join("payload"),
                )?;
                (
                    id.label(),
                    [m.allocation.device, m.allocation.inode],
                    Some([m.payload.device, m.payload.inode]),
                    [m.lock.device, m.lock.inode],
                    m.inventory,
                    m.runtime_dependencies.into_iter().map(ReclaimId::Package).collect(),
                )
            }
            ReclaimId::Package(key) => {
                let m: PackageManifest = serde_json::from_slice(&bytes)?;
                m.validate_metadata()?;
                ensure!(
                    m.key() == key && m.storage.key == key,
                    "reclaim package identity differs"
                );
                let deps = m
                    .facts
                    .dependencies
                    .iter()
                    .map(|k| ReclaimId::Package(*k))
                    .chain(m.language.into_iter().map(ReclaimId::Language))
                    .collect();
                (
                    m.storage.allocation,
                    m.storage.root_identity,
                    None,
                    m.storage.lock,
                    m.storage.inventory,
                    deps,
                )
            }
            ReclaimId::Layout(key) => {
                let m: LayoutManifest = serde_json::from_slice(&bytes)?;
                m.validate_metadata()?;
                ensure!(m.storage.key == key, "reclaim layout identity differs");
                let deps = m
                    .packages
                    .iter()
                    .map(|k| ReclaimId::Package(*k))
                    .chain([ReclaimId::Language(m.language)])
                    .collect();
                (
                    m.storage.allocation,
                    m.storage.root_identity,
                    None,
                    m.storage.lock,
                    m.storage.inventory,
                    deps,
                )
            }
        };
        for p in [allocation_identity, lock, manifest_identity] {
            identity(p)?;
        }
        if let Some(p) = payload {
            identity(p)?;
        }
        let record = Self {
            object: id,
            store: pair(&store.root)?,
            manifest: bytes,
            manifest_identity,
            allocation,
            allocation_identity,
            payload,
            lock,
            inventory,
            captured: format!("removing-{}", Uuid::now_v7()),
            dependencies,
        };
        record.validate()?;
        Ok(record)
    }
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.manifest.len() <= 64 * 1024 * 1024,
            "reclaim manifest capacity exceeded"
        );
        ensure!(
            self.captured.starts_with("removing-") && Uuid::parse_str(&self.captured[9..]).is_ok(),
            "invalid reclaim capture identity"
        );
        ensure!(
            self.allocation
                .as_bytes()
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
                && !self.allocation.is_empty(),
            "invalid reclaim allocation name"
        );
        for entry in self.inventory.entries() {
            ensure!(
                !entry.relative.as_os_str().is_empty()
                    && entry
                        .relative
                        .components()
                        .all(|p| matches!(p, std::path::Component::Normal(_))),
                "unsafe reclaim inventory path"
            );
        }
        Ok(())
    }
    pub(super) fn root_path(&self, store: &Store) -> PathBuf {
        let path = store.path.join(&self.allocation);
        if self.payload.is_some() {
            path.join("payload")
        } else {
            path
        }
    }
    pub(super) fn lock_name(&self) -> String {
        format!("{}.lock", self.allocation)
    }
}

/// Only this actual EX group member may enter the fd-relative tree engine.
pub(super) struct OwnedObject {
    pub store: Arc<Store>,
    pub record: Record,
    pub allocation: Option<File>,
    pub root: Option<File>,
    lock: StableLock,
    held: ObjectLease,
}
impl OwnedObject {
    pub(super) fn acquire(
        context: &OperationContext,
        store: Arc<Store>,
        record: Record,
        cursor: &super::journal::Cursor,
    ) -> Result<Option<Self>> {
        context.checkpoint()?;
        store.check()?;
        record.validate()?;
        if pair(&store.root)? != record.store {
            return Err(LibraryUnavailable("reclaim belongs to another physical store").into());
        }
        cursor.validate(record.inventory.entries().len(), record.payload.is_some())?;
        let mut decoded = Record::decode(
            &store,
            record.object,
            record.manifest.clone(),
            record.manifest_identity,
        )?;
        decoded.captured = record.captured.clone();
        ensure!(
            decoded == record,
            "reclaim immutable projections differ from the original manifest"
        );
        if let ReclaimId::Language(key) = record.object {
            let registration = store
                .snapshot(
                    ControlNamespace::Ownership,
                    &format!("{}.lock.json", record.object.label()),
                )?
                .context("reclaim language lock registration is missing")?;
            let registration: crate::libraries::language::manifest::LockRegistration =
                serde_json::from_slice(registration.bytes())?;
            ensure!(
                registration.schema == 1
                    && registration.key == key
                    && [registration.lock.device, registration.lock.inode] == record.lock,
                "reclaim language protection registration changed"
            );
        }
        let lock = StableLock::open_existing(
            &store.build,
            &record.lock_name(),
            uid(),
            identity(record.lock)?,
        )?;
        let Some(held) = lock.try_acquire(LeaseMode::Exclusive, context.policy().deadline)? else {
            return Ok(None);
        };
        check_ready(&store, &record, cursor)?;
        let original = stat_identity(&store.root, &record.allocation)?;
        let captured = stat_identity(&store.root, &record.captured)?;
        ensure!(
            !(original.is_some() && captured.is_some()),
            "reclaim original and captured directories both exist"
        );
        let name = if let Some(id) = captured {
            ensure!(
                cursor.capture_pending || cursor.captured,
                "reclaim capture has no durable intent"
            );
            ensure!(
                id == record.allocation_identity,
                "reclaim captured directory was replaced"
            );
            Some(record.captured.as_str())
        } else if let Some(id) = original {
            ensure!(!cursor.captured, "reclaim captured directory moved back");
            ensure!(
                id == record.allocation_identity,
                "reclaim original directory was replaced"
            );
            Some(record.allocation.as_str())
        } else {
            ensure!(
                cursor.allocation_pending || cursor.allocation_removed,
                "reclaim allocation disappeared without an intent"
            );
            None
        };
        let allocation = name
            .map(|name| open_directory(&store.root, name))
            .transpose()?;
        ensure!(
            !cursor.allocation_removed || allocation.is_none(),
            "removed reclaim allocation reappeared"
        );
        let root = if let Some(allocation) = &allocation {
            if let Some(expected) = record.payload {
                match stat_identity(allocation, "payload")? {
                    Some(id) => {
                        ensure!(id == expected, "reclaim payload was replaced");
                        Some(open_directory(allocation, "payload")?)
                    }
                    None => {
                        ensure!(
                            cursor.payload_pending || cursor.payload_removed,
                            "reclaim payload disappeared without an intent"
                        );
                        None
                    }
                }
            } else {
                Some(allocation.try_clone()?)
            }
        } else {
            None
        };
        ensure!(
            !cursor.payload_removed || root.is_none(),
            "removed reclaim payload reappeared"
        );
        let value = Self {
            store,
            record,
            allocation,
            root,
            lock,
            held,
        };
        value.check(context)?;
        Ok(Some(value))
    }
    pub(super) fn check(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.store.check()?;
        self.check_held(context)
    }
    /// The enclosing verification keeps full library/manifest admission at
    /// entry and exit. Every inventory IO still checks these original FDs,
    /// actual EX protection and allocation/payload names without reopening the
    /// whole store control plane for each directory entry or read block.
    pub(super) fn check_held(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive
                && self.held.identity() == self.lock.identity(),
            "reclaim lost actual EX protection"
        );
        self.lock
            .check_location(&self.store.build, &self.record.lock_name())?;
        if let Some(file) = &self.allocation {
            let metadata = file.metadata()?;
            ensure!(
                metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o7777 == 0o700,
                "reclaim allocation type, owner or mode changed"
            );
            ensure!(
                pair(file)? == self.record.allocation_identity,
                "reclaim allocation identity changed"
            );
            let original = stat_identity(&self.store.root, &self.record.allocation)?;
            let captured = stat_identity(&self.store.root, &self.record.captured)?;
            ensure!(
                matches!((original,captured),(Some(id),None)|(None,Some(id)) if id==self.record.allocation_identity),
                "reclaim allocation namespace changed"
            );
        }
        if let Some(file) = &self.root {
            ensure!(
                pair(file)?
                    == self
                        .record
                        .payload
                        .unwrap_or(self.record.allocation_identity),
                "reclaim root identity changed"
            );
            if let Some(payload) = self.record.payload {
                ensure_entry_identity(
                    self.allocation
                        .as_ref()
                        .context("reclaim allocation is missing")?,
                    "payload",
                    identity(payload)?,
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn verify(
        &self,
        context: &OperationContext,
        cursor: &super::journal::Cursor,
    ) -> Result<()> {
        self.verify_with_after_walk(context, cursor, || Ok(()))
    }
    pub(super) fn verify_with_after_walk(
        &self,
        context: &OperationContext,
        cursor: &super::journal::Cursor,
        after_walk: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.check(context)?;
        super::tree::verify_remaining(self, context, cursor)?;
        if cursor.next == 0 && !cursor.pending {
            if let ReclaimId::Package(_) = self.record.object {
                let manifest: PackageManifest = serde_json::from_slice(&self.record.manifest)?;
                crate::libraries::package::lease::validate_package_facts(
                    self.root.as_ref().context("package root is missing")?,
                    &manifest,
                )?;
            }
        }
        after_walk()?;
        check_ready(&self.store, &self.record, cursor)?;
        self.check_held(context)
    }
}
pub(super) fn pair_snapshot(snapshot: &pakro_store::control::ControlSnapshot) -> [u64; 2] {
    let p = snapshot.file_identity();
    [p.device as _, p.inode as _]
}
pub(super) fn stat_identity(parent: &File, name: &str) -> Result<Option<[u64; 2]>> {
    match nix::sys::stat::fstatat(parent, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some([stat.st_dev as _, stat.st_ino as _])),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub(super) fn describe(context: &OperationContext, store: &Store, id: ReclaimId) -> Result<Record> {
    context.checkpoint()?;
    let snapshot = store
        .snapshot(ControlNamespace::Catalog, &manifest_name(id))?
        .context("reclaim Ready manifest is missing")?;
    Record::decode(
        store,
        id,
        snapshot.bytes().to_vec(),
        pair_snapshot(&snapshot),
    )
}
pub(super) fn failure_status(error: &eyre::Report) -> ReclaimStatus {
    if error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some()
            || cause.downcast_ref::<nix::errno::Errno>().is_some()
            || cause
                .downcast_ref::<crate::programs::OperationError>()
                .is_some()
            || cause.downcast_ref::<LibraryUnavailable>().is_some()
            || cause.downcast_ref::<LibraryBindingUnavailable>().is_some()
    }) {
        ReclaimStatus::Unknown
    } else {
        ReclaimStatus::Damaged
    }
}
pub(super) fn probe(context: &OperationContext, id: ReclaimId) -> Result<ReclaimEntry> {
    if let Some(store) = Store::open(context, store_name(id), false)? {
        if store
            .snapshot(ControlNamespace::Journals, &claim_name(id))?
            .is_some()
        {
            return Ok(super::entry(
                id,
                ReclaimStatus::PendingRecovery,
                "object has a pending deletion claim",
            ));
        }
        let result: Result<ReclaimEntry> = (|| {
            let record = describe(context, &store, id)?;
            let Some(owner) = OwnedObject::acquire(context, store, record, &Default::default())?
            else {
                return Ok(super::entry(
                    id,
                    ReclaimStatus::Busy,
                    "object has an active lease",
                ));
            };
            owner.verify(context, &Default::default())?;
            Ok(super::entry(
                id,
                ReclaimStatus::Eligible,
                "complete object is not busy",
            ))
        })();
        return match result {
            Ok(row) => Ok(row),
            Err(error) => Ok(super::entry(
                id,
                failure_status(&error),
                format!("{error:#}"),
            )),
        };
    }
    Ok(super::entry(
        id,
        ReclaimStatus::Unknown,
        "library is absent",
    ))
}
