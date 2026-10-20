//! One native runtime allocation per exact input key.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::identity::LanguageKey;
use super::lease::LanguageLease;
use super::manifest::{DiskIdentity, LockRegistration};
use crate::artifact_snapshot::VerifiedArtifact;
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, TreeInventory, TreeInventoryKind, observe_tree_inventory,
};
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, bail, ensure};
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fchmod, fstatat, mkdirat};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, ReadStore, RootLimits, StoreWriter,
    ensure_entry_identity, open_directory, open_existing_readonly_regular,
    validate_owned_directory,
};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone)]
pub(crate) struct LanguageInstallSpec {
    plan: RuntimeInstallPlan,
    patch_sysconfig: bool,
    key: LanguageKey,
}
impl LanguageInstallSpec {
    pub(crate) fn from_plan(plan: &RuntimeInstallPlan, patch_sysconfig: bool) -> Result<Self> {
        let patch_sysconfig = patch_sysconfig
            && plan.distribution()
                == crate::plugins::core::python::native::RuntimeDistribution::Pbs;
        Ok(Self {
            plan: plan.clone(),
            patch_sysconfig,
            key: plan.language_key(patch_sysconfig)?,
        })
    }
    pub(crate) fn plan(&self) -> &RuntimeInstallPlan {
        &self.plan
    }
    pub(crate) fn patch_sysconfig(&self) -> bool {
        self.patch_sysconfig
    }
    pub(crate) fn key(&self) -> LanguageKey {
        self.key
    }
}

pub(crate) struct LanguageRepository {
    pub(super) context: OperationContext,
    pub(super) path: PathBuf,
    pub(super) writer: Option<Arc<StoreWriter>>,
}
pub(crate) enum LanguageReservation {
    Existing(LanguageLease),
    Build(LanguageBuildOwner),
}
pub(crate) enum LanguageState {
    Missing,
    Ready(LanguageLease),
}

/// Distinguishes unreadable store state from damaged stored facts, so queries
/// can report corruption instead of an uninstalled version.
#[derive(Debug)]
pub(crate) enum LanguageEnumerateError {
    Unreadable(eyre::Report),
    Corrupt(String),
}

/// One published local instance described from its stored manifest. The spec
/// is rebuilt from manifest facts only; carrying this value is descriptive,
/// not a lease.
#[derive(Debug, Clone)]
pub(crate) struct PublishedLanguage {
    manifest: super::manifest::LanguageManifest,
    spec: LanguageInstallSpec,
    payload_root: PathBuf,
}
impl PublishedLanguage {
    pub(crate) fn key(&self) -> LanguageKey {
        self.spec.key()
    }
    pub(crate) fn spec(&self) -> &LanguageInstallSpec {
        &self.spec
    }
    pub(crate) fn payload_root(&self) -> &Path {
        &self.payload_root
    }
    pub(crate) fn entrypoint(&self) -> &Path {
        &self.manifest.descriptor.executable
    }
    pub(crate) fn version(&self) -> String {
        self.manifest.descriptor.language_version.full_version()
    }
    /// The distribution build date parsed from the stored archive filename,
    /// e.g. `20261013` for `cpython-3.13.16+20261013-...`.
    pub(crate) fn build(&self) -> Option<&str> {
        self.manifest
            .filename
            .rsplit_once(".tar.gz")
            .map(|(stem, _)| stem)
            .and_then(|stem| stem.split_once('+'))
            .map(|(_, build)| build)
            .and_then(|build| build.split_once('-'))
            .map(|(build, _)| build)
    }
    pub(crate) fn platform_system(&self) -> &str {
        &self.manifest.descriptor.markers.platform_system
    }
    pub(crate) fn platform_machine(&self) -> &str {
        &self.manifest.descriptor.markers.platform_machine
    }
    /// F1 host matching by the manifest's own platform observations. This is
    /// the manifest-level view of target compatibility; full target identity
    /// checking stays with the acquire validation.
    pub(crate) fn matches_host_platform(&self) -> bool {
        let system = match std::env::consts::OS {
            "macos" => "Darwin",
            "linux" => "Linux",
            _ => return false,
        };
        let machine = match (std::env::consts::ARCH, std::env::consts::OS) {
            ("aarch64", "macos") => "arm64",
            (arch, _) => arch,
        };
        self.platform_system() == system && self.platform_machine() == machine
    }
}

impl LanguageRepository {
    pub(crate) fn open_writer(context: &OperationContext) -> Result<Self> {
        context.checkpoint()?;
        ensure!(
            context.policy().writes.libraries,
            "operation policy forbids language library writes"
        );
        let path = context.requested_data_root().join("languages");
        let writer = StoreWriter::open_writer(
            &path,
            uid(),
            RootLimits::default(),
            context.policy().deadline,
        )?;
        context.checkpoint()?;
        Ok(Self {
            context: context.clone(),
            path: writer.root_path().to_owned(),
            writer: Some(Arc::new(writer)),
        })
    }
    pub(crate) fn open_readonly(context: &OperationContext) -> Result<Self> {
        context.checkpoint()?;
        Ok(Self {
            context: context.clone(),
            path: context.requested_data_root().join("languages"),
            writer: None,
        })
    }
    pub(crate) fn lookup(&self, spec: &LanguageInstallSpec) -> Result<LanguageState> {
        Ok(match self.acquire_existing(spec)? {
            Some(lease) => LanguageState::Ready(lease),
            None => LanguageState::Missing,
        })
    }

