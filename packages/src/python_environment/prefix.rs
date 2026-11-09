//! Owned unpublished final-prefix allocation for the package materializer.
//!
//! Allocator and writer implementation follows independently observed RED.
//! Discard captures the candidate, checks ownership, and only removes tracked
//! entries; unknown or changed entries are retained for explicit recovery.
//! This type never means runtime validation or environment publication.

#[cfg(unix)]
use pakro_store::control::lease::{LeaseMode, StableLock, lock_file_with_deadline};
#[cfg(unix)]
use pakro_store::control::{
    FileIdentity, RootLimits, StoreWriter, ensure_entry_identity, open_directory,
    open_existing_owned_regular, open_private_directory, open_trusted_store, rename_noclobber,
    validate_owned_directory, validate_owned_regular,
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
use std::time::Instant;

#[cfg(unix)]
use std::collections::BTreeMap;

use eyre::{Context, Result, bail, ensure};

pub(crate) mod control;
pub(crate) mod verify;

#[cfg(unix)]
mod clone;

#[cfg(unix)]
mod observer;
#[cfg(unix)]
pub(crate) use observer::OwnershipObserverLimits;
#[cfg(unix)]
pub(crate) use observer::recovery::{OwnershipRecoveryLimits, OwnershipRecoveryOutcome};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(all(unix, test))]
use nix::fcntl::FlockArg;
#[cfg(unix)]
use nix::fcntl::{AtFlags, Flock, OFlag, openat};
#[cfg(unix)]
use nix::sys::stat::{Mode, fchmod, fstatat, mkdirat};

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrefixLimits {
    pub max_components: usize,
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_total_bytes: u64,
    pub max_cleanup_entries: usize,
}

impl Default for PrefixLimits {
    fn default() -> Self {
        Self {
            max_components: 256,
            max_entries: 1_048_576,
            max_path_bytes: 16_384,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
            max_cleanup_entries: 1_048_576,
        }
    }
}

#[derive(Debug)]
pub(crate) struct EnvironmentStore {
    root_path: PathBuf,
    expected_uid: u32,
    limits: PrefixLimits,
    #[cfg(unix)]
    root: File,
    #[cfg(unix)]
    kernel: Arc<StoreWriter>,
}

impl EnvironmentStore {
    #[cfg(unix)]
    pub(crate) fn open_readonly(
        root: &Path,
        expected_uid: u32,
        limits: PrefixLimits,
        deadline: Instant,
    ) -> Result<Option<pakro_store::control::ReadStore>> {
        validate_store_input(root, limits)?;
        pakro_store::control::ReadStore::open_readonly(
            root,
            expected_uid,
            RootLimits {
                max_components: limits.max_components,
                max_path_bytes: limits.max_path_bytes,
            },
            deadline,
        )
    }

    pub(crate) fn open_writer(
        root: &Path,
        expected_uid: u32,
        limits: PrefixLimits,
        deadline: Instant,
    ) -> Result<Self> {
        validate_store_input(root, limits)?;
        Self::open_shared_writer(root, expected_uid, limits, deadline)
    }

    /// Shared allocations use the UTF-8-capable R05 root control. Consumers
    /// still enforce their own script-format restrictions; the existing
    /// runtime bootstrap entry keeps its ASCII-only gate.
    pub(crate) fn open_shared_writer(
        root: &Path,
        expected_uid: u32,
        limits: PrefixLimits,
        deadline: Instant,
    ) -> Result<Self> {
        validate_store_input_policy(root, limits, false)?;
        #[cfg(unix)]
        {
            let kernel = Arc::new(StoreWriter::open_writer(
                root,
                expected_uid,
                RootLimits {
                    max_components: limits.max_components,
                    max_path_bytes: limits.max_path_bytes,
                },
                deadline,
            )?);
            Ok(Self {
                root_path: kernel.root_path().to_path_buf(),
                expected_uid,
                limits,
                root: kernel.root_handle().try_clone()?,
                kernel,
            })
        }
        #[cfg(not(unix))]
        bail!("unpublished environment allocation requires a supported Unix platform")
    }

    // Existing unit fixtures start an explicit bounded test operation here.
    // Production callers must supply their own original absolute deadline.
    #[cfg(test)]
    pub(crate) fn open(root: &Path, expected_uid: u32, limits: PrefixLimits) -> Result<Self> {
        Self::open_writer(
            root,
            expected_uid,
            limits,
            Instant::now() + std::time::Duration::from_secs(1200),
        )
    }

    pub(crate) fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub(crate) fn allocate(&self) -> Result<UnpublishedPrefix> {
        #[cfg(unix)]
        {
            for _ in 0..16 {
                let name = format!("g-{}", uuid::Uuid::now_v7().simple());
                match self.allocate_named(&name) {
                    Err(error)
                        if error.downcast_ref::<nix::errno::Errno>()
                            == Some(&nix::errno::Errno::EEXIST) => {}
                    result => return result,
                }
            }
            bail!("environment allocation exceeded its collision retry limit")
        }
        #[cfg(not(unix))]
        bail!("unpublished environment allocation requires a supported Unix platform")
    }

