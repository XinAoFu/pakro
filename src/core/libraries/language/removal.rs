//! Private authority for deleting one published runtime. Stable protection and
//! lock registration outlive its content, so a stale handle cannot rebind it.
use super::manifest::{LanguageManifest, LockRegistration};
use super::repository::{PublishedLanguage, manifest_name, registration_name, uid};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, TreeInventory, TreeInventoryKind, observe_tree_inventory,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use nix::fcntl::AtFlags;
use nix::sys::stat::fstatat;
use nix::unistd::{UnlinkatFlags, unlinkat};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, StoreWriter, ensure_entry_identity,
    open_directory,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Component, Path};

/// This capability is constructed only after actual EX protection, identity
/// validation and a fresh read of the same authoritative project catalog.
pub(in crate::libraries) struct RemovalPermit<'a> {
    context: &'a OperationContext,
    writer: StoreWriter,
    lock: StableLock,
    held: ObjectLease,
    allocation: File,
    root: File,
    target: &'a PublishedLanguage,
    manifest: LanguageManifest,
}

impl RemovalPermit<'_> {
    fn check(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.writer.check_binding()?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive,
            "language removal lost EX protection"
        );
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("{}.lock", self.target.key().filename()),
        )?;
        ensure!(
            FileIdentity::of(&self.allocation)? == self.manifest.allocation.file_identity()
                && FileIdentity::of(&self.root)? == self.manifest.payload.file_identity(),
            "language removal identity changed"
        );
        Ok(())
    }
}
impl InventoryRoot for RemovalPermit<'_> {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        self.target.payload_root()
    }
    fn allocation_id(&self) -> &str {
        self.target
            .payload_root()
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .expect("validated key path")
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.check()
    }
}

fn acquire<'a>(
    context: &'a OperationContext,
    target: &'a PublishedLanguage,
) -> Result<RemovalPermit<'a>> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids language removal"
    );
    let writer = StoreWriter::open_writer(
        &context.requested_data_root().join("languages"),
        uid(),
        Default::default(),
        context.policy().deadline,
    )?;
    let key = target.key().filename();
    let registration = {
        let control = writer.control(ControlLimits::default())?;
        control
            .read(
                ControlNamespace::Ownership,
                &registration_name(target.spec()),
            )?
            .context("language protection registration is missing")?
    };
    let registered: LockRegistration = serde_json::from_slice(registration.bytes())?;
    ensure!(
        registered.schema == 1 && registered.key == target.key().fingerprint(),
        "language protection identity differs"
    );
    let lock = StableLock::open_existing(
        writer.build_handle(),
        &format!("{key}.lock"),
        uid(),
        registered.lock.file_identity(),
    )?;
    let control = writer.control(ControlLimits::default())?;
    // Never wait while holding a catalog lock. Active users produce a precise
    // refusal and keep their leases; no fallback to pathname deletion exists.
    let held = control
        .try_object(&key, &lock, LeaseMode::Exclusive)?
        .context("language is in use by an active Pakro operation")?;
    let snapshot = control
        .read(ControlNamespace::Catalog, &manifest_name(target.spec()))?
        .context("language Ready manifest disappeared")?;
    let current_registration = control
        .read(
            ControlNamespace::Ownership,
            &registration_name(target.spec()),
        )?
        .context("language protection registration disappeared")?;
    ensure!(
        current_registration.bytes() == registration.bytes(),
        "language protection registration changed"
    );
    let manifest: LanguageManifest = serde_json::from_slice(snapshot.bytes())?;
    super::publication::validate_manifest(target.spec(), &manifest)?;
    super::publication::validate_manifest_location(&manifest, target.payload_root())?;
    ensure!(
        registered.lock == manifest.lock,
        "language protection and manifest disagree"
    );
    ensure_entry_identity(
        writer.root_handle(),
        &key,
        manifest.allocation.file_identity(),
    )?;
    let allocation = open_directory(writer.root_handle(), key.as_str())?;
    ensure_entry_identity(&allocation, "payload", manifest.payload.file_identity())?;
    let root = open_directory(&allocation, "payload")?;
    drop(control);
    let permit = RemovalPermit {
        context,
        writer,
        lock,
        held,
        allocation,
        root,
        target,
        manifest,
    };
    permit.check()?;
    ensure!(
        observe_tree_inventory(&permit, PrefixInventoryLimits::default())?
            == permit.manifest.inventory,
        "published language inventory is damaged; removal refused"
    );
    // Selection commits retain SH on this exact object until their catalog
    // commit. Therefore no new valid reference can be committed past this EX.
    crate::programs::uninstall::reachability::ensure_unreferenced(
        context,
        target.key().fingerprint(),
    )?;
    permit.check()?;
    Ok(permit)
}

pub(crate) fn remove(context: &OperationContext, target: &PublishedLanguage) -> Result<()> {
    capture_and_remove(acquire(context, target)?, || Ok(()))
}

