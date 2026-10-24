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

#[cfg(test)]
thread_local! {
    static FULL_INVENTORY_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn full_inventory_checks() -> usize {
    FULL_INVENTORY_CHECKS.with(|count| count.get())
}

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
        self.check_held_tree()
    }

    // The complete namespace is checked around the one anchored inventory
    // walk. Inside it, every IO keeps these original fds and the real lease;
    // no member lookup reopens a payload through a mutable absolute path.
    fn check_held_tree(&self) -> Result<()> {
        self.context.checkpoint()?;
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
        self.check_full_with_after_walk(|| Ok(()))
    }

    fn check_full_with_after_walk(&self, after_walk: impl FnOnce() -> Result<()>) -> Result<()> {
        self.check_manifest()?;
        #[cfg(test)]
        FULL_INVENTORY_CHECKS.with(|count| count.set(count.get() + 1));
        ensure!(
            observe_tree_inventory(&HeldTreeRoot(self), PrefixInventoryLimits::default())?
                == self.manifest.inventory,
            "published language inventory changed"
        );
        after_walk()?;
        self.check_manifest()
    }
}

/// A private view of this already pinned tree, valid only between the full
/// store/manifest checks in check_full. The existing observer still performs
/// every member's no-follow open, inode/owner/mode checks and complete hash.
struct HeldTreeRoot<'a>(&'a LeaseInner);
impl InventoryRoot for HeldTreeRoot<'_> {
    fn root_file(&self) -> &File {
        &self.0.root
    }
    fn root_path(&self) -> &Path {
        &self.0.path
    }
    fn allocation_id(&self) -> &str {
        &self.0.key_name
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.0.check_held_tree()
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

/// Borrowed only by a synchronous, private package materialization batch.
/// The enclosing helper verifies complete contents before exposing this view
/// and again before returning any prepared objects to their publisher.
pub(in crate::libraries) struct LanguageMaterialization<'a> {
    language: &'a LanguageLease,
}
impl LanguageMaterialization<'_> {
    pub(in crate::libraries) fn language(&self) -> &LanguageLease {
        self.language
    }
    pub(in crate::libraries) fn check_binding(&self) -> Result<()> {
        self.language.inner.check_manifest()
    }
}

impl LanguageLease {
    /// The callback only materializes inspected wheels into private prefixes;
    /// it must not execute an interpreter or build backend, or publish them.
    /// On either failure its prepared values drop before becoming available.
    pub(in crate::libraries) fn with_package_materialization<T>(
        &self,
        prepare: impl FnOnce(&LanguageMaterialization<'_>) -> Result<T>,
    ) -> Result<T> {
        self.check_binding()?;
        let scope = LanguageMaterialization { language: self };
        let prepared = prepare(&scope)?;
        self.check_binding()?;
        Ok(prepared)
    }

    pub(crate) fn key(&self) -> super::identity::LanguageKey {
        self.inner.spec.key()
    }
    pub(crate) fn shared_target_identity(&self) -> Result<super::identity::TargetIdentity> {
        self.inner.spec.plan().shared_target_identity()
    }
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

#[cfg(test)]
mod held_tree_tests {
    use super::*;
    use crate::libraries::language::repository::tests::{
        archive, install_spec, operation_context, prepare,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[tokio::test]
    async fn f1_held_inventory_rechecks_store_namespace_after_the_complete_hash_walk() {
        let owner = tempfile::tempdir().unwrap();
        let path = owner.path().canonicalize().unwrap();
        let context = operation_context(&path, true, Duration::from_secs(120));
        let bytes = archive("3.15.0");
        let spec = install_spec(&bytes);
        let lease = prepare(&spec, &context, &bytes).await.unwrap();
        let catalog = context
            .requested_data_root()
            .join("languages/.control/catalog");
        let displaced = catalog.with_file_name("catalog-displaced");
        let result = lease.inner.check_full_with_after_walk(|| {
            std::fs::rename(&catalog, &displaced)?;
            std::fs::create_dir(&catalog)?;
            std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o700))?;
            Ok(())
        });
        assert!(
            result.is_err(),
            "a full tree hash cannot authenticate a replaced control namespace"
        );
        std::fs::remove_dir(&catalog).unwrap();
        std::fs::rename(&displaced, &catalog).unwrap();
        lease.check_binding().unwrap();
        std::fs::write(
            lease.root().join("lib/python3.15/stdlib-marker"),
            b"changed",
        )
        .unwrap();
        assert!(
            lease.check_binding().is_err(),
            "the held-fd scope must retain complete content hashing"
        );
    }
}
