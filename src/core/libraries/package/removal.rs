//! Closed removal authority over one package allocation and its stable EX.
use super::lease::{identity, pair, uid};
use super::manifest::{self, LayoutManifest, PackageManifest};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, observe_tree_inventory,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, StoreWriter, ensure_entry_identity, open_directory,
};
use std::fs::File;
use std::path::{Path, PathBuf};

pub(in crate::libraries) struct RemovalPermit<'a> {
    context: &'a OperationContext,
    writer: StoreWriter,
    root: File,
    lock: StableLock,
    held: ObjectLease,
    target: &'a PackageManifest,
    path: PathBuf,
}
impl RemovalPermit<'_> {
    pub(in crate::libraries) fn removal_root(&self) -> &File {
        &self.root
    }
    pub(in crate::libraries) fn removal_inventory(
        &self,
    ) -> &crate::packages::python_environment::prefix::verify::TreeInventory {
        &self.target.storage.inventory
    }
    pub(in crate::libraries) fn check(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.writer.check_binding()?;
        self.held.check_binding()?;
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("{}.lock", self.target.storage.allocation),
        )?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive
                && self.held.identity() == self.lock.identity(),
            "package removal lost EX protection"
        );
        ensure!(
            pair(&self.root)? == self.target.storage.root_identity,
            "package allocation identity changed"
        );
        Ok(())
    }
}
impl InventoryRoot for RemovalPermit<'_> {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        &self.target.storage.allocation
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.check()
    }
}

struct AdapterInvalidation {
    name: String,
    expected: Vec<u8>,
    storage: super::manifest::StorageManifest,
    lock: StableLock,
    held: ObjectLease,
}

fn acquire_adapters(permit: &RemovalPermit<'_>) -> Result<Vec<AdapterInvalidation>> {
    let records = {
        let control = permit.writer.control(ControlLimits::default())?;
        let mut records = Vec::new();
        for name in control.list_names(ControlNamespace::Catalog, 65536, 8 * 1024 * 1024)? {
            if !name.starts_with("layout-") {
                continue;
            }
            permit.context.checkpoint()?;
            let snapshot = control
                .read(ControlNamespace::Catalog, &name)?
                .context("derived adapter manifest disappeared")?;
            let layout: LayoutManifest = serde_json::from_slice(snapshot.bytes())?;
            layout.storage.validate()?;
            ensure!(
                name == manifest::layout_name(&layout.storage.key),
                "derived adapter identity differs"
            );
            if layout.packages.contains(&permit.target.key()) {
                records.push((name, snapshot.bytes().to_owned(), layout.storage));
            }
        }
        records
    };
    let mut records = records;
    records.sort_by(|left, right| left.2.allocation.cmp(&right.2.allocation));
    let mut guards = Vec::new();
    for (name, expected, storage) in records {
        let lock = StableLock::open_existing(
            permit.writer.build_handle(),
            &format!("{}.lock", storage.allocation),
            uid(),
            identity(storage.lock),
        )?;
        let held = lock
            .try_acquire(LeaseMode::Exclusive, permit.context.policy().deadline)?
            .context("derived adapter is in use; package removal refused")?;
        ensure_entry_identity(
            permit.writer.root_handle(),
            &storage.allocation,
            identity(storage.root_identity),
        )?;
        guards.push(AdapterInvalidation {
            name,
            expected,
            storage,
            lock,
            held,
        });
    }
    Ok(guards)
}

