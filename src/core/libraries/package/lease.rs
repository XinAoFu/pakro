//! Real stable-lock leases for published package and layout allocations.
use super::manifest::{self, LayoutManifest, PackageManifest, StorageManifest};
use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, observe_tree_inventory,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, ReadStore, RootLimits, ensure_entry_identity,
    open_directory, open_trusted_store_with_deadline, validate_owned_directory,
};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
thread_local! {
    static STORE_BINDING_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FULL_INVENTORY_CHECKS: std::cell::RefCell<std::collections::BTreeMap<[u8; 32], usize>> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}
#[cfg(test)]
fn store_binding_checks() -> usize {
    STORE_BINDING_CHECKS.with(|value| value.get())
}
#[cfg(test)]
pub(crate) fn full_inventory_checks(key: [u8; 32]) -> usize {
    FULL_INVENTORY_CHECKS.with(|value| value.borrow().get(&key).copied().unwrap_or(0))
}

pub(crate) fn uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
pub(crate) fn pair(file: &File) -> Result<[u64; 2]> {
    let id = FileIdentity::of(file)?;
    Ok([id.device as u64, id.inode as u64])
}
pub(crate) fn identity(pair: [u64; 2]) -> FileIdentity {
    FileIdentity {
        device: pair[0] as _,
        inode: pair[1] as _,
    }
}

pub(crate) struct ObjectProtection {
    context: OperationContext,
    read: ReadStore,
    store: File,
    root: File,
    build: File,
    lock: StableLock,
    held: ObjectLease,
    pub(crate) storage: StorageManifest,
    path: PathBuf,
    name: String,
    bytes: Vec<u8>,
}
impl ObjectProtection {
    fn shallow(&self) -> Result<()> {
        self.context.checkpoint()?;
        #[cfg(test)]
        STORE_BINDING_CHECKS.with(|value| value.set(value.get() + 1));
        self.read.check_binding()?;
        self.check_held_tree()
    }
    fn check_held_tree(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.held.check_binding()?;
        self.lock
            .check_location(&self.build, &format!("{}.lock", self.storage.allocation))?;
        ensure!(
            self.held.mode() == LeaseMode::Shared && self.held.identity() == self.lock.identity(),
            "package lease protection changed"
        );
        ensure_entry_identity(
            &self.store,
            &self.storage.allocation,
            identity(self.storage.root_identity),
        )?;
        ensure!(
            pair(&self.root)? == self.storage.root_identity,
            "package allocation identity changed"
        );
        validate_owned_directory(&self.root, uid())?;
        Ok(())
    }
    pub(crate) fn check_binding(&self) -> Result<()> {
        self.check_binding_with_after_walk(|| Ok(()))
    }
    fn check_binding_with_after_walk(&self, after_walk: impl FnOnce() -> Result<()>) -> Result<()> {
        self.check_manifest()?;
        #[cfg(test)]
        FULL_INVENTORY_CHECKS.with(|value| {
            *value.borrow_mut().entry(self.storage.key).or_insert(0) += 1;
        });
        ensure!(
            observe_tree_inventory(&HeldObjectTreeRoot(self), PrefixInventoryLimits::default())?
                == self.storage.inventory,
            "published package inventory changed"
        );
        after_walk()?;
        self.check_manifest()
    }
    fn check_manifest(&self) -> Result<()> {
        self.shallow()?;
        let control = self.read.control(ControlLimits::default())?;
        let current = control
            .read(ControlNamespace::Catalog, &self.name)?
            .context("package Ready manifest disappeared")?;
        ensure!(
            current.bytes() == self.bytes,
            "package Ready manifest changed"
        );
        drop(control);
        self.shallow()
    }
    pub(crate) fn root(&self) -> &Path {
        &self.path
    }
}

/// The full store and Ready manifest checks bracket this one anchored walk.
/// Every member still uses the original root FD and actual object SH, without
/// reopening the complete store namespace for each payload read block.
struct HeldObjectTreeRoot<'a>(&'a ObjectProtection);
impl InventoryRoot for HeldObjectTreeRoot<'_> {
    fn root_file(&self) -> &File {
        &self.0.root
    }
    fn root_path(&self) -> &Path {
        &self.0.path
    }
    fn allocation_id(&self) -> &str {
        &self.0.storage.allocation
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.0.check_held_tree()
    }
}
impl InventoryRoot for ObjectProtection {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        &self.storage.allocation
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.shallow()
    }
}

pub(crate) fn acquire_object(
    context: &OperationContext,
    name: &str,
    storage: &StorageManifest,
    expected: &[u8],
) -> Result<Arc<ObjectProtection>> {
    pin_object(context, name, storage, expected)?.verify()
}