    /// Rebuilds the installation spec of one stored manifest from its own
    /// facts. No remote source is contacted and the rebuilt identity must
    /// match the stored key fingerprint, otherwise the record is damaged.
    pub(crate) fn spec_from_manifest(
        manifest: &super::manifest::LanguageManifest,
    ) -> Result<LanguageInstallSpec> {
        let Some((version, _build, target, flavor)) =
            crate::plugins::core::python::native::pbs_filename(&manifest.filename)?
        else {
            eyre::bail!("stored language manifest is not an official PBS archive");
        };
        let platform = crate::lockfile::PlatformInfo {
            url: Some(manifest.source.clone()),
            checksum: Some(manifest.artifact.replacen('-', ":", 1)),
            ..Default::default()
        };
        let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_locked(
            &version, &target, flavor, &platform,
        )?;
        let spec = LanguageInstallSpec::from_plan(&plan, manifest.patch_sysconfig)?;
        ensure!(
            spec.key().fingerprint() == manifest.key,
            "stored language manifest disagrees with its rebuilt installation identity"
        );
        Ok(spec)
    }

    /// Enumerates the published local instances from their real manifests.
    /// Store-level failures are unreadable state; manifest-level damage is
    /// reported as corrupt so a caller never mistakes it for "not installed".
    pub(crate) fn enumerate_published(
        &self,
    ) -> std::result::Result<Vec<PublishedLanguage>, LanguageEnumerateError> {
        self.context
            .checkpoint()
            .map_err(|error| LanguageEnumerateError::Unreadable(error.into()))?;
        let read = ReadStore::open_readonly(
            &self.path,
            uid(),
            RootLimits::default(),
            self.context.policy().deadline,
        )
        .map_err(LanguageEnumerateError::Unreadable)?;
        // A store that does not exist yet simply has nothing published; that
        // is distinct from unreadable or damaged state.
        let Some(read) = read else {
            return Ok(Vec::new());
        };
        let control = read
            .control(ControlLimits::default())
            .map_err(LanguageEnumerateError::Unreadable)?;
        let names = control
            .list_names(ControlNamespace::Catalog, 4096, 512 * 1024)
            .map_err(LanguageEnumerateError::Unreadable)?;
        let mut published = Vec::new();
        for name in names {
            self.context
                .checkpoint()
                .map_err(|error| LanguageEnumerateError::Unreadable(error.into()))?;
            let snapshot = control
                .read(ControlNamespace::Catalog, &name)
                .map_err(LanguageEnumerateError::Unreadable)?
                .with_context(|| format!("language manifest {name} is unreadable"))
                .map_err(LanguageEnumerateError::Unreadable)?;
            let manifest: super::manifest::LanguageManifest =
                serde_json::from_slice(snapshot.bytes()).map_err(|error| {
                    LanguageEnumerateError::Corrupt(format!(
                        "language manifest {name} is damaged: {error}"
                    ))
                })?;
            let spec = Self::spec_from_manifest(&manifest).map_err(|error| {
                LanguageEnumerateError::Corrupt(format!(
                    "language manifest {name} is damaged: {error:#}"
                ))
            })?;
            let payload_root = read.root_path().join(spec.key().filename()).join("payload");
            published.push(PublishedLanguage {
                manifest,
                spec,
                payload_root,
            });
        }
        Ok(published)
    }

    /// Re-acquires the real lease of one published key by rebuilding its spec
    /// from the stored manifest, then walking the full acquire validation.
    pub(crate) fn acquire_by_key(&self, key: &LanguageKey) -> Result<Option<LanguageLease>> {
        self.context.checkpoint()?;
        let Some(read) = ReadStore::open_readonly(
            &self.path,
            uid(),
            RootLimits::default(),
            self.context.policy().deadline,
        )?
        else {
            return Ok(None);
        };
        let control = read.control(ControlLimits::default())?;
        let manifest_name = format!("{}.json", key.filename());
        let Some(snapshot) = control.read(ControlNamespace::Catalog, &manifest_name)? else {
            return Ok(None);
        };
        let manifest: super::manifest::LanguageManifest = serde_json::from_slice(snapshot.bytes())?;
        ensure!(
            manifest.key == key.fingerprint(),
            "stored language manifest disagrees with the requested key"
        );
        let spec = Self::spec_from_manifest(&manifest)?;
        self.acquire_existing(&spec)
    }