    fn allocate_named(&self, name: &str) -> Result<UnpublishedPrefix> {
        ensure!(
            name.len() == 34
                && name.starts_with("g-")
                && name.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit),
            "invalid unpublished allocation identifier"
        );
        let final_path = self.root_path.join(name);
        ensure!(
            final_path.as_os_str().as_encoded_bytes().len() <= self.limits.max_path_bytes,
            "environment allocation path exceeds its byte limit"
        );
        #[cfg(unix)]
        {
            self.check_store_binding()?;
            // Existing allocations never acquire newly manufactured protection,
            // even when their registered lock is missing.
            match fstatat(&self.root, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Err(nix::errno::Errno::ENOENT) => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Err(nix::errno::Errno::EEXIST.into()),
            }
            // The writer initialized these stable directories; allocation never
            // repairs a missing object-lock namespace.
            let leases = open_directory(&self.root, ".leases")?;
            let build = open_directory(&leases, "build")?;
            ensure!(
                FileIdentity::of(&leases)? == FileIdentity::of(self.kernel.leases_handle())?
                    && FileIdentity::of(&build)? == FileIdentity::of(self.kernel.build_handle())?,
                "environment object lock namespace identity changed"
            );
            self.check_store_binding()?;
            let lease_name = format!("{name}.lock");
            // The shared primitive persists the stable lock and its parent
            // before any generation can be created under this name.
            let build_lock = StableLock::create_new(&build, &lease_name, self.expected_uid)?;
            let lease = open_existing_owned_regular(&build, &lease_name, self.expected_uid)?;
            ensure!(
                FileIdentity::of(&lease)? == build_lock.identity(),
                "environment build lock identity changed"
            );
            let mut build_lease = Some(lock_file_with_deadline(
                lease,
                LeaseMode::Exclusive,
                self.kernel.deadline(),
            )?);
            build_lock.check_binding()?;
            ensure!(
                FileIdentity::of(build_lease.as_ref().expect("newly acquired build lease"))?
                    == build_lock.identity(),
                "environment build lease identity changed"
            );
            self.check_store_binding()?;
            // A newly allocated object is protected before entering catalog.
            let _lock = self
                .kernel
                .catalog_lock()
                .acquire(LeaseMode::Exclusive, self.kernel.deadline())?;
            self.check_store_binding()?;
            build_lock.check_binding()?;
            mkdirat(&self.root, name, Mode::from_bits_truncate(0o700))?;
            let root = open_directory(&self.root, name)
                .wrap_err("could not open the exclusively allocated environment")?;
            let identity = FileIdentity::of(&root)?;
            let result = (|| {
                fchmod(&root, Mode::from_bits_truncate(0o700))?;
                validate_owned_directory(&root, self.expected_uid)?;
                ensure_entry_identity(&self.root, name, identity)?;
                self.check_store_binding()?;
                build_lock.check_binding()?;
                ensure_entry_identity(&self.root, name, identity)?;
                Ok(UnpublishedPrefix {
                    final_path,
                    allocation_id: name.into(),
                    binding: PrefixBinding {
                        kernel: self.kernel.clone(),
                        store_root: self.root.try_clone()?,
                        root,
                        identity,
                        build_lock: build_lock.clone(),
                        _build_directory: build,
                        _build_lease: build_lease.take().expect("newly acquired build lease"),
                        _owner_nonce: uuid::Uuid::now_v7(),
                    },
                    expected_uid: self.expected_uid,
                    limits: self.limits,
                    entries: BTreeMap::new(),
                    written_bytes: 0,
                    tainted: false,
                    discarded: false,
                    workspace_cleanup: false,
                    journal_retained: false,
                    recovery: None,
                    ownership: observer::OwnershipRegistration::Unbound,
                })
            })();
            if result.is_err()
                && build_lease.is_some()
                && self.check_store_binding().is_ok()
                && build_lock.check_binding().is_ok()
                && ensure_entry_identity(&self.root, name, identity).is_ok()
            {
                // Only an empty directory we just created can be removed here.
                // General discard/recursive cleanup remains a separate RED gate.
                let _ =
                    nix::unistd::unlinkat(&self.root, name, nix::unistd::UnlinkatFlags::RemoveDir);
            }
            result
        }
        #[cfg(not(unix))]
        bail!("unpublished environment allocation requires a supported Unix platform")
    }

    #[cfg(unix)]
    fn check_store_binding(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.kernel.deadline(),
            "store operation deadline exceeded"
        );
        self.kernel.check_binding()?;
        ensure!(
            FileIdentity::of(self.kernel.root_handle())? == FileIdentity::of(&self.root)?,
            "environment store identity changed"
        );
        Ok(())
    }
}

