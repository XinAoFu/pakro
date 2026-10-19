//! Opaque protection retained by each shared runtime handle.

use super::manifest::LanguageManifest;
use super::repository::{LanguageInstallSpec, manifest_name, uid};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, observe_tree_inventory,
};
use crate::packages::python_resolution::source::locked_graph::FrozenRegistryTarget;
use crate::programs::OperationContext;
use crate::python_runtime::{PythonRuntimeDescriptor, PythonRuntimeHandle, RuntimeProtection};
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, ControlSnapshot, FileIdentity, ReadStore,
    ensure_entry_identity, validate_owned_directory,
};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct LanguageLease {
    pub(super) inner: Arc<LeaseInner>,
}
pub(super) struct LeaseInner {
    pub context: OperationContext,
    pub read: ReadStore,
    pub store: File,
    pub build: File,
    pub catalog: File,
    pub manifest_pin: File,
    pub allocation: File,
    pub root: File,
    pub path: PathBuf,
    pub key_name: String,
    pub spec: LanguageInstallSpec,
    pub lock: StableLock,
    pub held: ObjectLease,
    pub snapshot: ControlSnapshot,
    pub manifest: LanguageManifest,
    pub target: FrozenRegistryTarget,
}
impl std::fmt::Debug for LeaseInner {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("LanguageProtection")
            .field("key", &self.key_name)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for LanguageLease {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_tuple("LanguageLease").field(&self.inner).finish()
    }
}
impl LeaseInner {
    pub(super) fn check_shallow(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.read.check_binding()?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Shared && self.held.identity() == self.lock.identity(),
            "language use protection changed"
        );
        self.lock
            .check_location(&self.build, &format!("{}.lock", self.key_name))?;
        ensure_entry_identity(
            &self.store,
            &self.key_name,
            self.manifest.allocation.file_identity(),
        )?;
        ensure_entry_identity(
            &self.allocation,
            "payload",
            self.manifest.payload.file_identity(),
        )?;
        ensure!(
            FileIdentity::of(&self.allocation)? == self.manifest.allocation.file_identity()
                && FileIdentity::of(&self.root)? == self.manifest.payload.file_identity(),
            "language payload identity changed"
        );
        validate_owned_directory(&self.allocation, uid())?;
        validate_owned_directory(&self.root, uid())?;
        ensure!(
            self.allocation.metadata()?.mode() & 0o7777 == 0o700
                && self.root.metadata()?.mode() & 0o7777 == 0o700,
            "published language allocation permissions changed"
        );
        ensure_entry_identity(
            &self.catalog,
            &manifest_name(&self.spec),
            FileIdentity::of(&self.manifest_pin)?,
        )?;
        self.context.checkpoint()?;
        Ok(())
    }
    fn check_manifest(&self) -> Result<()> {
        self.check_shallow()?;
        let control = self.read.control(ControlLimits::default())?;
        let snapshot = control
            .read(ControlNamespace::Catalog, &manifest_name(&self.spec))?
            .context("language Ready manifest disappeared")?;
        ensure!(
            snapshot.sha256() == self.snapshot.sha256()
                && snapshot.bytes() == self.snapshot.bytes(),
            "language Ready manifest changed while leased"
        );
        self.check_shallow()
    }
    pub(super) fn check_full(&self) -> Result<()> {
        self.check_manifest()?;
        ensure!(
            observe_tree_inventory(self, PrefixInventoryLimits::default())?
                == self.manifest.inventory,
            "published language inventory changed"
        );
        self.check_manifest()
    }
}
impl InventoryRoot for LeaseInner {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        &self.key_name
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.check_shallow()
    }
}
impl RuntimeProtection for LeaseInner {
    fn check_binding(&self) -> Result<()> {
        self.check_full()
    }
}
impl LanguageLease {
    pub(crate) fn root(&self) -> &Path {
        &self.inner.path
    }
    pub(crate) fn entrypoint(&self) -> &Path {
        &self.inner.manifest.descriptor.executable
    }
    pub(crate) fn descriptor(&self) -> &PythonRuntimeDescriptor {
        &self.inner.manifest.descriptor
    }
    pub(crate) fn target(&self) -> &FrozenRegistryTarget {
        &self.inner.target
    }
    pub(crate) fn handle(&self) -> Result<PythonRuntimeHandle> {
        self.check_binding()?;
        let expected = self.root().join(&self.inner.manifest.entrypoint);
        ensure!(
            expected == self.entrypoint(),
            "language entrypoint binding differs from its manifest"
        );
        ensure!(
            self.inner
                .manifest
                .inventory
                .contains_file(&self.inner.manifest.entrypoint),
            "language entrypoint is absent from its complete inventory"
        );
        PythonRuntimeHandle::from_protected_executable(&expected, self.inner.clone())
    }
    pub(crate) fn check_binding(&self) -> Result<()> {
        self.inner.check_full()
    }
}