    pub(crate) fn acquire_existing(
        &self,
        spec: &LanguageInstallSpec,
    ) -> Result<Option<LanguageLease>> {
        self.context.checkpoint()?;
        let Some(read) = ReadStore::open_readonly(
            &self.path,
            uid(),
            RootLimits::default(),
            self.context.policy().deadline,
        )?
        else {
            return Ok(None);
        };
        let control = read.control(ControlLimits::default())?;
        let initial = control.read(ControlNamespace::Catalog, &manifest_name(spec))?;
        let registration = control.read(ControlNamespace::Ownership, &registration_name(spec))?;
        let store_identity = control.store_identity()?;
        let store = pakro_store::control::open_trusted_store_with_deadline(
            read.root_path(),
            uid(),
            false,
            self.context.policy().deadline,
        )?;
        let actual = FileIdentity::of(&store)?;
        ensure!(
            (actual.device as u64, actual.inode as u64) == store_identity,
            "language store identity changed"
        );
        let key_name = spec.key.filename();
        let leases = open_directory(&store, ".leases")?;
        let build = open_directory(&leases, "build")?;
        let Some(registration) = registration else {
            // Registration precedes every allocation. This catalog SH makes a
            // missing result coherent with the absence of an actual payload.
            ensure!(
                initial.is_none(),
                "language Ready lock registration is missing"
            );
            ensure!(
                entry_missing(&store, &key_name)?,
                "unregistered language payload is corrupt, not missing"
            );
            ensure!(
                entry_missing(&build, &format!("{key_name}.lock"))?,
                "unregistered language protection inode is corrupt, not missing"
            );
            control.check_binding()?;
            self.context.checkpoint()?;
            return Ok(None);
        };
        let registered: LockRegistration = serde_json::from_slice(registration.bytes())?;
        ensure!(
            registered.schema == 1 && registered.key == spec.key.fingerprint(),
            "language lock registration differs from the requested key"
        );
        registered.lock.validate()?;
        drop(control);
        let lock = StableLock::open_existing(
            &build,
            &format!("{key_name}.lock"),
            uid(),
            registered.lock.file_identity(),
        )?;
        let scope = read.control(ControlLimits::default())?;
        let held = loop {
            self.context.checkpoint()?;
            if let Some(held) = scope.try_object(&key_name, &lock, LeaseMode::Shared)? {
                break held;
            }
            std::thread::sleep(
                Duration::from_millis(5).min(
                    self.context
                        .policy()
                        .deadline
                        .saturating_duration_since(Instant::now()),
                ),
            );
        };
        // Wait for an active publisher without holding catalog, then distinguish
        // its complete Ready record from an interrupted or damaged allocation.
        let current = scope.read(ControlNamespace::Catalog, &manifest_name(spec))?;
        let current_registration = scope
            .read(ControlNamespace::Ownership, &registration_name(spec))?
            .context("language lock registration disappeared")?;
        ensure!(
            current_registration.bytes() == registration.bytes(),
            "language lock registration changed while acquiring its lease"
        );
        let Some(current) = current else {
            ensure!(
                initial.is_none() && entry_missing(&store, &key_name)?,
                "language payload exists without its Ready manifest"
            );
            scope.check_binding()?;
            self.context.checkpoint()?;
            return Ok(None);
        };
        if let Some(initial) = initial {
            ensure!(
                current.sha256() == initial.sha256() && current.bytes() == initial.bytes(),
                "language publication changed while acquiring its lease"
            );
        }
        let manifest: super::manifest::LanguageManifest = serde_json::from_slice(current.bytes())?;
        let target = super::publication::validate_manifest(spec, &manifest)?;
        ensure!(
            registered.lock == manifest.lock,
            "language Ready lock registration differs"
        );
        let allocation = open_directory(&store, key_name.as_str())?;
        let root = open_directory(&allocation, "payload")?;
        let control = open_directory(&store, ".control")?;
        let catalog = open_directory(&control, "catalog")?;
        let manifest_pin = open_existing_readonly_regular(&catalog, &manifest_name(spec), uid())?;
        let path = read.root_path().join(&key_name).join("payload");
        super::publication::validate_manifest_location(&manifest, &path)?;
        drop(scope);
        let lease = LanguageLease {
            inner: Arc::new(super::lease::LeaseInner {
                context: self.context.clone(),
                read,
                store,
                build,
                catalog,
                manifest_pin,
                allocation,
                root,
                path,
                key_name,
                spec: spec.clone(),
                lock,
                held,
                snapshot: current,
                manifest,
                target,
            }),
        };
        lease.check_binding()?;
        Ok(Some(lease))
    }
    pub(crate) fn reserve_install(
        &self,
        spec: &LanguageInstallSpec,
    ) -> Result<LanguageReservation> {
        self.context.checkpoint()?;
        if let Some(existing) = self.acquire_existing(spec)? {
            return Ok(LanguageReservation::Existing(existing));
        }
        ensure!(
            self.context.policy().writes.libraries,
            "operation policy forbids language library writes"
        );
        let writer = self
            .writer
            .as_ref()
            .context("readonly repository cannot reserve a language")?;
        let key = spec.key.filename();
        let lock_name = format!("{key}.lock");
        // The catalog serializes first registration. Release it before waiting
        // for the object EX; established missing locks are never recreated.
        let lock = {
            let mut control = writer.control(ControlLimits::default())?;
            let registration =
                control.read(ControlNamespace::Ownership, &registration_name(spec))?;
            match registration {
                Some(snapshot) => {
                    let value: LockRegistration = serde_json::from_slice(snapshot.bytes())?;
                    ensure!(
                        value.schema == 1 && value.key == spec.key.fingerprint(),
                        "language lock registration differs from the request"
                    );
                    value.lock.validate()?;
                    StableLock::open_existing(
                        writer.build_handle(),
                        &lock_name,
                        uid(),
                        value.lock.file_identity(),
                    )?
                }
                None => {
                    ensure!(
                        control
                            .read(ControlNamespace::Catalog, &manifest_name(spec))?
                            .is_none(),
                        "Ready language has no lock registration"
                    );
                    ensure!(
                        entry_missing(writer.root_handle(), &key)?,
                        "unregistered language allocation already exists"
                    );
                    let lock = StableLock::create_new(writer.build_handle(), &lock_name, uid())?;
                    let value = LockRegistration {
                        schema: 1,
                        key: spec.key.fingerprint(),
                        lock: DiskIdentity::from_file_identity(lock.identity()),
                    };
                    control.replace(
                        ControlNamespace::Ownership,
                        &registration_name(spec),
                        None,
                        &serde_json::to_vec(&value)?,
                    )?;
                    lock
                }
            }
        };
        self.context.checkpoint()?;
        let control = writer.control(ControlLimits::default())?;
        let held = loop {
            self.context.checkpoint()?;
            if let Some(held) = control.try_object(&key, &lock, LeaseMode::Exclusive)? {
                break held;
            }
            std::thread::sleep(
                Duration::from_millis(5).min(
                    self.context
                        .policy()
                        .deadline
                        .saturating_duration_since(Instant::now()),
                ),
            );
        };
        let existing = control
            .read(ControlNamespace::Catalog, &manifest_name(spec))?
            .is_some();
        drop(control);
        if existing {
            drop(held);
            return Ok(LanguageReservation::Existing(
                self.acquire_existing(spec)?
                    .context("winning language publication disappeared")?,
            ));
        }
        self.context.checkpoint()?;
        writer.check_binding()?;
        held.check_binding()?;
        ensure!(
            entry_missing(writer.root_handle(), &key)?,
            "an interrupted unpublished language allocation requires recovery"
        );
        mkdirat(
            writer.root_handle(),
            key.as_str(),
            Mode::from_bits_truncate(0o700),
        )?;
        let allocation = open_directory(writer.root_handle(), key.as_str())?;
        fchmod(&allocation, Mode::from_bits_truncate(0o700))?;
        writer.root_handle().sync_all()?;
        mkdirat(&allocation, "payload", Mode::from_bits_truncate(0o700))?;
        let root = open_directory(&allocation, "payload")?;
        fchmod(&root, Mode::from_bits_truncate(0o700))?;
        root.sync_all()?;
        allocation.sync_all()?;
        let owner = LanguageBuildOwner {
            context: self.context.clone(),
            writer: writer.clone(),
            spec: spec.clone(),
            allocation,
            root,
            path: self.path.join(&key).join("payload"),
            lock,
            held,
            entries: BTreeMap::new(),
            files: RefCell::new(BTreeMap::new()),
            limits: PrefixInventoryLimits::default(),
            path_bytes: 0,
            file_count: 0,
            directory_count: 0,
            total_bytes: Cell::new(0),
            poisoned: Cell::new(false),
            retain_allocation: Cell::new(false),
        };
        owner.checkpoint()?;
        Ok(LanguageReservation::Build(owner))
    }
}