fn validate_store_input(root: &Path, limits: PrefixLimits) -> Result<()> {
    validate_store_input_policy(root, limits, true)
}
fn validate_store_input_policy(root: &Path, limits: PrefixLimits, ascii: bool) -> Result<()> {
    ensure!(
        limits.max_components != 0
            && limits.max_entries != 0
            && limits.max_path_bytes != 0
            && limits.max_total_bytes != 0
            && limits.max_cleanup_entries != 0,
        "environment prefix limits must be nonzero"
    );
    let bytes = root.as_os_str().as_encoded_bytes();
    ensure!(
        root.is_absolute() && (!ascii || bytes.is_ascii()) && !bytes.contains(&0),
        "environment root must be an absolute ASCII path"
    );
    ensure!(
        !bytes
            .split(|byte| *byte == b'/')
            .any(|part| part == b"." || part == b".."),
        "environment root contains an unsafe path component"
    );
    ensure!(
        root.components()
            .count()
            .checked_add(1)
            .is_some_and(|count| count <= limits.max_components),
        "environment root exceeds its component limit"
    );
    ensure!(
        bytes
            .len()
            .checked_add(35)
            .is_some_and(|count| count <= limits.max_path_bytes),
        "environment allocation path exceeds its byte limit"
    );
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct PrefixBinding {
    kernel: Arc<StoreWriter>,
    store_root: File,
    root: File,
    identity: FileIdentity,
    build_lock: StableLock,
    _build_directory: File,
    _build_lease: Flock<File>,
    _owner_nonce: uuid::Uuid,
}

#[cfg(unix)]
impl PrefixBinding {
    fn check_build_protection(&self) -> Result<()> {
        self.kernel.check_binding()?;
        self.build_lock.check_binding()?;
        ensure!(
            FileIdentity::of(&self.store_root)? == FileIdentity::of(self.kernel.root_handle())?
                && FileIdentity::of(&self._build_directory)?
                    == FileIdentity::of(self.kernel.build_handle())?
                && FileIdentity::of(&self._build_lease)? == self.build_lock.identity(),
            "environment build protection identity changed"
        );
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct TrackedEntry {
    identity: FileIdentity,
    kind: EntryKind,
}

#[cfg(unix)]
#[derive(Debug)]
struct Recovery {
    directory: File,
    name: String,
    path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct UnpublishedPrefix {
    final_path: PathBuf,
    allocation_id: String,
    #[cfg(unix)]
    binding: PrefixBinding,
    #[cfg(unix)]
    expected_uid: u32,
    #[cfg(unix)]
    limits: PrefixLimits,
    #[cfg(unix)]
    entries: BTreeMap<PathBuf, TrackedEntry>,
    #[cfg(unix)]
    written_bytes: u64,
    #[cfg(unix)]
    tainted: bool,
    #[cfg(unix)]
    discarded: bool,
    workspace_cleanup: bool,
    journal_retained: bool,
    #[cfg(unix)]
    recovery: Option<Recovery>,
    #[cfg(unix)]
    ownership: observer::OwnershipRegistration,
}

#[derive(Debug, thiserror::Error)]
#[error("another complete shared publication already occupies this key")]
pub(crate) struct SharedPublicationConflict;

/// Keeps the original allocation EX until every member of a shared group
/// has completed publication. Dropping this guard releases protection but
/// never invokes temporary-output cleanup on Ready content.
pub(crate) struct SharedPublishedPrefix {
    prefix: UnpublishedPrefix,
    name: String,
    bytes: Vec<u8>,
}
impl SharedPublishedPrefix {
    /// Revoke only this operation's exact Ready CAS before temporary cleanup.
    pub(crate) fn rollback(mut self) -> Result<()> {
        self.prefix.binding.check_build_protection()?;
        ensure_entry_identity(
            &self.prefix.binding.store_root,
            &self.prefix.allocation_id,
            self.prefix.binding.identity,
        )?;
        let mut control = self
            .prefix
            .binding
            .kernel
            .control(control::ControlLimits::default())?;
        let current = control
            .read(control::ControlNamespace::Catalog, &self.name)?
            .ok_or_else(|| eyre::eyre!("shared publication disappeared before rollback"))?;
        ensure!(
            current.bytes() == self.bytes,
            "shared publication changed before rollback"
        );
        control.remove(&current)?;
        self.prefix.discarded = false;
        drop(control);
        Ok(())
    }
}

impl UnpublishedPrefix {
    /// Native dependency environments share the enclosing source workspace's
    /// watcher lifetime; their destructor must never race its process cleanup.
    pub(crate) fn delegate_cleanup_to_workspace(&mut self, workspace: &Path) -> Result<()> {
        ensure!(
            self.final_path.starts_with(workspace) && self.final_path != workspace,
            "delegated prefix is outside its owned source workspace"
        );
        self.workspace_cleanup = true;
        Ok(())
    }

    pub(crate) fn final_path(&self) -> &Path {
        &self.final_path
    }
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation_id
    }

    /// A durable publication journal owns cleanup until its recovery completes.
    pub(crate) fn retain_for_publication_journal(&mut self) -> Result<()> {
        self.check_binding()?;
        self.journal_retained = true;
        Ok(())
    }

    pub(crate) fn release_publication_journal_retention(&mut self) {
        self.journal_retained = false;
    }
    pub(crate) fn check_binding(&self) -> Result<()> {
        #[cfg(unix)]
        {
            self.check_write_ready()?;
            self.validate_tracked_tree()?;
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("unpublished environment writes require a supported Unix platform")
    }

    /// Publish one immutable shared object using the same controlled writer.
    /// The Ready CAS and disabling temporary cleanup are one operation; a
    /// caller cannot accidentally delete a committed tree during handoff.
    pub(crate) fn publish_shared(
        self,
        name: &str,
        bytes: &[u8],
        inventory: &verify::TreeInventory,
    ) -> Result<SharedPublishedPrefix> {
        self.publish_shared_with_after_cas(name, bytes, inventory, || Ok(()))
    }

    pub(crate) fn publish_shared_with_after_cas(
        mut self,
        name: &str,
        bytes: &[u8],
        inventory: &verify::TreeInventory,
        after_cas: impl FnOnce() -> Result<()>,
    ) -> Result<SharedPublishedPrefix> {
        #[derive(serde::Deserialize)]
        struct Envelope {
            storage: Binding,
        }
        #[derive(serde::Deserialize)]
        struct Binding {
            allocation: String,
            inventory: verify::TreeInventory,
        }
        self.check_binding()?;
        let binding: Envelope = serde_json::from_slice(bytes)?;
        ensure!(
            binding.storage.allocation == self.allocation_id
                && &binding.storage.inventory == inventory,
            "shared publication manifest does not bind its owned allocation"
        );
        ensure!(
            verify::observe_tree_inventory(&self, verify::PrefixInventoryLimits::default())?
                == *inventory,
            "shared publication inventory changed"
        );
        let mut control = self
            .binding
            .kernel
            .control(control::ControlLimits::default())?;
        ensure!(
            control
                .read(
                    control::ControlNamespace::Journals,
                    &format!("deleting-{name}")
                )?
                .is_none(),
            "PendingRecovery: shared object has an unfinished deletion"
        );
        // This preflight and the replace share the same catalog EX. A normal
        // competing publisher is distinguishable from an IO error after rename.
        if control
            .read(control::ControlNamespace::Catalog, name)?
            .is_some()
        {
            return Err(SharedPublicationConflict.into());
        }
        let publication = control
            .replace(control::ControlNamespace::Catalog, name, None, bytes)
            .and_then(|_| after_cas());
        if let Err(error) = publication {
            // R05 replace may fail after rename (for example parent fsync or
            // the final bounded read). Under original object EX + catalog EX,
            // only a confirmed absence permits temporary-output cleanup.
            if !matches!(
                control.read(control::ControlNamespace::Catalog, name),
                Ok(None)
            ) {
                self.discarded = true;
                return Err(error.wrap_err(format!(
                    "shared publication failed; retaining its potentially published allocation at {}",
                    self.final_path.display()
                )));
            }
            return Err(error);
        }
        // Nothing fallible can run before disarming Drop after the durable CAS.
        self.discarded = true;
        drop(control);
        Ok(SharedPublishedPrefix {
            prefix: self,
            name: name.into(),
            bytes: bytes.into(),
        })
    }

    /// Irreversibly downgrade a failed caller operation; never grants access.
    pub(crate) fn mark_failed(&mut self) {
        #[cfg(unix)]
        {
            self.tainted = true;
        }
    }

    pub(crate) fn create_dir_all(&mut self, relative: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            self.check_write_ready()?;
            self.validate_relative(relative, true)?;
            let mut mutated = false;
            let mut pending_creation = None;
            let result = (|| {
                let mut directory = self.binding.root.try_clone()?;
                let mut partial = PathBuf::new();
                for component in relative.components() {
                    self.binding.check_build_protection()?;
                    let std::path::Component::Normal(name) = component else {
                        bail!("unsafe relative environment path");
                    };
                    partial.push(name);
                    if let Some(entry) = self.entries.get(&partial) {
                        ensure!(
                            entry.kind == EntryKind::Directory,
                            "environment file occupies a directory"
                        );
                        let opened = open_directory(&directory, name)?;
                        ensure!(
                            FileIdentity::of(&opened)? == entry.identity,
                            "environment directory identity changed"
                        );
                        directory = opened;
                    } else {
                        self.check_entry_budget()?;
                        let operation =
                            self.observe_create_intent(EntryKind::Directory, &partial, &directory)?;
                        pending_creation = operation;
                        mkdirat(&directory, name, Mode::from_bits_truncate(0o755))?;
                        mutated = true;
                        let opened = open_directory(&directory, name)?;
                        let identity = FileIdentity::of(&opened)?;
                        self.entries.insert(
                            partial.clone(),
                            TrackedEntry {
                                identity,
                                kind: EntryKind::Directory,
                            },
                        );
                        fchmod(&opened, Mode::from_bits_truncate(0o755))?;
                        directory.sync_all()?;
                        self.observe_created(operation, &opened)?;
                        pending_creation = None;
                        directory = opened;
                    }
                }
                self.check_namespace()?;
                Ok(())
            })();
            if result.is_err() && mutated {
                self.tainted = true;
            }
            if result.is_err() {
                self.retain_uncertain_creation(pending_creation);
            }
            result
        }
        #[cfg(not(unix))]
        bail!("unpublished environment writes require a supported Unix platform")
    }

    pub(crate) fn create_file_exclusive(
        &mut self,
        relative: &Path,
        mode: u32,
    ) -> Result<PrefixFile<'_>> {
        #[cfg(unix)]
        {
            self.create_file_with_checkpoint(relative, mode, |_| Ok(()))
        }
        #[cfg(not(unix))]
        bail!("unpublished environment writes require a supported Unix platform")
    }

    /// Shared layouts are not attached to an environment ownership journal.
    /// Their links are registered in the allocation and checked as a complete
    /// tree before publication.
    pub(crate) fn create_layout_symlink(&mut self, relative: &Path, target: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            self.check_write_ready()?;
            ensure!(
                matches!(self.ownership, observer::OwnershipRegistration::Unbound),
                "layout symlinks require an unbound shared allocation"
            );
            self.validate_relative(relative, false)?;
            ensure!(
                !target.as_os_str().is_empty() && !target.is_absolute(),
                "layout symlink target must be relative"
            );
            let mut depth = relative
                .parent()
                .map_or(0, |parent| parent.components().count());
            for part in target.components() {
                match part {
                    std::path::Component::Normal(_) => depth += 1,
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir if depth > 0 => depth -= 1,
                    _ => bail!("layout symlink escapes its prefix"),
                }
            }
            ensure!(
                !self.entries.contains_key(relative),
                "environment output was already created by this allocation"
            );
            self.check_entry_budget()?;
            let parent = self.open_owned_parent(relative)?;
            let name = relative
                .file_name()
                .ok_or_else(|| eyre::eyre!("missing environment link name"))?;
            nix::unistd::symlinkat(target, &parent, name)?;
            let stat = match fstatat(&parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(error) => {
                    self.tainted = true;
                    return Err(error.into());
                }
            };
            self.entries.insert(
                relative.into(),
                TrackedEntry {
                    identity: FileIdentity {
                        device: stat.st_dev,
                        inode: stat.st_ino,
                    },
                    kind: EntryKind::Symlink,
                },
            );
            let result: Result<()> = (|| {
                parent.sync_all()?;
                self.check_namespace()?;
                Ok(())
            })();
            if let Err(error) = result {
                self.tainted = true;
                return Err(error.into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("layout symlinks require a supported Unix platform")
    }

    #[cfg(unix)]
    fn create_file_with_checkpoint(
        &mut self,
        relative: &Path,
        mode: u32,
        mut checkpoint: impl FnMut(observer::CreationCheckpoint) -> Result<()>,
    ) -> Result<PrefixFile<'_>> {
        #[cfg(unix)]
        {
            self.check_write_ready()?;
            self.validate_relative(relative, false)?;
            ensure!(mode & !0o777 == 0, "invalid environment file mode");
            ensure!(
                !self.entries.contains_key(relative),
                "environment output was already created by this allocation"
            );
            self.check_entry_budget()?;
            let parent = self.open_owned_parent(relative)?;
            let name = relative
                .file_name()
                .ok_or_else(|| eyre::eyre!("missing environment file name"))?;
            let operation = self.observe_create_intent(EntryKind::File, relative, &parent)?;
            let created = (|| {
                if operation.is_some() {
                    checkpoint(observer::CreationCheckpoint::Intent)?;
                }
                self.check_namespace()?;
                let file = File::from(openat(
                    &parent,
                    name,
                    OFlag::O_WRONLY
                        | OFlag::O_CREAT
                        | OFlag::O_EXCL
                        | OFlag::O_NOFOLLOW
                        | OFlag::O_CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )?);
                let identity = FileIdentity::of(&file)?;
                self.entries.insert(
                    relative.into(),
                    TrackedEntry {
                        identity,
                        kind: EntryKind::File,
                    },
                );
                parent.sync_all()?;
                if operation.is_some() {
                    checkpoint(observer::CreationCheckpoint::CreatedActual)?;
                }
                self.observe_created(operation, &file)?;
                self.check_namespace()?;
                Ok((file, identity))
            })();
            let (file, identity) = match created {
                Ok(created) => created,
                Err(error) => {
                    if self.entries.contains_key(relative) {
                        self.tainted = true;
                    }
                    self.retain_uncertain_creation(operation);
                    return Err(error);
                }
            };
            Ok(PrefixFile {
                prefix: self,
                relative: relative.into(),
                mode,
                file,
                parent,
                identity,
                size: 0,
                finished: false,
                ownership_operation: operation,
            })
        }
        #[cfg(not(unix))]
        bail!("unpublished environment writes require a supported Unix platform")
    }

    pub(crate) fn discard(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            if self.discarded {
                return Ok(());
            }
            match self.ownership {
                observer::OwnershipRegistration::Attached(_) => {
                    return self.discard_observed();
                }
                observer::OwnershipRegistration::Retained { .. } => {
                    bail!("environment retained because its ownership log is unavailable");
                }
                observer::OwnershipRegistration::Unbound => {}
            }
        }
        self.discard_with_capture_hook(|| Ok(()))
    }
    pub(crate) fn recovery_path(&self) -> Option<&Path> {
        #[cfg(unix)]
        {
            self.recovery
                .as_ref()
                .map(|recovery| recovery.path.as_path())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn discard_with_capture_hook(
        &mut self,
        before_capture: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        #[cfg(unix)]
        {
            if self.discarded {
                return Ok(());
            }
            self.binding.check_build_protection()?;
            // The prefix retains its build EX, so catalog is acquired second.
            let _lock = self
                .binding
                .kernel
                .catalog_lock()
                .acquire(LeaseMode::Exclusive, self.binding.kernel.deadline())?;
            self.binding.check_build_protection()?;
            if self.recovery.is_none() {
                self.check_namespace()?;
                let quarantine = open_private_directory(
                    &self.binding.store_root,
                    ".quarantine",
                    self.expected_uid,
                )?;
                let name = format!("q-{}", uuid::Uuid::now_v7().simple());
                before_capture()?;
                self.binding.check_build_protection()?;
                rename_noclobber(
                    &self.binding.store_root,
                    std::ffi::OsStr::new(&self.allocation_id),
                    &quarantine,
                    std::ffi::OsStr::new(&name),
                )?;
                let path = self
                    .final_path
                    .parent()
                    .ok_or_else(|| eyre::eyre!("missing environment store"))?
                    .join(".quarantine")
                    .join(&name);
                self.recovery = Some(Recovery {
                    directory: quarantine,
                    name,
                    path,
                });
                self.binding.check_build_protection()?;
            }
            let recovery = self
                .recovery
                .as_ref()
                .ok_or_else(|| eyre::eyre!("missing environment recovery location"))?;
            if ensure_entry_identity(&recovery.directory, &recovery.name, self.binding.identity)
                .is_err()
            {
                let restored = rename_noclobber(
                    &recovery.directory,
                    std::ffi::OsStr::new(&recovery.name),
                    &self.binding.store_root,
                    std::ffi::OsStr::new(&self.allocation_id),
                );
                if restored.is_ok() {
                    self.recovery = None;
                    bail!(
                        "environment discard captured a changed identity; captured entry restored"
                    );
                }
                bail!(
                    "environment discard captured a changed identity; entry retained at {}",
                    recovery.path.display()
                );
            }
            let retained = recovery.path.clone();
            self.remove_tracked_tree().wrap_err_with(|| {
                format!(
                    "environment retained for recovery at {}",
                    retained.display()
                )
            })?;
            let recovery = self
                .recovery
                .as_ref()
                .ok_or_else(|| eyre::eyre!("missing environment recovery location"))?;
            self.binding.check_build_protection()?;
            ensure_entry_identity(&recovery.directory, &recovery.name, self.binding.identity)?;
            nix::unistd::unlinkat(
                &recovery.directory,
                recovery.name.as_str(),
                nix::unistd::UnlinkatFlags::RemoveDir,
            )?;
            self.recovery = None;
            self.discarded = true;
            self.binding.check_build_protection()?;
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("unpublished environment cleanup requires a supported Unix platform")
    }

    #[cfg(unix)]
    fn check_write_ready(&self) -> Result<()> {
        self.check_namespace()?;
        ensure!(
            !self.tainted,
            "unpublished environment has unfinished or failed output"
        );
        Ok(())
    }

    #[cfg(unix)]
    fn check_namespace(&self) -> Result<()> {
        self.binding.check_build_protection()?;
        ensure!(
            !self.discarded && self.recovery.is_none(),
            "environment is no longer in its unpublished final location"
        );
        let store_path = self
            .final_path
            .parent()
            .ok_or_else(|| eyre::eyre!("missing environment store"))?;
        let current = open_trusted_store(store_path, self.expected_uid, false)?;
        ensure!(
            FileIdentity::of(&current)? == FileIdentity::of(&self.binding.store_root)?,
            "environment store identity changed"
        );
        ensure_entry_identity(
            &self.binding.store_root,
            &self.allocation_id,
            self.binding.identity,
        )?;
        ensure!(
            FileIdentity::of(&self.binding.root)? == self.binding.identity,
            "environment root identity changed"
        );
        Ok(())
    }

    #[cfg(unix)]
    fn validate_relative(&self, relative: &Path, allow_empty: bool) -> Result<()> {
        let bytes = relative.as_os_str().as_encoded_bytes();
        ensure!(
            (allow_empty || !bytes.is_empty()) && !relative.is_absolute() && !bytes.contains(&0),
            "unsafe relative environment path"
        );
        ensure!(
            !bytes
                .split(|byte| *byte == b'/')
                .any(|part| part == b"." || part == b".."),
            "unsafe relative environment component"
        );
        ensure!(
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "unsafe relative environment component"
        );
        ensure!(
            relative.components().count() <= self.limits.max_components,
            "environment relative path exceeds component limit"
        );
        let final_length = self
            .final_path
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .checked_add(1)
            .and_then(|length| length.checked_add(bytes.len()));
        ensure!(
            final_length.is_some_and(|length| length <= self.limits.max_path_bytes),
            "environment relative path exceeds byte limit"
        );
        Ok(())
    }

    #[cfg(unix)]
    fn check_entry_budget(&self) -> Result<()> {
        ensure!(
            self.entries.len() < self.limits.max_entries,
            "environment entry limit exceeded"
        );
        Ok(())
    }

    #[cfg(unix)]
    fn open_owned_parent(&self, relative: &Path) -> Result<File> {
        let parent = relative
            .parent()
            .ok_or_else(|| eyre::eyre!("missing environment parent"))?;
        let mut directory = self.binding.root.try_clone()?;
        let mut partial = PathBuf::new();
        for component in parent.components() {
            let std::path::Component::Normal(name) = component else {
                bail!("unsafe environment parent");
            };
            partial.push(name);
            let tracked = self
                .entries
                .get(&partial)
                .ok_or_else(|| eyre::eyre!("environment parent is not owned by this allocation"))?;
            ensure!(
                tracked.kind == EntryKind::Directory,
                "environment parent is not a directory"
            );
            let opened = open_directory(&directory, name)?;
            ensure!(
                FileIdentity::of(&opened)? == tracked.identity,
                "environment parent identity changed"
            );
            directory = opened;
        }
        Ok(directory)
    }

    #[cfg(unix)]
    fn validate_tracked_tree(&self) -> Result<()> {
        ensure!(
            self.entries.len() <= self.limits.max_cleanup_entries,
            "environment cleanup entry limit exceeded"
        );
        let mut observed = 0usize;
        self.validate_directory_tree(&self.binding.root, Path::new(""), &mut observed)?;
        ensure!(
            observed == self.entries.len(),
            "tracked environment entries are missing"
        );
        Ok(())
    }

    #[cfg(unix)]
    fn validate_directory_tree(
        &self,
        directory: &File,
        relative: &Path,
        observed: &mut usize,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let mut listing = nix::dir::Dir::openat(
            directory,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        for entry in listing.iter() {
            self.binding.check_build_protection()?;
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            *observed = observed
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("environment cleanup entry count overflow"))?;
            ensure!(
                *observed <= self.limits.max_cleanup_entries,
                "environment cleanup entry limit exceeded"
            );
            let name = std::ffi::OsStr::from_bytes(name);
            let path = relative.join(name);
            let tracked = self.entries.get(&path).ok_or_else(|| {
                eyre::eyre!("untracked environment entry cannot be deleted as owned output")
            })?;
            ensure_tracked_identity(directory, name, *tracked)?;
            if tracked.kind == EntryKind::Directory {
                let child = open_directory(directory, name)?;
                ensure!(
                    FileIdentity::of(&child)? == tracked.identity,
                    "environment directory identity changed"
                );
                self.validate_directory_tree(&child, &path, observed)?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn remove_tracked_tree(&mut self) -> Result<()> {
        self.validate_tracked_tree()?;
        let mut removal: Vec<_> = self.entries.keys().cloned().collect();
        removal.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        for relative in removal {
            self.binding.check_build_protection()?;
            let parent = self.open_owned_parent(&relative)?;
            let name = relative
                .file_name()
                .ok_or_else(|| eyre::eyre!("missing tracked environment name"))?;
            let tracked = *self
                .entries
                .get(&relative)
                .ok_or_else(|| eyre::eyre!("missing tracked environment identity"))?;
            ensure_tracked_identity(&parent, name, tracked)?;
            let flags = if tracked.kind == EntryKind::Directory {
                nix::unistd::UnlinkatFlags::RemoveDir
            } else {
                nix::unistd::UnlinkatFlags::NoRemoveDir
            };
            nix::unistd::unlinkat(&parent, name, flags)?;
            self.entries.remove(&relative);
            self.binding.check_build_protection()?;
        }
        self.binding.check_build_protection()?;
        ensure!(
            directory_is_empty(&self.binding.root, self.limits.max_cleanup_entries)?,
            "untracked environment entry appeared during cleanup"
        );
        Ok(())
    }
}

impl Drop for UnpublishedPrefix {
    fn drop(&mut self) {
        if !self.workspace_cleanup && !self.journal_retained {
            let _ = self.discard();
        }
    }
}

#[cfg(unix)]
fn directory_is_empty(directory: &File, max_entries: usize) -> Result<bool> {
    let mut listing = nix::dir::Dir::openat(
        directory,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let mut count = 0usize;
    for entry in listing.iter() {
        let entry = entry?;
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("environment cleanup count overflow"))?;
        ensure!(
            count <= max_entries,
            "environment cleanup entry limit exceeded"
        );
        return Ok(false);
    }
    Ok(true)
}

#[cfg(unix)]
fn ensure_tracked_identity<P: nix::NixPath + ?Sized>(
    parent: &File,
    name: &P,
    tracked: TrackedEntry,
) -> Result<()> {
    let observed = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    let kind = nix::sys::stat::SFlag::from_bits_truncate(observed.st_mode);
    let expected_kind = match tracked.kind {
        EntryKind::File => nix::sys::stat::SFlag::S_IFREG,
        EntryKind::Directory => nix::sys::stat::SFlag::S_IFDIR,
        EntryKind::Symlink => nix::sys::stat::SFlag::S_IFLNK,
    };
    ensure!(
        observed.st_dev == tracked.identity.device
            && observed.st_ino == tracked.identity.inode
            && kind == expected_kind,
        "tracked environment entry identity or type changed"
    );
    Ok(())
}

/// Closed writer: no File, AsFd, path reopening, Clone or unchecked constructor.
pub(crate) struct PrefixFile<'a> {
    prefix: &'a mut UnpublishedPrefix,
    relative: PathBuf,
    mode: u32,
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    parent: File,
    #[cfg(unix)]
    identity: FileIdentity,
    size: u64,
    finished: bool,
    ownership_operation: Option<u64>,
}

impl Write for PrefixFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.check_io_ready()?;
            let additional = u64::try_from(bytes.len()).map_err(io::Error::other)?;
            if self.prefix.tainted
                || self
                    .prefix
                    .written_bytes
                    .checked_add(additional)
                    .is_none_or(|total| total > self.prefix.limits.max_total_bytes)
            {
                self.prefix.tainted = true;
                return Err(io::Error::other(
                    "environment output byte limit exceeded or allocation tainted",
                ));
            }
            match self.file.write(bytes) {
                Ok(written) => {
                    self.size = self
                        .size
                        .checked_add(written as u64)
                        .ok_or_else(|| io::Error::other("environment file byte count overflow"))?;
                    self.prefix.written_bytes += written as u64;
                    self.check_io_ready()?;
                    Ok(written)
                }
                Err(error) => {
                    self.prefix.tainted = true;
                    Err(error)
                }
            }
        }
        #[cfg(not(unix))]
        Err(io::Error::other(
            "unpublished environment writes require a supported Unix platform",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.check_io_ready()?;
            let result = self.file.flush();
            if result.is_err() {
                self.prefix.tainted = true;
            }
            result?;
            self.check_io_ready()
        }
        #[cfg(not(unix))]
        Err(io::Error::other(
            "unpublished environment writes require a supported Unix platform",
        ))
    }
}

impl PrefixFile<'_> {
    #[cfg(unix)]
    fn check_io_ready(&mut self) -> io::Result<()> {
        if let Err(error) = self.prefix.check_write_ready() {
            self.prefix.tainted = true;
            return Err(io::Error::other(error.to_string()));
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<PrefixFileReceipt> {
        #[cfg(unix)]
        {
            self.prefix.check_write_ready()?;
            self.flush()?;
            fchmod(
                &self.file,
                Mode::from_bits_truncate(self.mode as nix::libc::mode_t),
            )?;
            self.file.sync_all()?;
            let metadata = self.file.metadata()?;
            ensure!(
                metadata.is_file()
                    && metadata.len() == self.size
                    && metadata.mode() & 0o777 == self.mode
                    && FileIdentity::of(&self.file)? == self.identity,
                "environment file completion facts changed"
            );
            let current_parent = self.prefix.open_owned_parent(&self.relative)?;
            ensure!(
                FileIdentity::of(&current_parent)? == FileIdentity::of(&self.parent)?,
                "environment file parent identity changed"
            );
            let name = self
                .relative
                .file_name()
                .ok_or_else(|| eyre::eyre!("missing environment output name"))?;
            ensure_tracked_identity(
                &current_parent,
                name,
                TrackedEntry {
                    identity: self.identity,
                    kind: EntryKind::File,
                },
            )?;
            self.prefix
                .observe_finished(self.ownership_operation, self.size, self.mode)?;
            self.prefix.check_write_ready()?;
            self.finished = true;
            Ok(PrefixFileReceipt {
                relative: self.relative.clone(),
                size: self.size,
                mode: self.mode,
            })
        }
        #[cfg(not(unix))]
        bail!("unpublished environment writes require a supported Unix platform")
    }
}

impl Drop for PrefixFile<'_> {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.finished {
            self.prefix.tainted = true;
        }
    }
}

#[derive(Debug)]
pub(crate) struct PrefixFileReceipt {
    relative: PathBuf,
    size: u64,
    mode: u32,
}

impl PrefixFileReceipt {
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
}

#[cfg(all(test, unix))]
mod tests;