pub(crate) fn remove(context: &OperationContext, target: &PackageManifest) -> Result<()> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package removal"
    );
    target.storage.validate()?;
    let writer = StoreWriter::open_writer(
        &manifest::store_path(context),
        uid(),
        Default::default(),
        context.policy().deadline,
    )?;
    let lock = StableLock::open_existing(
        writer.build_handle(),
        &format!("{}.lock", target.storage.allocation),
        uid(),
        identity(target.storage.lock),
    )?;
    let held = lock
        .try_acquire(LeaseMode::Exclusive, context.policy().deadline)?
        .context("package is in use by an active Pakro operation")?;
    let control = writer.control(ControlLimits::default())?;
    let name = manifest::package_name(&target.key());
    let snapshot = control
        .read(ControlNamespace::Catalog, &name)?
        .context("package Ready manifest disappeared")?;
    ensure!(
        snapshot.bytes() == serde_json::to_vec(target)?,
        "package manifest changed before removal"
    );
    ensure_entry_identity(
        writer.root_handle(),
        &target.storage.allocation,
        identity(target.storage.root_identity),
    )?;
    let root = open_directory(writer.root_handle(), target.storage.allocation.as_str())?;
    drop(control);
    let permit = RemovalPermit {
        path: writer.root_path().join(&target.storage.allocation),
        context,
        writer,
        root,
        lock,
        held,
        target,
    };
    permit.check()?;
    ensure!(
        observe_tree_inventory(&permit, PrefixInventoryLimits::default())?
            == target.storage.inventory,
        "published package inventory is damaged; removal refused"
    );
    super::lease::validate_package_facts(&permit.root, target)?;
    // A selection retains this same package SH through its catalog commit.
    crate::programs::uninstall::reachability::ensure_unreferenced(context, target.key())?;
    let adapters = acquire_adapters(&permit)?;
    let mut control = permit.writer.control(ControlLimits::default())?;
    let latest = control
        .read(ControlNamespace::Catalog, &name)?
        .context("package Ready manifest disappeared")?;
    ensure!(
        latest.bytes() == snapshot.bytes(),
        "package manifest changed before removal"
    );
    crate::programs::uninstall::reachability::ensure_unreferenced_with_package_control(
        context,
        target.key(),
        &control,
    )?;
    permit.check()?;
    let mut adapter_snapshots = Vec::new();
    for adapter in &adapters {
        adapter.held.check_binding()?;
        adapter.lock.check_location(
            permit.writer.build_handle(),
            &format!("{}.lock", adapter.storage.allocation),
        )?;
        ensure_entry_identity(
            permit.writer.root_handle(),
            &adapter.storage.allocation,
            identity(adapter.storage.root_identity),
        )?;
        let snapshot = control
            .read(ControlNamespace::Catalog, &adapter.name)?
            .context("derived adapter disappeared before invalidation")?;
        ensure!(
            snapshot.bytes() == adapter.expected,
            "derived adapter changed before invalidation"
        );
        adapter_snapshots.push(snapshot);
    }
    // No selected project references this package (fresh check above), and
    // target EX excludes every active adapter lease and new publication.
    // Invalidate these derived Ready facts without deleting their contents.
    for snapshot in adapter_snapshots {
        control.remove(&snapshot)?;
    }
    control.remove(&latest)?;
    let captured = format!("removing-{}", uuid::Uuid::new_v4());
    ensure_entry_identity(
        permit.writer.root_handle(),
        &target.storage.allocation,
        identity(target.storage.root_identity),
    )?;
    pakro_store::control::rename_noclobber(
        permit.writer.root_handle(),
        std::ffi::OsStr::new(&target.storage.allocation),
        permit.writer.root_handle(),
        std::ffi::OsStr::new(&captured),
    )?;
    permit.writer.root_handle().sync_all()?;
    drop(control);
    crate::libraries::language::removal::remove_inventory(
        crate::libraries::language::removal::RemovalAuthority::Package(&permit),
        || Ok(()),
    )?;
    ensure_entry_identity(
        permit.writer.root_handle(),
        &captured,
        identity(target.storage.root_identity),
    )?;
    nix::unistd::unlinkat(
        permit.writer.root_handle(),
        captured.as_str(),
        nix::unistd::UnlinkatFlags::RemoveDir,
    )?;
    permit.writer.root_handle().sync_all()?;
    permit.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