fn capture_and_remove(
    permit: RemovalPermit<'_>,
    before_entry: impl Fn() -> Result<()>,
) -> Result<()> {
    let context = permit.context;
    let target = permit.target;
    let key = target.key().filename();
    let captured = format!("removing-{}", uuid::Uuid::new_v4());
    let mut control = permit.writer.control(ControlLimits::default())?;
    let snapshot = control
        .read(ControlNamespace::Catalog, &manifest_name(target.spec()))?
        .context("language Ready manifest disappeared")?;
    let latest: LanguageManifest = serde_json::from_slice(snapshot.bytes())?;
    ensure!(
        serde_json::to_vec(&latest)? == serde_json::to_vec(&permit.manifest)?,
        "language manifest changed before removal"
    );
    crate::programs::uninstall::reachability::ensure_unreferenced(
        context,
        target.key().fingerprint(),
    )?;
    permit.check()?;
    // Revoke Ready before capture or unlink. Any partial deletion remains
    // explicitly unavailable and can never be acquired as a usable runtime.
    control.remove(&snapshot)?;
    ensure_entry_identity(
        permit.writer.root_handle(),
        &key,
        permit.manifest.allocation.file_identity(),
    )?;
    pakro_store::control::rename_noclobber(
        permit.writer.root_handle(),
        std::ffi::OsStr::new(&key),
        permit.writer.root_handle(),
        std::ffi::OsStr::new(&captured),
    )?;
    permit.writer.root_handle().sync_all()?;
    ensure_entry_identity(
        permit.writer.root_handle(),
        &captured,
        permit.manifest.allocation.file_identity(),
    )?;
    drop(control);
    remove_inventory(RemovalAuthority::Language(&permit), before_entry)?;
    ensure_entry_identity(
        &permit.allocation,
        "payload",
        permit.manifest.payload.file_identity(),
    )?;
    unlinkat(&permit.allocation, "payload", UnlinkatFlags::RemoveDir)?;
    permit.allocation.sync_all()?;
    ensure_entry_identity(
        permit.writer.root_handle(),
        &captured,
        permit.manifest.allocation.file_identity(),
    )?;
    unlinkat(
        permit.writer.root_handle(),
        captured.as_str(),
        UnlinkatFlags::RemoveDir,
    )?;
    permit.writer.root_handle().sync_all()?;
    permit.check()?;
    Ok(())
}

/// Only these unforgeable permits can enter the shared fd-relative kernel.
/// Fields and constructors stay private to each object-removal module.
pub(in crate::libraries) enum RemovalAuthority<'a, 'b> {
    Language(&'a RemovalPermit<'b>),
    Package(&'a crate::libraries::package::removal::RemovalPermit<'b>),
}
impl RemovalAuthority<'_, '_> {
    fn root(&self) -> &File {
        match self {
            Self::Language(permit) => &permit.root,
            Self::Package(permit) => permit.removal_root(),
        }
    }
    fn inventory(&self) -> &TreeInventory {
        match self {
            Self::Language(permit) => &permit.manifest.inventory,
            Self::Package(permit) => permit.removal_inventory(),
        }
    }
    fn check(&self) -> Result<()> {
        match self {
            Self::Language(permit) => permit.check(),
            Self::Package(permit) => permit.check(),
        }
    }
}
pub(in crate::libraries) fn remove_inventory(
    authority: RemovalAuthority<'_, '_>,
    before_entry: impl Fn() -> Result<()>,
) -> Result<()> {
    let root = authority.root();
    let inventory = authority.inventory();
    let entries: BTreeMap<_, _> = inventory
        .entries()
        .iter()
        .map(|entry| (entry.relative.as_path(), entry))
        .collect();
    let mut order: Vec<_> = inventory.entries().iter().collect();
    order.sort_by_key(|entry| std::cmp::Reverse(entry.relative.components().count()));
    for entry in order {
        authority.check()?;
        before_entry()?;
        let mut parent = root.try_clone()?;
        let mut partial = std::path::PathBuf::new();
        for component in entry
            .relative
            .parent()
            .context("removal entry has no parent")?
            .components()
        {
            let Component::Normal(name) = component else {
                eyre::bail!("unsafe removal path")
            };
            partial.push(name);
            let fact = entries
                .get(partial.as_path())
                .context("removal parent is untracked")?;
            ensure!(
                matches!(fact.kind, TreeInventoryKind::Directory),
                "removal parent is not a directory"
            );
            let next = open_directory(&parent, name)?;
            ensure!(
                FileIdentity::of(&next)?
                    == FileIdentity {
                        device: fact.identity[0] as _,
                        inode: fact.identity[1] as _
                    },
                "removal parent identity changed"
            );
            parent = next;
        }
        let name = entry
            .relative
            .file_name()
            .context("removal entry has no name")?;
        let stat = fstatat(&parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
        ensure!(
            [stat.st_dev as u64, stat.st_ino as u64] == entry.identity,
            "removal entry identity changed"
        );
        let flags = if matches!(entry.kind, TreeInventoryKind::Directory) {
            UnlinkatFlags::RemoveDir
        } else {
            UnlinkatFlags::NoRemoveDir
        };
        unlinkat(&parent, name, flags)?;
        parent.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
