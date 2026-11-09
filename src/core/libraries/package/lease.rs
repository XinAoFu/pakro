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
    /// Query path: Ready manifest and physical binding only. The complete
    /// member hash stays on install, publication, materialization and deletion.
    pub(crate) fn check_metadata(&self) -> Result<()> {
        self.check_manifest()
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
    fn verify_metadata(self) -> Result<Arc<ObjectProtection>> {
        self.protection.check_metadata()?;
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
    /// Reusing a held member must preserve the same library authority, not
    /// merely an equal portable object key from another managed data root.
    pub(crate) fn ensure_library_context(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.protection.check_manifest()?;
        ensure!(
            self.protection.context.requested_data_root() == context.requested_data_root()
                && self.protection.read.root_path() == manifest::store_path(context)
                && serde_json::to_vec(&self.manifest)? == self.protection.bytes,
            "held package does not belong to this operation's library context"
        );
        let expected = open_trusted_store_with_deadline(
            &manifest::store_path(context),
            uid(),
            false,
            context
                .policy()
                .deadline
                .min(self.protection.context.policy().deadline),
        )?;
        ensure!(
            FileIdentity::of(&expected)? == FileIdentity::of(&self.protection.store)?,
            "held package store identity differs from the requested library"
        );
        self.protection.check_manifest()?;
        context.checkpoint()?;
        Ok(())
    }
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
    /// Query path: Ready manifest and physical binding, no member re-hash.
    pub(crate) fn check_metadata(&self) -> Result<()> {
        self.protection.check_metadata()?;
        validate_package_facts(&self.protection.root, &self.manifest)
    }

    /// A read capability for one exact regular member of this held immutable
    /// inventory. Callers retain the lease, bracket copying with full checks,
    /// and verify the streamed hash against this same inventory.
    pub(crate) fn open_inventory_file(&self, relative: &Path) -> Result<File> {
        use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;
        self.protection.check_manifest()?;
        let limits = PrefixInventoryLimits::default();
        ensure!(
            !relative.as_os_str().is_empty()
                && !relative.is_absolute()
                && relative.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
            "inventory file path is not a bounded relative member"
        );
        let components = relative
            .components()
            .map(|part| match part {
                std::path::Component::Normal(name) => Ok(name),
                _ => Err(eyre::eyre!(
                    "inventory file path contains non-normal components"
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            !components.is_empty() && components.len() <= limits.max_components,
            "inventory file component budget exceeded"
        );
        let canonical: PathBuf = components.iter().collect();
        ensure!(
            canonical.as_os_str() == relative.as_os_str(),
            "inventory file path is not canonical"
        );
        let entries = self.manifest.storage.inventory.entries();
        // Lease admission compared this complete ordered inventory with an
        // actual tree walk; this is an index over those existing facts.
        let find = |path: &Path| {
            entries
                .binary_search_by(|entry| entry.relative.as_path().cmp(path))
                .ok()
                .map(|index| &entries[index])
                .context("requested file is absent from the held inventory")
        };
        let expected = find(relative)?;
        let TreeInventoryKind::File { size, .. } = &expected.kind else {
            eyre::bail!("requested inventory member is not a regular file")
        };
        let mut directory = self.protection.root.try_clone()?;
        let mut path = PathBuf::new();
        for name in &components[..components.len() - 1] {
            path.push(name);
            let entry = find(&path)?;
            ensure!(
                matches!(entry.kind, TreeInventoryKind::Directory),
                "inventory ancestor is not a directory"
            );
            let opened = open_directory(&directory, *name)?;
            let metadata = opened.metadata()?;
            let actual = FileIdentity::of(&opened)?;
            ensure!(
                [actual.device as u64, actual.inode as u64] == entry.identity
                    && metadata.uid() == entry.uid
                    && entry.uid == uid()
                    && metadata.mode() & 0o7777 == entry.mode,
                "inventory directory identity, owner or mode changed"
            );
            validate_owned_directory(&opened, uid())?;
            ensure_entry_identity(
                &directory,
                name.to_str().context("inventory path is not UTF-8")?,
                actual,
            )?;
            directory = opened;
        }
        let name = components.last().expect("nonempty inventory path");
        let file = File::from(openat(
            &directory,
            *name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        let actual = FileIdentity::of(&file)?;
        ensure!(
            metadata.is_file()
                && metadata.nlink() == 1
                && [actual.device as u64, actual.inode as u64] == expected.identity
                && metadata.uid() == expected.uid
                && expected.uid == uid()
                && metadata.mode() & 0o7777 == expected.mode
                && metadata.len() == *size,
            "inventory file identity, owner, mode, links or size changed"
        );
        ensure_entry_identity(
            &directory,
            name.to_str().context("inventory filename is not UTF-8")?,
            actual,
        )?;
        self.protection.check_manifest()?;
        Ok(file)
    }

    /// Read only this wheel's original METADATA through the held allocation
    /// FD. The complete inventory remains mandatory on lease admission and
    /// layout validation; this additionally verifies the exact member bytes.
    pub(crate) fn read_wheel_metadata(&self, max_bytes: usize) -> Result<Vec<u8>> {
        use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
        use sha2::{Digest as _, Sha256};
        let max_bytes =
            max_bytes.min(crate::packages::python_wheel::WheelLimits::default().max_metadata_bytes);
        ensure!(
            max_bytes > 0,
            "installed wheel METADATA read budget is empty"
        );
        self.protection.check_manifest()?;
        let mut candidates = self
            .manifest
            .storage
            .inventory
            .entries()
            .iter()
            .filter(|entry| {
                let components = entry.relative.components().collect::<Vec<_>>();
                matches!(components.as_slice(), [
                std::path::Component::Normal(site),
                std::path::Component::Normal(info),
                std::path::Component::Normal(metadata),
            ] if *site == "site-packages" && *metadata == "METADATA"
                && info.to_str().is_some_and(|name| name.ends_with(".dist-info")))
            });
        let entry = candidates
            .next()
            .context("published wheel METADATA is missing")?;
        ensure!(
            candidates.next().is_none(),
            "published wheel has several METADATA members"
        );
        let TreeInventoryKind::File { size, sha256 } = &entry.kind else {
            eyre::bail!("published wheel METADATA is not a regular file")
        };
        ensure!(
            *size <= max_bytes as u64,
            "installed wheel METADATA exceeds its bounded read budget"
        );
        let mut file = self.open_inventory_file(&entry.relative)?;
        let mut bytes = Vec::new();
        file.by_ref().take(*size + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == *size && <[u8; 32]>::from(Sha256::digest(&bytes)) == *sha256,
            "published wheel METADATA bytes differ from their immutable inventory"
        );
        ensure!(
            file.metadata()?.len() == *size,
            "published wheel METADATA size changed during read"
        );
        self.protection.check_manifest()?;
        Ok(bytes)
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
    pub(crate) fn activation_plan(&self) -> Result<Option<crate::packages::conda_scripts::activation::ActivationPlan>> {
        use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
        use nix::{fcntl::{OFlag, openat}, sys::stat::Mode};
        use sha2::{Digest as _, Sha256};
        if !self.manifest.activation { return Ok(None); }
        self.protection.check_manifest()?;
        let entries = self.manifest.storage.inventory.entries();
        let directory = entries.iter().find(|entry| entry.relative == Path::new("bootstrap"))
            .context("activation plan directory is missing")?;
        let expected = entries.iter().find(|entry| entry.relative == Path::new("bootstrap/.pakro-conda-activation.json"))
            .context("activation plan is missing")?;
        let TreeInventoryKind::File { size, sha256 } = &expected.kind else { eyre::bail!("activation plan is not a file") };
        ensure!(matches!(directory.kind, TreeInventoryKind::Directory) && *size <= 1024 * 1024,
            "activation plan inventory is invalid");
        let parent = open_directory(&self.protection.root, "bootstrap")?;
        let meta = parent.metadata()?;
        ensure!([meta.dev(), meta.ino()] == directory.identity && meta.uid() == uid() && meta.mode() & 0o7777 == directory.mode,
            "activation plan directory changed");
        let mut file = File::from(openat(&parent, ".pakro-conda-activation.json",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK, Mode::empty())?);
        let meta = file.metadata()?;
        ensure!(meta.is_file() && meta.nlink() == 1 && [meta.dev(), meta.ino()] == expected.identity
            && meta.uid() == uid() && meta.mode() & 0o7777 == expected.mode && meta.len() == *size,
            "activation plan file changed");
        let mut bytes = Vec::new();
        file.by_ref().take(*size + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == *size && <[u8; 32]>::from(Sha256::digest(&bytes)) == *sha256,
            "activation plan bytes changed");
        let plan: crate::packages::conda_scripts::activation::ActivationPlan = serde_json::from_slice(&bytes)?;
        plan.validate()?;
        self.protection.check_manifest()?;
        Ok(Some(plan))
    }
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
    /// Query path: Ready manifests and physical bindings only.
    pub(crate) fn check_metadata(&self) -> Result<()> {
        self.language.check_metadata()?;
        for p in &self.packages {
            p.protection.check_metadata()?;
        }
        self.protection.check_metadata()?;
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

/// Read-only query path: Ready manifest and physical binding, without
/// re-hashing the published member tree.
pub(crate) fn acquire_package_for_query(
    context: &OperationContext,
    key: [u8; 32],
) -> Result<Option<PackageLease>> {
    pin_package(context, key)?
        .map(PendingPackage::verify_metadata)
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
    fn verify_metadata(self) -> Result<PackageLease> {
        let protection = self.protection.verify_metadata()?;
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
    value.validate_metadata()?;
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
    acquire_layout_inner(context, key, None, &[])
}

/// Capture the layout while continuously sharing this operation's actual
/// already-held members. Missing members still take ordinary real leases.
pub(crate) fn acquire_layout_with_held_inputs(
    context: &OperationContext,
    key: [u8; 32],
    language: &LanguageLease,
    packages: &[PackageLease],
) -> Result<Option<LayoutLease>> {
    acquire_layout_inner(context, key, Some(language), packages)
}

fn acquire_layout_inner(
    context: &OperationContext,
    key: [u8; 32],
    held_language: Option<&LanguageLease>,
    held_packages: &[PackageLease],
) -> Result<Option<LayoutLease>> {
    acquire_layout_verified(context, key, held_language, held_packages, true)
}

/// Read-only query path for the layout and every member it carries: Ready
/// manifests and physical bindings, no member re-hash. Install and deletion
/// keep the complete inventory admission.
pub(crate) fn acquire_layout_for_query(
    context: &OperationContext,
    key: [u8; 32],
) -> Result<Option<LayoutLease>> {
    acquire_layout_verified(context, key, None, &[], false)
}

fn acquire_layout_verified(
    context: &OperationContext,
    key: [u8; 32],
    held_language: Option<&LanguageLease>,
    held_packages: &[PackageLease],
    full_inventory: bool,
) -> Result<Option<LayoutLease>> {
    context.checkpoint()?;
    if let Some(language) = held_language {
        language.ensure_library_context(context)?;
    }
    ensure!(
        held_packages.len() <= 65536,
        "held layout member budget exceeded"
    );
    let mut supplied = std::collections::BTreeMap::new();
    for package in held_packages {
        package.ensure_library_context(context)?;
        ensure!(
            supplied.insert(package.manifest().key(), package).is_none(),
            "held layout repeats a package member"
        );
    }
    let name = manifest::layout_name(&key);
    let Some(value): Option<LayoutManifest> = manifest::read_json(context, &name)? else {
        return Ok(None);
    };
    ensure!(value.storage.key == key, "layout manifest key differs");
    value.validate_metadata()?;
    ensure!(
        supplied
            .keys()
            .all(|key| value.packages.binary_search(key).is_ok()),
        "held package is not referenced by the actual layout manifest"
    );
    let language = if let Some(language) = held_language {
        ensure!(
            language.key().fingerprint() == value.language,
            "held language differs from the actual layout manifest"
        );
        language.clone()
    } else {
        let languages =
            crate::libraries::language::repository::LanguageRepository::open_readonly(context)?;
        let found = languages
            .enumerate_published()
            .map_err(|e| eyre::eyre!("language enumeration failed: {e:?}"))?;
        let descriptor = found
            .iter()
            .find(|p| p.key().fingerprint() == value.language)
            .context("layout language is missing")?;
        languages
            .acquire_by_key_verified(&descriptor.key(), full_inventory)?
            .context("layout language is not Ready")?
    };
    enum Member {
        Held(PackageLease),
        New(PendingPackage),
    }
    let mut members = Vec::new();
    for package in &value.packages {
        if let Some(held) = supplied.get(package) {
            ensure!(
                (matches!(value.recipe, 5 | 6)
                    && (value.recipe == 6 || held.manifest().conda.is_some())
                    && super::conda::layout_member_matches(&language, held.manifest()))
                    || (!matches!(value.recipe, 5 | 6) && held.manifest().language == Some(value.language)),
                "held package language differs from the layout"
            );
            members.push(Member::Held((**held).clone()));
        } else {
            members.push(Member::New(
                pin_package(context, *package)?.context("layout package is missing")?,
            ));
        }
    }
    let protection = pin_object(context, &name, &value.storage, &serde_json::to_vec(&value)?)?;
    // Capture the entire group under its real SH guards first. A language
    // acquired above has already completed its full inventory; only internal
    // group pinning occurred since then. Supplied held inputs still need their
    // complete admission check here.
    if !full_inventory {
        language.check_metadata()?;
    } else if held_language.is_some() {
        language.check_binding()?;
    } else {
        language.check_held_binding()?;
    }
    let packages = members
        .into_iter()
        .map(|member| match member {
            Member::Held(package) => {
                if full_inventory {
                    package.check_binding()?;
                } else {
                    package.protection.check_metadata()?;
                }
                Ok(package)
            }
            Member::New(package) => {
                if full_inventory {
                    package.verify()
                } else {
                    package.verify_metadata()
                }
            }
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        packages
            .iter()
            .map(|package| package.manifest().key())
            .eq(value.packages.iter().copied())
            && packages
                .iter()
                .all(|package| (matches!(value.recipe, 5 | 6)
                    && (value.recipe == 6 || package.manifest().conda.is_some())
                    && super::conda::layout_member_matches(&language, package.manifest()))
                    || (!matches!(value.recipe, 5 | 6) && package.manifest().language == Some(value.language))),
        "protected layout members differ from its complete manifest reference set"
    );
    let protection = if full_inventory {
        protection.verify()?
    } else {
        protection.verify_metadata()?
    };
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