pub(super) fn uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}
pub(super) fn manifest_name(spec: &LanguageInstallSpec) -> String {
    format!("{}.json", spec.key.filename())
}
pub(super) fn registration_name(spec: &LanguageInstallSpec) -> String {
    format!("{}.lock.json", spec.key.filename())
}
fn validate_payload_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == uid()
            && metadata.nlink() == 1
            && metadata.mode() & 0o7022 == 0
            && metadata.mode() & 0o400 != 0,
        "language payload file has unsafe type, owner, links or mode"
    );
    Ok(())
}
fn entry_missing(parent: &File, name: &str) -> Result<bool> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
}
#[derive(Clone)]
struct OwnedEntry {
    identity: FileIdentity,
    kind: EntryKind,
    mode: u32,
    target: Option<PathBuf>,
    explicit_directory: bool,
}
#[derive(Clone, Copy)]
struct WrittenFile {
    size: u64,
    sha256: [u8; 32],
}

pub(crate) struct LanguageBuildOwner {
    pub(super) context: OperationContext,
    pub(super) writer: Arc<StoreWriter>,
    pub(super) spec: LanguageInstallSpec,
    pub(super) allocation: File,
    pub(super) root: File,
    pub(super) path: PathBuf,
    pub(super) lock: StableLock,
    pub(super) held: ObjectLease,
    entries: BTreeMap<PathBuf, OwnedEntry>,
    files: RefCell<BTreeMap<PathBuf, WrittenFile>>,
    limits: PrefixInventoryLimits,
    path_bytes: usize,
    file_count: usize,
    directory_count: usize,
    total_bytes: Cell<u64>,
    poisoned: Cell<bool>,
    pub(super) retain_allocation: Cell<bool>,
}
impl LanguageBuildOwner {
    pub(crate) fn final_path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn spec(&self) -> &LanguageInstallSpec {
        &self.spec
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.context.policy().deadline
    }
    pub(crate) fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.context.cancellation_token()
    }
    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.context.checkpoint()?;
        ensure!(
            !self.poisoned.get(),
            "unpublished language owner is poisoned"
        );
        self.writer.check_binding()?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive
                && self.held.identity() == self.lock.identity(),
            "language build protection changed"
        );
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("{}.lock", self.spec.key.filename()),
        )?;
        ensure_entry_identity(
            self.writer.root_handle(),
            &self.spec.key.filename(),
            FileIdentity::of(&self.allocation)?,
        )?;
        ensure_entry_identity(&self.allocation, "payload", FileIdentity::of(&self.root)?)?;
        for directory in [&self.allocation, &self.root] {
            validate_owned_directory(directory, uid())?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "language allocation permissions changed"
            );
        }
        self.context.checkpoint()?;
        Ok(())
    }
    pub(crate) fn check_artifact(&self, artifact: &VerifiedArtifact) -> Result<()> {
        self.checkpoint()?;
        ensure!(
            artifact.filename() == self.spec.plan.source().filename()
                && artifact.digest() == self.spec.plan.source().digest(),
            "verified artifact differs from the reserved language"
        );
        Ok(())
    }
    pub(crate) fn capture_inventory(&self) -> Result<TreeInventory> {
        let inventory = observe_tree_inventory(self, self.limits)?;
        ensure!(
            inventory.entries().len() == self.entries.len(),
            "language inventory contains unowned or missing entries"
        );
        let files = self.files.borrow();
        for actual in inventory.entries() {
            let expected = self
                .entries
                .get(&actual.relative)
                .context("language inventory contains an unowned entry")?;
            ensure!(
                actual.identity
                    == [
                        expected.identity.device as u64,
                        expected.identity.inode as u64
                    ]
                    && actual.uid == uid()
                    && actual.mode == expected.mode,
                "language inventory entry identity, owner or mode changed"
            );
            match (&actual.kind, expected.kind) {
                (TreeInventoryKind::Directory, EntryKind::Directory) => {}
                (TreeInventoryKind::Symlink { target }, EntryKind::Symlink) => ensure!(
                    expected.target.as_ref() == Some(target),
                    "language inventory link changed"
                ),
                (TreeInventoryKind::File { size, sha256 }, EntryKind::File) => {
                    let written = files
                        .get(&actual.relative)
                        .context("language inventory includes an unfinished file")?;
                    ensure!(
                        *size == written.size && *sha256 == written.sha256,
                        "language inventory differs from bytes written by its owner"
                    );
                }
                _ => bail!("language inventory entry type changed"),
            }
        }
        Ok(inventory)
    }
    fn validate_relative(&self, relative: &Path) -> Result<()> {
        self.checkpoint()?;
        let text = relative
            .to_str()
            .context("language member path is not UTF-8")?;
        ensure!(
            !text.is_empty()
                && !relative.is_absolute()
                && !text.as_bytes().contains(&0)
                && !text.split('/').any(|part| matches!(part, "" | "." | ".."))
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "unsafe language member path"
        );
        ensure!(
            text.len() <= self.limits.max_path_bytes
                && relative.components().count() <= self.limits.max_components,
            "language member path budget exceeded"
        );
        Ok(())
    }
    // Charge the complete planned path before creating even its first parent.
    fn preflight_creation(
        &self,
        relative: &Path,
        kind: EntryKind,
        target_bytes: usize,
    ) -> Result<()> {
        self.validate_relative(relative)?;
        let mut path = PathBuf::new();
        let mut bytes = self
            .path_bytes
            .checked_add(target_bytes)
            .filter(|v| *v <= self.limits.max_total_path_bytes)
            .context("language link target exceeds total path budget")?;
        let mut files = self.file_count;
        let mut directories = self.directory_count;
        let count = relative.components().count();
        for (index, part) in relative.components().enumerate() {
            path.push(part.as_os_str());
            let desired = if index + 1 == count {
                kind
            } else {
                EntryKind::Directory
            };
            if let Some(entry) = self.entries.get(&path) {
                ensure!(
                    entry.kind == desired && desired == EntryKind::Directory,
                    "duplicate or conflicting language member"
                );
                continue;
            }
            bytes = bytes
                .checked_add(path.as_os_str().as_encoded_bytes().len())
                .filter(|v| *v <= self.limits.max_total_path_bytes)
                .context("language total path byte budget exceeded")?;
            if desired == EntryKind::Directory {
                directories = directories
                    .checked_add(1)
                    .filter(|v| *v <= self.limits.max_directories)
                    .context("language directory count budget exceeded")?;
            } else {
                files = files
                    .checked_add(1)
                    .filter(|v| *v <= self.limits.max_files)
                    .context("language file count budget exceeded")?;
            }
        }
        Ok(())
    }
    fn register(&mut self, relative: &Path, entry: OwnedEntry) -> Result<()> {
        ensure!(
            !self.entries.contains_key(relative),
            "duplicate language member"
        );
        self.path_bytes = self
            .path_bytes
            .checked_add(relative.as_os_str().as_encoded_bytes().len())
            .and_then(|value| {
                value.checked_add(
                    entry
                        .target
                        .as_ref()
                        .map_or(0, |target| target.as_os_str().as_encoded_bytes().len()),
                )
            })
            .filter(|v| *v <= self.limits.max_total_path_bytes)
            .context("language total path byte budget exceeded")?;
        if entry.kind == EntryKind::Directory {
            self.directory_count += 1;
        } else {
            self.file_count += 1;
        }
        self.entries.insert(relative.to_owned(), entry);
        Ok(())
    }
    pub(super) fn open_parent(&self, relative: &Path) -> Result<File> {
        self.validate_relative(relative)?;
        let mut parent = self.root.try_clone()?;
        let mut path = PathBuf::new();
        for part in relative.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(name) = part else {
                bail!("unsafe language member parent")
            };
            path.push(name);
            let tracked = self
                .entries
                .get(&path)
                .context("language member parent is not owned")?;
            ensure!(
                tracked.kind == EntryKind::Directory,
                "language member parent is not a directory"
            );
            let name = name
                .to_str()
                .context("language member parent is not UTF-8")?;
            ensure_entry_identity(&parent, name, tracked.identity)?;
            let opened = open_directory(&parent, name)?;
            ensure!(
                FileIdentity::of(&opened)? == tracked.identity
                    && opened.metadata()?.mode() & 0o7777 == tracked.mode,
                "language member parent changed during open"
            );
            ensure_entry_identity(&parent, name, tracked.identity)?;
            validate_owned_directory(&opened, uid())?;
            parent = opened;
        }
        Ok(parent)
    }
    fn recheck_parent(&self, relative: &Path, held: &File) -> Result<()> {
        ensure!(
            FileIdentity::of(&self.open_parent(relative)?)? == FileIdentity::of(held)?,
            "language member parent moved outside its payload"
        );
        Ok(())
    }
    pub(crate) fn ensure_parents(&mut self, relative: &Path) -> Result<()> {
        self.validate_relative(relative)?;
        if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.create_directories(parent, None)?;
        }
        Ok(())
    }
    pub(crate) fn create_dir(&mut self, relative: &Path, mode: u32) -> Result<()> {
        ensure!(
            mode & !0o777 == 0 && mode & 0o022 == 0 && mode & 0o500 == 0o500,
            "unsafe language directory mode"
        );
        self.create_directories(relative, Some(mode))
    }
    fn create_directories(&mut self, relative: &Path, explicit_mode: Option<u32>) -> Result<()> {
        self.preflight_creation(relative, EntryKind::Directory, 0)?;
        let mut parent = self.root.try_clone()?;
        let mut path = PathBuf::new();
        let count = relative.components().count();
        for (index, part) in relative.components().enumerate() {
            self.checkpoint()?;
            let Component::Normal(name) = part else {
                bail!("unsafe language directory path")
            };
            path.push(name);
            let name = name
                .to_str()
                .context("language directory name is not UTF-8")?;
            let declared = (index + 1 == count).then_some(explicit_mode).flatten();
            let existing = self.entries.get(&path).cloned();
            let directory = match &existing {
                Some(entry) => {
                    ensure!(
                        entry.kind == EntryKind::Directory,
                        "language directory conflicts with another member"
                    );
                    ensure_entry_identity(&parent, name, entry.identity)?;
                    let directory = open_directory(&parent, name)?;
                    ensure!(
                        FileIdentity::of(&directory)? == entry.identity
                            && directory.metadata()?.mode() & 0o7777 == entry.mode,
                        "owned language directory changed"
                    );
                    ensure!(
                        !entry.explicit_directory || declared.is_none_or(|mode| mode == entry.mode),
                        "conflicting explicit language directory modes"
                    );
                    directory
                }
                None => {
                    let mode = declared.unwrap_or(0o755);
                    mkdirat(&parent, name, Mode::from_bits_truncate(mode as _))?;
                    let directory = open_directory(&parent, name)?;
                    fchmod(&directory, Mode::from_bits_truncate(mode as _))?;
                    self.register(
                        &path,
                        OwnedEntry {
                            identity: FileIdentity::of(&directory)?,
                            kind: EntryKind::Directory,
                            mode,
                            target: None,
                            explicit_directory: declared.is_some(),
                        },
                    )?;
                    directory
                }
            };
            if let Some(mode) = declared {
                fchmod(&directory, Mode::from_bits_truncate(mode as _))?;
                let entry = self
                    .entries
                    .get_mut(&path)
                    .expect("directory was registered");
                entry.mode = mode;
                entry.explicit_directory = true;
            }
            ensure_entry_identity(&parent, name, FileIdentity::of(&directory)?)?;
            directory.sync_all()?;
            parent.sync_all()?;
            parent = directory;
        }
        self.checkpoint()
    }
    pub(crate) fn create_file_exclusive(
        &mut self,
        relative: &Path,
        mode: u32,
    ) -> Result<LanguageFile<'_>> {
        self.preflight_creation(relative, EntryKind::File, 0)?;
        ensure!(
            mode & !0o777 == 0 && mode & 0o022 == 0 && mode & 0o400 != 0,
            "unsafe language file mode"
        );
        self.ensure_parents(relative)?;
        let parent = self.open_parent(relative)?;
        let name = relative
            .file_name()
            .context("language member has no name")?
            .to_str()
            .context("language member is not UTF-8")?
            .to_owned();
        let file = File::from(openat(
            &parent,
            name.as_str(),
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::from_bits_truncate(mode as _),
        )?);
        fchmod(&file, Mode::from_bits_truncate(mode as _))?;
        validate_payload_file(&file)?;
        let identity = FileIdentity::of(&file)?;
        self.register(
            relative,
            OwnedEntry {
                identity,
                kind: EntryKind::File,
                mode,
                target: None,
                explicit_directory: false,
            },
        )?;
        self.recheck_parent(relative, &parent)?;
        Ok(LanguageFile {
            owner: self,
            file,
            parent,
            relative: relative.to_owned(),
            name,
            identity,
            size: 0,
            hash: Sha256::new(),
            finished: false,
        })
    }
    pub(crate) fn create_symlink(&mut self, relative: &Path, target: &Path) -> Result<()> {
        self.preflight_creation(
            relative,
            EntryKind::Symlink,
            target.as_os_str().as_encoded_bytes().len(),
        )?;
        let target_text = target
            .to_str()
            .context("language link target is not UTF-8")?;
        ensure!(
            !target_text.is_empty()
                && !target.is_absolute()
                && !target_text.as_bytes().contains(&0),
            "language link target is not relative"
        );
        ensure!(
            target_text.len() <= self.limits.max_path_bytes
                && target.components().count() <= self.limits.max_components,
            "language link target budget exceeded"
        );
        let mut depth = relative.parent().map_or(0, |p| p.components().count());
        for component in target.components() {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir => {
                    ensure!(depth > 0, "language link escapes its payload");
                    depth -= 1;
                }
                _ => bail!("unsafe language link target"),
            }
        }
        self.ensure_parents(relative)?;
        let parent = self.open_parent(relative)?;
        let name = relative
            .file_name()
            .context("language link has no name")?
            .to_str()
            .context("language link name is not UTF-8")?;
        nix::unistd::symlinkat(target, &parent, name)?;
        let stat = fstatat(&parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
        self.register(
            relative,
            OwnedEntry {
                identity: FileIdentity {
                    device: stat.st_dev,
                    inode: stat.st_ino,
                },
                kind: EntryKind::Symlink,
                mode: (stat.st_mode as u32) & 0o7777,
                target: Some(target.to_owned()),
                explicit_directory: false,
            },
        )?;
        self.recheck_parent(relative, &parent)?;
        parent.sync_all()?;
        self.checkpoint()
    }
    fn read_finished_file(&self, relative: &Path, max_bytes: u64) -> Result<(File, File, Vec<u8>)> {
        ensure!(
            max_bytes > 0 && max_bytes < u64::MAX,
            "invalid unpublished read budget"
        );
        let entry = self
            .entries
            .get(relative)
            .context("language file is not owned")?;
        ensure!(
            entry.kind == EntryKind::File,
            "language member is not a regular file"
        );
        let expected = *self
            .files
            .borrow()
            .get(relative)
            .context("language file is not finished")?;
        ensure!(
            expected.size <= max_bytes,
            "language read exceeds its byte budget"
        );
        let parent = self.open_parent(relative)?;
        let name = relative
            .file_name()
            .context("language file has no name")?
            .to_str()
            .context("language file name is not UTF-8")?;
        ensure_entry_identity(&parent, name, entry.identity)?;
        let mut file = File::from(openat(
            &parent,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        validate_payload_file(&file)?;
        ensure!(
            FileIdentity::of(&file)? == entry.identity
                && file.metadata()?.mode() & 0o7777 == entry.mode
                && file.metadata()?.len() == expected.size,
            "language read identity, mode or size changed"
        );
        let mut bytes = Vec::new();
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            self.recheck_parent(relative, &parent)?;
            let amount = (expected
                .size
                .saturating_sub(bytes.len() as u64)
                .saturating_add(1))
            .min(buffer.len() as u64) as usize;
            let count = file.read(&mut buffer[..amount])?;
            if count == 0 {
                break;
            }
            ensure!(
                (bytes.len() as u64).saturating_add(count as u64) <= expected.size,
                "language file read exceeds its exact size"
            );
            hash.update(&buffer[..count]);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let digest: [u8; 32] = hash.finalize().into();
        ensure!(
            bytes.len() as u64 == expected.size && digest == expected.sha256,
            "language file bytes differ from its completed write receipt"
        );
        validate_payload_file(&file)?;
        ensure_entry_identity(&parent, name, entry.identity)?;
        self.recheck_parent(relative, &parent)?;
        Ok((file, parent, bytes))
    }
    pub(crate) fn read_file(&self, relative: &Path, max_bytes: u64) -> Result<Vec<u8>> {
        Ok(self.read_finished_file(relative, max_bytes)?.2)
    }
    pub(crate) fn replace_file(
        &mut self,
        relative: &Path,
        expected: &[u8],
        bytes: &[u8],
    ) -> Result<()> {
        ensure!(
            bytes.len() as u64 <= self.limits.max_file_bytes,
            "language replacement exceeds file budget"
        );
        ensure!(
            self.read_file(relative, self.limits.max_file_bytes)? == expected,
            "unpublished file changed before replacement"
        );
        let entry = self
            .entries
            .get(relative)
            .context("language file is not owned")?
            .clone();
        let parent = self.open_parent(relative)?;
        let name = relative
            .file_name()
            .context("language file has no name")?
            .to_str()
            .context("language file name is not UTF-8")?;
        let mut file = File::from(openat(
            &parent,
            name,
            OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        validate_payload_file(&file)?;
        ensure!(
            FileIdentity::of(&file)? == entry.identity
                && file.metadata()?.mode() & 0o7777 == entry.mode,
            "language replacement inode or mode changed"
        );
        // Recheck exact bytes on the same opened inode before truncating it.
        let mut offset = 0usize;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            self.recheck_parent(relative, &parent)?;
            let amount = expected
                .len()
                .saturating_sub(offset)
                .saturating_add(1)
                .min(buffer.len());
            let count = file.read(&mut buffer[..amount])?;
            if count == 0 {
                break;
            }
            ensure!(
                offset
                    .checked_add(count)
                    .is_some_and(|end| end <= expected.len())
                    && &buffer[..count] == &expected[offset..offset + count],
                "language replacement expected bytes changed"
            );
            offset += count;
        }
        ensure!(
            offset == expected.len(),
            "language replacement expected size changed"
        );
        let total = self
            .total_bytes
            .get()
            .checked_sub(expected.len() as u64)
            .and_then(|v| v.checked_add(bytes.len() as u64))
            .filter(|v| *v <= self.limits.max_total_bytes)
            .context("language replacement exceeds total budget")?;
        let result = (|| {
            self.recheck_parent(relative, &parent)?;
            file.seek(SeekFrom::Start(0))?;
            file.set_len(0)?;
            for chunk in bytes.chunks(64 * 1024) {
                self.recheck_parent(relative, &parent)?;
                file.write_all(chunk)?;
            }
            file.sync_all()?;
            ensure_entry_identity(&parent, name, entry.identity)?;
            self.recheck_parent(relative, &parent)
        })();
        if result.is_err() {
            self.poisoned.set(true);
        } else {
            self.total_bytes.set(total);
            self.files.borrow_mut().insert(
                relative.to_owned(),
                WrittenFile {
                    size: bytes.len() as u64,
                    sha256: Sha256::digest(bytes).into(),
                },
            );
        }
        result
    }
    fn discard_unpublished(&self) -> Result<()> {
        ensure!(
            !self.retain_allocation.get(),
            "publication has begun; retain its allocation"
        );
        self.capture_inventory()?;
        for (relative, entry) in self.entries.iter().rev() {
            self.checkpoint()?;
            let parent = self.open_parent(relative)?;
            let name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .context("owned cleanup name is not UTF-8")?;
            ensure_entry_identity(&parent, name, entry.identity)?;
            self.recheck_parent(relative, &parent)?;
            let flags = if entry.kind == EntryKind::Directory {
                nix::unistd::UnlinkatFlags::RemoveDir
            } else {
                nix::unistd::UnlinkatFlags::NoRemoveDir
            };
            nix::unistd::unlinkat(&parent, name, flags)?;
            parent.sync_all()?;
        }
        self.checkpoint()?;
        nix::unistd::unlinkat(
            &self.allocation,
            "payload",
            nix::unistd::UnlinkatFlags::RemoveDir,
        )?;
        self.allocation.sync_all()?;
        self.writer.check_binding()?;
        self.held.check_binding()?;
        self.context.checkpoint()?;
        ensure_entry_identity(
            self.writer.root_handle(),
            &self.spec.key.filename(),
            FileIdentity::of(&self.allocation)?,
        )?;
        nix::unistd::unlinkat(
            self.writer.root_handle(),
            self.spec.key.filename().as_str(),
            nix::unistd::UnlinkatFlags::RemoveDir,
        )?;
        self.writer.root_handle().sync_all()?;
        Ok(())
    }
}
impl Drop for LanguageBuildOwner {
    fn drop(&mut self) {
        if !self.retain_allocation.get() {
            let _ = self.discard_unpublished();
        }
    }
}
impl InventoryRoot for LanguageBuildOwner {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        self.path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .expect("key is ASCII")
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.checkpoint()
    }
}
pub(crate) struct LanguageFile<'a> {
    owner: &'a LanguageBuildOwner,
    file: File,
    parent: File,
    relative: PathBuf,
    name: String,
    identity: FileIdentity,
    size: u64,
    hash: Sha256,
    finished: bool,
}
impl LanguageFile<'_> {
    fn check_io_binding(&self) -> Result<()> {
        self.owner.recheck_parent(&self.relative, &self.parent)?;
        let entry = self
            .owner
            .entries
            .get(&self.relative)
            .context("language writer entry is not owned")?;
        validate_payload_file(&self.file)?;
        ensure!(
            FileIdentity::of(&self.file)? == self.identity
                && entry.identity == self.identity
                && self.file.metadata()?.mode() & 0o7777 == entry.mode
                && self.file.metadata()?.len() == self.size,
            "language writer file identity, mode or size changed"
        );
        ensure_entry_identity(&self.parent, &self.name, self.identity)
    }
}
impl Write for LanguageFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| -> Result<usize> {
            self.check_io_binding()?;
            let limits = self.owner.limits;
            ensure!(
                self.size
                    .checked_add(bytes.len() as u64)
                    .is_some_and(|v| v <= limits.max_file_bytes)
                    && self
                        .owner
                        .total_bytes
                        .get()
                        .checked_add(bytes.len() as u64)
                        .is_some_and(|v| v <= limits.max_total_bytes),
                "language write byte budget exceeded"
            );
            let count = self.file.write(bytes)?;
            self.hash.update(&bytes[..count]);
            self.size += count as u64;
            self.owner
                .total_bytes
                .set(self.owner.total_bytes.get() + count as u64);
            self.check_io_binding()?;
            Ok(count)
        })();
        result.map_err(|error| {
            self.owner.poisoned.set(true);
            io::Error::other(error.to_string())
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = (|| -> Result<()> {
            self.check_io_binding()?;
            self.file.flush()?;
            self.check_io_binding()
        })();
        result.map_err(|error| {
            self.owner.poisoned.set(true);
            io::Error::other(error.to_string())
        })
    }
}
impl LanguageFile<'_> {
    pub(crate) fn finish(mut self) -> Result<()> {
        self.check_io_binding()?;
        self.file.flush()?;
        self.file.sync_all()?;
        self.parent.sync_all()?;
        self.check_io_binding()?;
        self.owner.files.borrow_mut().insert(
            self.relative.clone(),
            WrittenFile {
                size: self.size,
                sha256: self.hash.clone().finalize().into(),
            },
        );
        self.finished = true;
        Ok(())
    }
}
impl Drop for LanguageFile<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.owner.poisoned.set(true);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