/// A real pinned owner that has not yet completed its full inventory check.
/// It remains private and cannot expose a usable lease before verification.
struct PendingObjectProtection {
    protection: Arc<ObjectProtection>,
}
impl PendingObjectProtection {
    fn verify(self) -> Result<Arc<ObjectProtection>> {
        self.protection.check_binding()?;
        Ok(self.protection)
    }
}
fn pin_object(
    context: &OperationContext,
    name: &str,
    storage: &StorageManifest,
    expected: &[u8],
) -> Result<PendingObjectProtection> {
    storage.validate()?;
    let read = ReadStore::open_readonly(
        &manifest::store_path(context),
        uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    .context("package library disappeared")?;
    let store = open_trusted_store_with_deadline(
        read.root_path(),
        uid(),
        false,
        context.policy().deadline,
    )?;
    let leases = open_directory(&store, ".leases")?;
    let build = open_directory(&leases, "build")?;
    let lock = StableLock::open_existing(
        &build,
        &format!("{}.lock", storage.allocation),
        uid(),
        identity(storage.lock),
    )?;
    let held = lock.acquire(LeaseMode::Shared, context.policy().deadline)?;
    let control = read.control(ControlLimits::default())?;
    let current = control
        .read(ControlNamespace::Catalog, name)?
        .context("package Ready manifest disappeared while acquiring")?;
    ensure!(
        current.bytes() == expected,
        "package Ready manifest changed while acquiring"
    );
    drop(control);
    let root = open_directory(&store, storage.allocation.as_str())?;
    let path = read.root_path().join(&storage.allocation);
    let result = Arc::new(ObjectProtection {
        context: context.clone(),
        read,
        store,
        root,
        build,
        lock,
        held,
        storage: storage.clone(),
        path,
        name: name.into(),
        bytes: expected.to_vec(),
    });
    result.shallow()?;
    Ok(PendingObjectProtection { protection: result })
}
/// Read the one reserved facts member through the caller's already pinned
/// allocation. Callers retain its actual SH/EX, or the catalog scope that
/// prevents Ready revocation/capture; this helper never acquires a new lock.
pub(crate) fn validate_package_facts(root: &File, manifest: &PackageManifest) -> Result<()> {
    use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
    use sha2::{Digest as _, Sha256};
    ensure!(
        pair(root)? == manifest.storage.root_identity,
        "package facts root identity changed"
    );
    validate_owned_directory(root, uid())?;
    let entry = manifest
        .storage
        .inventory
        .entries()
        .iter()
        .find(|entry| entry.relative == Path::new(manifest::PACKAGE_FACTS_FILE))
        .context("package facts are missing from its complete inventory")?;
    let TreeInventoryKind::File { size, sha256 } = &entry.kind else {
        eyre::bail!("package facts inventory member is not a regular file")
    };
    let limit = ControlLimits::default().max_file_bytes;
    ensure!(
        *size <= limit,
        "package facts exceed their bounded read budget"
    );
    let mut file = pakro_store::control::open_existing_readonly_regular(
        root,
        manifest::PACKAGE_FACTS_FILE,
        uid(),
    )?;
    let id = FileIdentity::of(&file)?;
    ensure!(
        [id.device as u64, id.inode as u64] == entry.identity
            && file.metadata()?.len() == *size
            && file.metadata()?.mode() & 0o7777 == 0o600,
        "package facts member identity, mode or size changed"
    );
    ensure_entry_identity(root, manifest::PACKAGE_FACTS_FILE, id)?;
    let mut bytes = Vec::new();
    file.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == *size && <[u8; 32]>::from(Sha256::digest(&bytes)) == *sha256,
        "package facts member bytes changed"
    );
    ensure!(
        bytes
            == manifest::package_facts_bytes(
                &manifest.facts,
                &manifest.language,
                &manifest.filename
            )?,
        "package manifest facts disagree with the immutable published tree"
    );
    ensure_entry_identity(root, manifest::PACKAGE_FACTS_FILE, id)?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct PackageLease {
    pub(super) protection: Arc<ObjectProtection>,
    pub(super) manifest: PackageManifest,
}
impl PackageLease {
    pub(crate) fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
    pub(crate) fn root(&self) -> &Path {
        self.protection.root()
    }
    pub(crate) fn site_packages(&self) -> PathBuf {
        self.root().join("site-packages")
    }
    pub(crate) fn check_binding(&self) -> Result<()> {
        self.protection.check_binding()?;
        validate_package_facts(&self.protection.root, &self.manifest)
    }
}
#[derive(Clone)]
pub(crate) struct LayoutLease {
    pub(super) protection: Arc<ObjectProtection>,
    pub(super) manifest: LayoutManifest,
    pub(super) language: LanguageLease,
    pub(super) packages: Vec<PackageLease>,
}
impl LayoutLease {
    pub(crate) fn key(&self) -> [u8; 32] {
        self.manifest.storage.key
    }
    pub(crate) fn language(&self) -> [u8; 32] {
        self.manifest.language
    }
    pub(crate) fn graph(&self) -> [u8; 32] {
        self.manifest.graph
    }
    pub(crate) fn prefix(&self) -> &Path {
        self.protection.root()
    }
    pub(crate) fn bin(&self) -> PathBuf {
        self.prefix().join("bin")
    }
    pub(crate) fn packages(&self) -> &[PackageLease] {
        &self.packages
    }
    pub(crate) fn language_lease(&self) -> &LanguageLease {
        &self.language
    }
    pub(crate) fn check_binding(&self) -> Result<()> {
        self.language.check_binding()?;
        for p in &self.packages {
            p.check_binding()?;
        }
        self.protection.check_binding()?;
        super::layouts::projection::validate(
            &self.protection.context,
            &self.manifest,
            &self.packages,
        )
    }
}
pub(crate) fn acquire_package(
    context: &OperationContext,
    key: [u8; 32],
) -> Result<Option<PackageLease>> {
    pin_package(context, key)?
        .map(PendingPackage::verify)
        .transpose()
}

struct PendingPackage {
    protection: PendingObjectProtection,
    manifest: PackageManifest,
}
impl PendingPackage {
    fn verify(self) -> Result<PackageLease> {
        let protection = self.protection.verify()?;
        validate_package_facts(&protection.root, &self.manifest)?;
        Ok(PackageLease {
            protection,
            manifest: self.manifest,
        })
    }
}
fn pin_package(context: &OperationContext, key: [u8; 32]) -> Result<Option<PendingPackage>> {
    let name = manifest::package_name(&key);
    let Some(value): Option<PackageManifest> = manifest::read_json(context, &name)? else {
        return Ok(None);
    };
    ensure!(
        value.facts.key == key && value.storage.key == key,
        "package manifest key differs"
    );
    let artifact: pakro_store::Digest = value.facts.artifact.parse()?;
    ensure!(
        artifact.to_string() == value.facts.artifact
            && !value.facts.source.is_empty()
            && value.facts.source.len() <= 65536
            && value.facts.dependencies.len() <= 65536,
        "package source facts are invalid"
    );
    ensure!(
        value
            .facts
            .dependencies
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "package dependencies are not canonical"
    );
    let bytes = serde_json::to_vec(&value)?;
    let protection = pin_object(context, &name, &value.storage, &bytes)?;
    Ok(Some(PendingPackage {
        protection,
        manifest: value,
    }))
}
pub(crate) fn acquire_layout(
    context: &OperationContext,
    key: [u8; 32],
) -> Result<Option<LayoutLease>> {
    let name = manifest::layout_name(&key);
    let Some(value): Option<LayoutManifest> = manifest::read_json(context, &name)? else {
        return Ok(None);
    };
    ensure!(value.storage.key == key, "layout manifest key differs");
    ensure!(
        key == super::layouts::layout_key_for(value.recipe, value.language, value.graph)?
            && !value.packages.is_empty()
            && value.packages.len() <= 65536
            && value.packages.windows(2).all(|pair| pair[0] < pair[1]),
        "layout identity or package list differs"
    );
    let languages =
        crate::libraries::language::repository::LanguageRepository::open_readonly(context)?;
    let found = languages
        .enumerate_published()
        .map_err(|e| eyre::eyre!("language enumeration failed: {e:?}"))?;
    let descriptor = found
        .iter()
        .find(|p| p.key().fingerprint() == value.language)
        .context("layout language is missing")?;
    let language = languages
        .acquire_by_key(&descriptor.key())?
        .context("layout language is not Ready")?;
    let mut pending = Vec::new();
    for package in &value.packages {
        pending.push(pin_package(context, *package)?.context("layout package is missing")?);
    }
    let protection = pin_object(context, &name, &value.storage, &serde_json::to_vec(&value)?)?;
    // Capture the entire group under its real SH guards first. Complete every
    // inventory and reserved-facts check before exposing any result to callers.
    language.check_binding()?;
    let packages = pending
        .into_iter()
        .map(PendingPackage::verify)
        .collect::<Result<Vec<_>>>()?;
    let protection = protection.verify()?;
    super::layouts::projection::validate(context, &value, &packages)?;
    let result = LayoutLease {
        protection,
        manifest: value,
        language,
        packages,
    };
    Ok(Some(result))
}

#[cfg(test)]
mod tests;
