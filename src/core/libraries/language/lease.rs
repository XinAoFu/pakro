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
    ensure_entry_identity, open_trusted_store_with_deadline, validate_owned_directory,
};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Match the complete portable result against already validated published
/// facts. A source request key identifies inputs, not its observed output.
pub(super) fn matches_source_pin_facts(
    spec: &LanguageInstallSpec,
    source_build: Option<&super::source::SourcePythonBuildRecord>,
    request: &str,
    pin: &crate::lockfile::PlatformInfo,
    settings: &crate::config::Settings,
) -> Result<bool> {
    let expected = crate::plugins::core::python::pinned_shared_language_spec(
        request, pin, spec.patch_sysconfig(), settings,
    )?;
    let wanted = expected.plan().source();
    let actual = spec.plan().source();
    if expected.key() != spec.key()
        || wanted.filename() != actual.filename()
        || wanted.digest() != actual.digest()
        || pin.source_python.as_ref() != source_build
    {
        return Ok(false);
    }
    expected.plan().validate_source_locator(actual.url().as_str())?;
    Ok(true)
}

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
    /// Read-only queries need the Ready manifest, the physical binding and the
    /// actual interpreter bytes, not a re-hash of every published file.
    /// Hashing the whole tree on each `which`/`env`/`installs ls` costs seconds
    /// on slow disks. Install, publication, package materialization and
    /// deletion keep check_full.
    pub(super) fn check_metadata(&self) -> Result<()> {
        self.check_manifest()?;
        let entrypoint = &self.manifest.entrypoint;
        let expected = self
            .manifest
            .inventory
            .entries()
            .iter()
            .find(|entry| &entry.relative == entrypoint)
            .context("language entrypoint is absent from its complete inventory")?;
        let crate::packages::python_environment::prefix::verify::TreeInventoryKind::File {
            size,
            sha256,
        } = expected.kind
        else {
            eyre::bail!("language entrypoint is not a regular file")
        };
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;
        use sha2::{Digest as _, Sha256};
        use std::io::Read as _;
        let entries = self.manifest.inventory.entries();
        let mut parent = self.root.try_clone()?;
        let components = entrypoint.components().collect::<Vec<_>>();
        let mut partial = PathBuf::new();
        for component in &components[..components.len() - 1] {
            let std::path::Component::Normal(name) = component else {
                eyre::bail!("invalid language entrypoint member")
            };
            let name = *name;
            partial.push(name);
            parent = File::from(openat(
                &parent,
                name,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?);
            let directory = entries
                .iter()
                .find(|entry| entry.relative == partial)
                .context("language entrypoint directory is unrecorded")?;
            let identity = FileIdentity::of(&parent)?;
            ensure!(
                [identity.device as u64, identity.inode as u64] == directory.identity,
                "language entrypoint directory changed"
            );
        }
        let name = components
            .last()
            .and_then(|component| match component {
                std::path::Component::Normal(name) => Some(*name),
                _ => None,
            })
            .context("language entrypoint has no filename")?;
        let mut file = File::from(openat(
            &parent,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        let identity = FileIdentity::of(&file)?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == expected.uid
                && metadata.mode() & 0o7777 == expected.mode
                && metadata.len() == size
                && [identity.device as u64, identity.inode as u64] == expected.identity,
            "language entrypoint identity, owner, mode or size changed"
        );
        let mut bytes = Vec::new();
        file.by_ref().take(size + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == size && <[u8; 32]>::from(Sha256::digest(&bytes)) == sha256,
            "language entrypoint bytes changed"
        );
        self.context.checkpoint()?;
        Ok(())
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
        if let Some(record) = &self.manifest.source_build {
            let layout = self
                .manifest
                .source_layout
                .as_ref()
                .context("source Python layout is missing")?;
            let output = super::source::portable_output_digest(
                &self.manifest.inventory,
                &self.path,
                layout,
                |relative| {
                    use nix::fcntl::{OFlag, openat};
                    use nix::sys::stat::Mode;
                    use std::io::Read;
                    let mut parent = self.root.try_clone()?;
                    let mut partial = PathBuf::new();
                    let components = relative.components().collect::<Vec<_>>();
                    for component in &components[..components.len() - 1] {
                        let std::path::Component::Normal(name) = component else {
                            eyre::bail!("invalid source Python metadata member");
                        };
                        let name = *name;
                        partial.push(name);
                        parent = File::from(openat(
                            &parent,
                            name,
                            OFlag::O_RDONLY
                                | OFlag::O_DIRECTORY
                                | OFlag::O_NOFOLLOW
                                | OFlag::O_CLOEXEC,
                            Mode::empty(),
                        )?);
                        let expected = self
                            .manifest
                            .inventory
                            .entries()
                            .iter()
                            .find(|entry| entry.relative == partial)
                            .context("source Python metadata directory is unrecorded")?;
                        let identity = FileIdentity::of(&parent)?;
                        ensure!(
                            [identity.device as u64, identity.inode as u64] == expected.identity,
                            "source Python metadata directory changed"
                        );
                    }
                    let expected = self
                        .manifest
                        .inventory
                        .entries()
                        .iter()
                        .find(|entry| entry.relative == relative)
                        .context("source Python metadata is unrecorded")?;
                    let name = relative
                        .file_name()
                        .context("source Python metadata has no filename")?;
                    let mut file = File::from(openat(
                        &parent,
                        name,
                        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )?);
                    let metadata = file.metadata()?;
                    let identity = FileIdentity::of(&file)?;
                    ensure!(
                        metadata.is_file()
                            && metadata.len() <= 16 * 1024 * 1024
                            && metadata.uid() == expected.uid
                            && metadata.mode() & 0o7777 == expected.mode
                            && [identity.device as u64, identity.inode as u64] == expected.identity,
                        "source Python metadata differs from its held inventory"
                    );
                    let mut bytes = Vec::new();
                    Read::by_ref(&mut file)
                        .take(16 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)?;
                    self.context.checkpoint()?;
                    Ok(bytes)
                },
            )?;
            ensure!(
                output == record.output_sha256,
                "published source Python portable output differs from its original build result"
            );
        }
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
        self.language.check_held_binding()
    }
}

impl LanguageLease {
    /// Recheck the original manifest bytes and actual protection bindings
    /// inside an operation that already admitted this held inventory.
    /// This does not replace a fresh acquisition or an execution/publication
    /// boundary's complete content verification.
    pub(crate) fn check_held_binding(&self) -> Result<()> {
        self.inner.check_manifest()
    }

    /// Prove membership in the caller's same physical language library. This
    /// does not replace complete Ready-manifest and inventory validation.
    pub(crate) fn ensure_library_context(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.inner.check_shallow()?;
        ensure!(
            self.inner.context.requested_data_root() == context.requested_data_root(),
            "held language belongs to another operation library root"
        );
        let expected = context.requested_data_root().join("languages");
        ensure!(
            self.inner.read.root_path() == expected.as_path(),
            "held language reader is bound to another library"
        );
        let deadline = context
            .policy()
            .deadline
            .min(self.inner.context.policy().deadline);
        let current = open_trusted_store_with_deadline(&expected, uid(), false, deadline)?;
        ensure!(
            FileIdentity::of(&current)? == FileIdentity::of(&self.inner.store)?,
            "held language library differs from the caller's physical store"
        );
        self.inner.check_shallow()?;
        context.checkpoint()?;
        Ok(())
    }

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

    pub(crate) fn spec(&self) -> &LanguageInstallSpec {
        &self.inner.spec
    }
    pub(crate) fn descriptor(&self) -> &PythonRuntimeDescriptor {
        &self.inner.manifest.descriptor
    }
    /// Descriptive bytes already checked by the held language inventory.
    pub(crate) fn entrypoint_sha256(&self) -> Result<[u8; 32]> {
        self.inner.manifest.inventory.entries().iter().find_map(|entry| {
            if entry.relative != self.inner.manifest.entrypoint { return None; }
            match entry.kind {
                crate::packages::python_environment::prefix::verify::TreeInventoryKind::File { sha256, .. } => Some(sha256),
                _ => None,
            }
        }).context("language entrypoint is absent from its validated file inventory")
    }
    pub(crate) fn target(&self) -> &FrozenRegistryTarget {
        &self.inner.target
    }
    /// Pure portable source description from the held runtime descriptor.
    /// This does not revalidate contents or grant acquisition/publication rights;
    /// callers keep the existing complete lease checks at their IO boundaries.
    pub(crate) fn source_pin(&self) -> Result<crate::lockfile::PlatformInfo> {
        let source = self.inner.spec.plan().source();
        Ok(crate::lockfile::PlatformInfo {
            install: self
                .inner
                .manifest
                .source_build
                .as_ref()
                .map(|_| "source".into()),
            source_python: self.inner.manifest.source_build.clone(),
            url: Some(source.url().to_string()),
            checksum: Some(source.digest().to_string().replacen('-', ":", 1)),
            ..Default::default()
        })
    }
    /// Compare the complete portable source result with this selected instance.
    /// This reads validated facts only; the caller retains the actual use lease.
    pub(crate) fn matches_source_pin(
        &self,
        request: &str,
        pin: &crate::lockfile::PlatformInfo,
        settings: &crate::config::Settings,
    ) -> Result<bool> {
        matches_source_pin_facts(
            &self.inner.spec,
            self.inner.manifest.source_build.as_ref(),
            request,
            pin,
            settings,
        )
    }
    /// Pure exact request reconstructed from the held source and probe,
    /// including the PBS distribution build. Content validation belongs to the
    /// surrounding acquisition/query/commit boundary.
    pub(crate) fn lock_version(&self) -> Result<String> {
        let descriptor = self.descriptor();
        if let Some(record) = &self.inner.manifest.source_build {
            ensure!(
                record.version == descriptor.language_version.full_version(),
                "source CPython version differs from its build result"
            );
            return Ok(record.version.clone());
        }
        match descriptor.implementation {
            crate::python_runtime::PythonImplementation::CPython => {
                let (version, build, _, _) = crate::plugins::core::python::native::pbs_filename(
                    self.inner.spec.plan().source().filename(),
                )?
                .context("protected CPython source lacks its exact PBS identity")?;
                ensure!(
                    version == descriptor.language_version.full_version(),
                    "protected runtime version differs from its source"
                );
                Ok(format!("{version}+{build}"))
            }
            crate::python_runtime::PythonImplementation::PyPy => Ok(format!(
                "pypy{}.{}-{}",
                descriptor.language_version.major,
                descriptor.language_version.minor,
                descriptor.implementation_version.full_version(),
            )),
        }
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
    /// Query path: Ready manifest and physical binding only.
    pub(crate) fn check_metadata(&self) -> Result<()> {
        self.inner.check_metadata()
    }
}

#[cfg(test)]
pub(crate) mod pin_tests {
    use super::*;
    use crate::libraries::language::source::{SourcePythonBuildInput, SourcePythonBuildRecord};
    use crate::packages::python_environment::model::{NativeProgramRecord, NativeToolchainRecord};
    use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
    use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;

    /// Valid portable test data only; this does not create a language lease or
    /// claim that these synthetic digests describe an actual source build.
    pub(crate) fn source_facts() -> (LanguageInstallSpec, SourcePythonBuildRecord, crate::lockfile::PlatformInfo) {
        let platform = if cfg!(target_os = "macos") { RuntimePlatform::Macos }
            else if cfg!(target_env = "musl") { RuntimePlatform::LinuxMusl }
            else { RuntimePlatform::LinuxGnu };
        let target = RuntimeArchiveTarget::new(std::env::consts::ARCH, platform).unwrap();
        let mut toolchain = NativeToolchainRecord::test_record();
        toolchain.platform = format!("{}-{}-{}", if cfg!(target_os = "macos") { "macos" } else { "linux" },
            std::env::consts::ARCH, if cfg!(target_os = "macos") { "none" } else if cfg!(target_env = "musl") { "musl" } else { "gnu" });
        toolchain.programs = ["ar", "cc", "cxx", "linker", "make"].into_iter().enumerate()
            .map(|(index, role)| NativeProgramRecord { role: role.into(), sha256: [index as u8 + 31; 32], version: "portable fixture 1".into() }).collect();
        let source = pakro_index::artifact::SourceArtifact::new("Python-3.15.0.tar.xz",
            "https://www.python.org/ftp/python/3.15.0/Python-3.15.0.tar.xz", pakro_store::Digest::Sha256([71; 32])).unwrap();
        let input = SourcePythonBuildInput::new("3.15.0", source, target, toolchain, None).unwrap();
        let record = input.record([72; 32], [73; 32]).unwrap();
        let spec = LanguageInstallSpec::from_plan(&RuntimeInstallPlan::from_source(&input).unwrap(), false).unwrap();
        let pin = crate::lockfile::PlatformInfo { install: Some("source".into()), source_python: Some(record.clone()),
            url: Some(record.url.clone()), checksum: Some(record.artifact.replacen('-', ":", 1)), ..Default::default() };
        (spec, record, pin)
    }

    #[test]
    fn p6_source_pin_matching_requires_the_complete_observed_build_result() {
        let (spec, record, pin) = source_facts();
        let settings = crate::config::Settings::default();
        assert!(matches_source_pin_facts(&spec, Some(&record), "3.15.0", &pin, &settings).unwrap());
        for output in [true, false] {
            let mut changed = pin.clone();
            let result = changed.source_python.as_mut().unwrap();
            if output { result.output_sha256[0] ^= 1; } else { result.runtime_target[0] ^= 1; }
            let expected = crate::plugins::core::python::pinned_shared_language_spec("3.15.0", &changed, false, &settings).unwrap();
            assert_eq!(expected.key(), spec.key(), "request identity deliberately excludes observed output");
            assert!(!matches_source_pin_facts(&spec, Some(&record), "3.15.0", &changed, &settings).unwrap(),
                "an equal request key and archive cannot substitute a different actual output or runtime target");
        }
        assert!(!matches_source_pin_facts(&spec, None, "3.15.0", &pin, &settings).unwrap());
        let mut invalid = pin;
        invalid.source_python.as_mut().unwrap().request_id[0] ^= 1;
        assert!(matches_source_pin_facts(&spec, Some(&record), "3.15.0", &invalid, &settings).is_err(),
            "invalid lock facts must remain an error rather than a candidate mismatch");
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
