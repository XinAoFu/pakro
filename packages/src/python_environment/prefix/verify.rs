//! Read-only inventory facts for one real unpublished allocation.
//!
//! Verification follows independently observed filesystem behavior RED.
//! A receipt never grants runtime semantics, sealing or publication.

use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, ensure};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use sha2::{Digest as _, Sha256};
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::{File, Metadata};
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::UnpublishedPrefix;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixExpectedFile {
    pub relative: PathBuf,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixExpectedDirectory {
    pub relative: PathBuf,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixExpectedSymlink {
    pub relative: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrefixInventoryLimits {
    pub max_files: usize,
    pub max_directories: usize,
    pub max_components: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for PrefixInventoryLimits {
    fn default() -> Self {
        Self {
            max_files: 1_048_576,
            max_directories: 1_048_576,
            max_components: 256,
            max_path_bytes: 16_384,
            max_total_path_bytes: 64 * 1024 * 1024,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
        }
    }
}

/// An actual pinned tree, supplied only by a live owner or lease. Serialized
/// inventory facts deliberately cannot implement this capability themselves.
#[cfg(unix)]
pub(crate) trait InventoryRoot {
    fn root_file(&self) -> &File;
    fn root_path(&self) -> &Path;
    fn allocation_id(&self) -> &str;
    fn expected_uid(&self) -> u32;
    fn check_binding(&self) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeInventory {
    root: PathBuf,
    allocation: String,
    identity: [u64; 2],
    uid: u32,
    mode: u32,
    entries: Vec<TreeInventoryEntry>,
    total_bytes: u64,
    inventory_sha256: [u8; 32],
}

impl TreeInventory {
    /// Compare only the original root binding for a controlled partial removal.
    /// This is descriptive; the caller still needs its actual deletion EX.
    pub(crate) fn same_root_binding(&self, original: &Self) -> bool {
        self.root == original.root
            && self.allocation == original.allocation
            && self.identity == original.identity
            && self.uid == original.uid
            && self.mode == original.mode
    }
    pub(crate) fn entries(&self) -> &[TreeInventoryEntry] {
        &self.entries
    }
    pub(crate) fn contains_file(&self, relative: &Path) -> bool {
        self.entries.iter().any(|entry| {
            entry.relative == relative && matches!(entry.kind, TreeInventoryKind::File { .. })
        })
    }
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub(crate) fn inventory_sha256(&self) -> &[u8; 32] {
        &self.inventory_sha256
    }

    /// Describe four replacement file identities without granting any write
    /// authority. Recovery must still compare this prediction to the complete
    /// independently observed tree before publishing it.
    #[cfg(unix)]
    pub(crate) fn with_file_identities(
        &self,
        replacements: &[(PathBuf, [u64; 2])],
    ) -> Result<Self> {
        ensure!(
            replacements.len() == 4,
            "bytecode inventory prediction requires exactly four files"
        );
        let mut predicted = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        for (path, identity) in replacements {
            ensure!(
                seen.insert(path),
                "duplicate inventory identity replacement"
            );
            let entry = predicted
                .entries
                .iter_mut()
                .find(|entry| entry.relative == *path)
                .context("inventory identity replacement is not a published file")?;
            ensure!(
                matches!(entry.kind, TreeInventoryKind::File { .. }),
                "inventory identity replacement is not a regular file"
            );
            entry.identity = *identity;
        }
        predicted.refresh_digest();
        Ok(predicted)
    }

    #[cfg(unix)]
    fn refresh_digest(&mut self) {
        let mut digest = Sha256::new();
        digest.update(b"pakro-tree-inventory-v1\0");
        hash_field(&mut digest, self.root.as_os_str().as_encoded_bytes());
        hash_field(&mut digest, self.allocation.as_bytes());
        for identity in self.identity {
            digest.update(identity.to_le_bytes());
        }
        digest.update(self.uid.to_le_bytes());
        digest.update(self.mode.to_le_bytes());
        for entry in &self.entries {
            hash_field(&mut digest, entry.relative.as_os_str().as_encoded_bytes());
            for identity in entry.identity {
                digest.update(identity.to_le_bytes());
            }
            digest.update(entry.uid.to_le_bytes());
            digest.update(entry.mode.to_le_bytes());
            match &entry.kind {
                TreeInventoryKind::Directory => digest.update(b"d"),
                TreeInventoryKind::File { size, sha256 } => {
                    digest.update(b"f");
                    digest.update(size.to_le_bytes());
                    digest.update(sha256);
                }
                TreeInventoryKind::Symlink { target } => {
                    digest.update(b"l");
                    hash_field(&mut digest, target.as_os_str().as_encoded_bytes());
                }
            }
        }
        self.inventory_sha256 = digest.finalize().into();
    }

    /// Compare the complete root and every entry outside a small, explicit
    /// file set. This is a pure comparison, never permission to write or a new
    /// inventory receipt; the caller must independently validate each exception.
    pub(crate) fn ensure_same_root_and_unaffected_entries(
        &self,
        observed: &Self,
        except: &[PathBuf],
    ) -> Result<()> {
        ensure!(
            self.root == observed.root
                && self.allocation == observed.allocation
                && self.identity == observed.identity
                && self.uid == observed.uid
                && self.mode == observed.mode,
            "published inventory root changed outside bytecode recovery"
        );
        let unique: std::collections::BTreeSet<_> = except.iter().map(PathBuf::as_path).collect();
        ensure!(
            !unique.is_empty() && unique.len() == except.len(),
            "inventory exception file set is invalid"
        );
        for path in &unique {
            ensure!(
                self.entries.iter().any(|entry| entry.relative == *path
                    && matches!(entry.kind, TreeInventoryKind::File { .. })),
                "inventory exception is not a published regular file"
            );
        }
        let before: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| !unique.contains(entry.relative.as_path()))
            .collect();
        let after: Vec<_> = observed
            .entries
            .iter()
            .filter(|entry| !unique.contains(entry.relative.as_path()))
            .collect();
        ensure!(
            before == after,
            "published inventory changed outside the four bytecode files"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeInventoryEntry {
    pub(crate) relative: PathBuf,
    pub(crate) identity: [u64; 2],
    pub(crate) uid: u32,
    pub(crate) mode: u32,
    pub(crate) kind: TreeInventoryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TreeInventoryKind {
    Directory,
    File { size: u64, sha256: [u8; 32] },
    Symlink { target: PathBuf },
}

#[cfg(unix)]
pub(crate) fn observe_tree_inventory(
    root: &impl InventoryRoot,
    limits: PrefixInventoryLimits,
) -> Result<TreeInventory> {
    observe_tree(root, limits, None)
}

#[cfg(unix)]
impl InventoryRoot for UnpublishedPrefix {
    fn root_file(&self) -> &File {
        &self.binding.root
    }
    fn root_path(&self) -> &Path {
        self.final_path()
    }
    fn allocation_id(&self) -> &str {
        self.allocation_id()
    }
    fn expected_uid(&self) -> u32 {
        self.expected_uid
    }
    fn check_binding(&self) -> Result<()> {
        self.check_write_ready()
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryStamp {
    identity: [u64; 2],
    uid: u32,
    mode: u32,
    links: u64,
    size: i64,
    modified: [i64; 2],
    changed: [i64; 2],
}
#[cfg(unix)]
impl EntryStamp {
    pub(crate) fn of(stat: nix::sys::stat::FileStat) -> Self {
        Self {
            identity: [stat.st_dev as u64, stat.st_ino as u64],
            uid: stat.st_uid,
            mode: stat.st_mode as u32,
            links: stat.st_nlink as u64,
            size: stat.st_size,
            modified: [stat.st_mtime, stat.st_mtime_nsec],
            changed: [stat.st_ctime, stat.st_ctime_nsec],
        }
    }
    pub(crate) fn file(file: &File) -> Result<Self> {
        Ok(Self::of(nix::sys::stat::fstat(file)?))
    }
    fn kind(self) -> nix::sys::stat::SFlag {
        nix::sys::stat::SFlag::from_bits_truncate(self.mode as _) & nix::sys::stat::SFlag::S_IFMT
    }
}

#[cfg(unix)]
struct TreeObserver<'a, R> {
    check_link_targets: bool,
    root: &'a R,
    limits: PrefixInventoryLimits,
    expected: Option<&'a BTreeMap<&'a Path, ExpectedEntry<'a>>>,
    entries: BTreeMap<PathBuf, TreeInventoryEntry>,
    stamps: BTreeMap<PathBuf, EntryStamp>,
    files: usize,
    directories: usize,
    path_bytes: usize,
    total_bytes: u64,
    before_read: &'a mut dyn FnMut(&Path) -> Result<()>,
}

#[cfg(unix)]
impl<R: InventoryRoot> TreeObserver<'_, R> {
    fn charge_path(&mut self, path: &Path) -> Result<()> {
        let bytes = path.as_os_str().as_encoded_bytes().len();
        ensure!(
            bytes <= self.limits.max_path_bytes
                && path.components().count() <= self.limits.max_components,
            "prefix inventory limit: observed path exceeded its budget"
        );
        self.path_bytes = self
            .path_bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_total_path_bytes)
            .ok_or_else(|| {
                eyre::eyre!("prefix inventory limit: observed total path bytes exceeded")
            })?;
        Ok(())
    }

    fn walk(&mut self, directory: &File, relative: &Path) -> Result<()> {
        use nix::fcntl::{AtFlags, OFlag, openat, readlinkat};
        use nix::sys::stat::{Mode, SFlag, fstatat};
        use std::os::unix::ffi::OsStrExt;
        self.root.check_binding()?;
        let before = EntryStamp::file(directory)?;
        let mut listing = nix::dir::Dir::openat(
            directory,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        for item in listing.iter() {
            self.root.check_binding()?;
            let item = item?;
            let name = item.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(name);
            let path = relative.join(name);
            self.charge_path(&path)?;
            let stamp = EntryStamp::of(fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?);
            ensure!(
                stamp.uid == self.root.expected_uid(),
                "prefix inventory entry owner changed"
            );
            ensure!(
                stamp.mode & 0o7000 == 0,
                "prefix inventory mode has privileged bits"
            );
            let kind = if stamp.kind() == SFlag::S_IFDIR {
                self.directories = self
                    .directories
                    .checked_add(1)
                    .filter(|n| *n <= self.limits.max_directories)
                    .ok_or_else(|| {
                        eyre::eyre!("prefix inventory limit: observed directory count exceeded")
                    })?;
                if let Some(expected) = self.expected {
                    let Some(ExpectedEntry::Directory(expected)) = expected.get(path.as_path())
                    else {
                        eyre::bail!("prefix inventory entry has an unexpected directory");
                    };
                    ensure!(
                        stamp.mode & 0o7777 == expected.mode,
                        "prefix inventory mode differs for a directory"
                    );
                }
                let child = File::from(openat(
                    directory,
                    name,
                    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
                ensure!(
                    EntryStamp::file(&child)? == stamp,
                    "prefix inventory entry directory changed while opening"
                );
                self.walk(&child, &path)?;
                TreeInventoryKind::Directory
            } else if stamp.kind() == SFlag::S_IFREG {
                self.files = self
                    .files
                    .checked_add(1)
                    .filter(|n| *n <= self.limits.max_files)
                    .ok_or_else(|| {
                        eyre::eyre!("prefix inventory limit: observed file count exceeded")
                    })?;
                ensure!(
                    stamp.size >= 0 && stamp.size as u64 <= self.limits.max_file_bytes,
                    "prefix inventory limit: observed file bytes exceeded"
                );
                self.total_bytes = self
                    .total_bytes
                    .checked_add(stamp.size as u64)
                    .filter(|n| *n <= self.limits.max_total_bytes)
                    .ok_or_else(|| {
                        eyre::eyre!("prefix inventory limit: observed total bytes exceeded")
                    })?;
                let expected = self
                    .expected
                    .map(|expected| match expected.get(path.as_path()) {
                        Some(ExpectedEntry::File(file)) => Ok(*file),
                        _ => Err(eyre::eyre!("prefix inventory entry has an unexpected file")),
                    })
                    .transpose()?;
                let opened = File::from(openat(
                    directory,
                    name,
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
                ensure!(
                    EntryStamp::file(&opened)? == stamp,
                    "prefix inventory entry file changed while opening"
                );
                (self.before_read)(&path)?;
                let (size, sha256) =
                    observe_file(self.root, &opened, expected, self.limits.max_file_bytes)?;
                ensure!(
                    size == stamp.size as u64,
                    "prefix inventory size changed after byte admission"
                );
                TreeInventoryKind::File { size, sha256 }
            } else if stamp.kind() == SFlag::S_IFLNK {
                self.files = self
                    .files
                    .checked_add(1)
                    .filter(|n| *n <= self.limits.max_files)
                    .ok_or_else(|| {
                        eyre::eyre!("prefix inventory limit: observed link count exceeded")
                    })?;
                ensure!(
                    stamp.links == 1
                        && stamp.size >= 0
                        && stamp.size as u64 <= self.limits.max_path_bytes as u64,
                    "prefix inventory entry symlink exceeds its bound"
                );
                let target = PathBuf::from(readlinkat(directory, name)?);
                ensure!(
                    !target.as_os_str().is_empty() && !target.is_absolute(),
                    "prefix inventory entry symlink target is not relative: {}", self.root.root_path().join(&path).display()
                );
                if let Some(expected) = self.expected {
                    let Some(ExpectedEntry::Symlink(link)) = expected.get(path.as_path()) else {
                        eyre::bail!("prefix inventory entry is an unexpected symlink");
                    };
                    ensure!(
                        target == link.target,
                        "prefix inventory symlink target differs"
                    );
                }
                self.charge_path(&target)?;
                TreeInventoryKind::Symlink { target }
            } else {
                eyre::bail!("prefix inventory entry has an unsupported type: {}", self.root.root_path().join(&path).display());
            };
            ensure!(
                EntryStamp::of(fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?) == stamp,
                "prefix inventory entry changed while observed"
            );
            ensure!(
                self.entries
                    .insert(
                        path.clone(),
                        TreeInventoryEntry {
                            relative: path.clone(),
                            identity: stamp.identity,
                            uid: stamp.uid,
                            mode: stamp.mode & 0o7777,
                            kind
                        }
                    )
                    .is_none(),
                "prefix inventory entry appeared more than once"
            );
            self.stamps.insert(path, stamp);
        }
        ensure!(
            EntryStamp::file(directory)? == before,
            "prefix inventory entry directory changed during traversal"
        );
        self.root.check_binding()?;
        Ok(())
    }

    fn check_link(&self, relative: &Path) -> Result<()> {
        use std::collections::VecDeque;
        use std::path::Component;
        let mut pending = relative
            .components()
            .map(|part| part.as_os_str().to_owned())
            .collect::<VecDeque<_>>();
        let mut current = PathBuf::new();
        let mut expansions = 0usize;
        let mut expanded_bytes = 0usize;
        while let Some(part) = pending.pop_front() {
            self.root.check_binding()?;
            match Path::new(&part).components().next() {
                Some(Component::CurDir) => continue,
                Some(Component::ParentDir) => {
                    ensure!(
                        current.pop(),
                        "prefix inventory entry symlink escapes its root"
                    );
                    continue;
                }
                Some(Component::Normal(_)) => current.push(&part),
                _ => eyre::bail!("prefix inventory entry symlink path is unsafe"),
            }
            ensure!(
                current.components().count() <= self.limits.max_components
                    && current.as_os_str().as_encoded_bytes().len() <= self.limits.max_path_bytes,
                "prefix inventory limit: resolved link path exceeded"
            );
            let entry = self
                .entries
                .get(&current)
                .ok_or_else(|| eyre::eyre!("prefix inventory entry symlink is dangling"))?;
            match &entry.kind {
                TreeInventoryKind::Symlink { target } => {
                    expansions = expansions
                        .checked_add(1)
                        .filter(|n| *n <= self.limits.max_components)
                        .ok_or_else(|| {
                            eyre::eyre!("prefix inventory entry symlink cycle or expansion limit")
                        })?;
                    expanded_bytes = expanded_bytes
                        .checked_add(target.as_os_str().as_encoded_bytes().len())
                        .filter(|n| *n <= self.limits.max_total_path_bytes)
                        .ok_or_else(|| {
                            eyre::eyre!("prefix inventory limit: expanded symlink bytes exceeded")
                        })?;
                    current.pop();
                    // Component normalization discards this directory demand;
                    // actual metadata/open resolve `file/` and `file/.` with
                    // ENOTDIR even when a path-canonicalization helper does not.
                    let raw = target.as_os_str().as_encoded_bytes();
                    if raw.ends_with(b"/") || raw.rsplit(|byte| *byte == b'/').next() == Some(b".")
                    {
                        pending.push_front(std::ffi::OsString::from("."));
                    }
                    for part in target.components().rev() {
                        pending.push_front(part.as_os_str().to_owned());
                    }
                }
                TreeInventoryKind::File { .. } => ensure!(
                    pending.is_empty(),
                    "prefix inventory entry symlink traverses a file"
                ),
                TreeInventoryKind::Directory => {}
            }
        }
        Ok(())
    }

    fn recheck(&self) -> Result<()> {
        use nix::fcntl::{AtFlags, OFlag, openat, readlinkat};
        use nix::sys::stat::{Mode, fstatat};
        for (path, expected) in &self.stamps {
            self.root.check_binding()?;
            let mut parent = self.root.root_file().try_clone()?;
            for component in path.parent().unwrap_or_else(|| Path::new("")).components() {
                parent = File::from(openat(
                    &parent,
                    component.as_os_str(),
                    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
            }
            let leaf = path
                .file_name()
                .ok_or_else(|| eyre::eyre!("prefix inventory entry has no leaf"))?;
            ensure!(
                EntryStamp::of(fstatat(&parent, leaf, AtFlags::AT_SYMLINK_NOFOLLOW)?) == *expected,
                "prefix inventory entry changed after observation"
            );
            if let TreeInventoryKind::Symlink { target } = &self.entries[path].kind {
                ensure!(
                    PathBuf::from(readlinkat(&parent, leaf)?) == *target,
                    "prefix inventory entry symlink target changed"
                );
                if self.check_link_targets {
                    self.check_link(path).wrap_err_with(|| format!("invalid symlink: {}", self.root.root_path().join(path).display()))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn observe_tree<'a>(
    root: &'a impl InventoryRoot,
    limits: PrefixInventoryLimits,
    expected: Option<&'a BTreeMap<&'a Path, ExpectedEntry<'a>>>,
) -> Result<TreeInventory> {
    observe_tree_with_hook(root, limits, expected, &mut |_| Ok(()))
}

#[cfg(unix)]
fn observe_tree_with_hook<'a>(
    root: &'a impl InventoryRoot,
    limits: PrefixInventoryLimits,
    expected: Option<&'a BTreeMap<&'a Path, ExpectedEntry<'a>>>,
    before_read: &'a mut dyn FnMut(&Path) -> Result<()>,
) -> Result<TreeInventory> {
    observe_tree_with_link_policy(root, limits, expected, before_read, true)
}

/// Partial deletion can leave an original symlink's target already removed.
/// This returns descriptive bytes only; the deletion owner must compare every
/// surviving entry and link target with its original immutable inventory.
#[cfg(unix)]
pub(crate) fn observe_inventory_for_removal(
    root: &impl InventoryRoot,
    limits: PrefixInventoryLimits,
) -> Result<TreeInventory> {
    observe_tree_with_link_policy(root, limits, None, &mut |_| Ok(()), false)
}

#[cfg(unix)]
fn observe_tree_with_link_policy<'a>(
    root: &'a impl InventoryRoot,
    limits: PrefixInventoryLimits,
    expected: Option<&'a BTreeMap<&'a Path, ExpectedEntry<'a>>>,
    before_read: &'a mut dyn FnMut(&Path) -> Result<()>,
    check_link_targets: bool,
) -> Result<TreeInventory> {
    preflight(&[], &[], &[], limits)?;
    root.check_binding()
        .wrap_err("prefix inventory allocation is unavailable")?;
    ensure!(
        root.root_path().is_absolute(),
        "prefix inventory root path is not absolute"
    );
    let before = EntryStamp::file(root.root_file())?;
    ensure!(
        before.kind() == nix::sys::stat::SFlag::S_IFDIR
            && before.uid == root.expected_uid()
            && before.mode & 0o7777 == 0o700,
        "prefix inventory root has changed type, owner or mode"
    );
    let mut observer = TreeObserver {
        check_link_targets,
        root,
        limits,
        expected,
        entries: BTreeMap::new(),
        stamps: BTreeMap::new(),
        files: 0,
        directories: 0,
        path_bytes: 0,
        total_bytes: 0,
        before_read,
    };
    observer.walk(root.root_file(), Path::new(""))?;
    observer.recheck()?;
    root.check_binding()
        .wrap_err("prefix inventory allocation changed during verification")?;
    ensure!(
        EntryStamp::file(root.root_file())? == before,
        "prefix inventory root changed during verification"
    );
    let mut inventory = TreeInventory {
        root: root.root_path().into(),
        allocation: root.allocation_id().into(),
        identity: before.identity,
        uid: before.uid,
        mode: before.mode & 0o7777,
        entries: observer.entries.into_values().collect(),
        total_bytes: observer.total_bytes,
        inventory_sha256: [0; 32],
    };
    inventory.refresh_digest();
    Ok(inventory)
}

#[derive(Debug)]
pub(crate) struct PrefixInventoryReceipt {
    allocation: String,
    final_prefix: PathBuf,
    inventory_sha256: [u8; 32],
    file_count: usize,
    directory_count: usize,
    total_bytes: u64,
    tree_inventory: TreeInventory,
}

impl PrefixInventoryReceipt {
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn inventory_sha256(&self) -> &[u8; 32] {
        &self.inventory_sha256
    }
    pub(crate) fn file_count(&self) -> usize {
        self.file_count
    }
    pub(crate) fn directory_count(&self) -> usize {
        self.directory_count
    }
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    /// The very same observation checked against the caller's expected
    /// bytes, modes and owned inodes; never a fresh unauthenticated snapshot.
    pub(crate) fn into_tree_inventory(self) -> TreeInventory {
        self.tree_inventory
    }
}

pub(crate) fn verify_prefix_inventory(
    prefix: &UnpublishedPrefix,
    files: &[PrefixExpectedFile],
    directories: &[PrefixExpectedDirectory],
    limits: PrefixInventoryLimits,
) -> Result<PrefixInventoryReceipt> {
    #[cfg(unix)]
    {
        verify_unix(prefix, files, directories, &[], limits)
    }
    #[cfg(not(unix))]
    eyre::bail!("prefix inventory requires a supported Unix platform")
}

pub(crate) fn verify_layout_inventory(
    prefix: &UnpublishedPrefix,
    files: &[PrefixExpectedFile],
    directories: &[PrefixExpectedDirectory],
    links: &[PrefixExpectedSymlink],
    limits: PrefixInventoryLimits,
) -> Result<PrefixInventoryReceipt> {
    #[cfg(unix)]
    {
        verify_unix(prefix, files, directories, links, limits)
    }
    #[cfg(not(unix))]
    eyre::bail!("prefix inventory requires a supported Unix platform")
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum ExpectedEntry<'a> {
    File(&'a PrefixExpectedFile),
    Directory(&'a PrefixExpectedDirectory),
    Symlink(&'a PrefixExpectedSymlink),
}

#[cfg(unix)]
fn preflight<'a>(
    files: &'a [PrefixExpectedFile],
    directories: &'a [PrefixExpectedDirectory],
    links: &'a [PrefixExpectedSymlink],
    limits: PrefixInventoryLimits,
) -> Result<(BTreeMap<&'a Path, ExpectedEntry<'a>>, u64)> {
    ensure!(
        limits.max_files != 0
            && limits.max_directories != 0
            && limits.max_components != 0
            && limits.max_path_bytes != 0
            && limits.max_total_path_bytes != 0
            && limits.max_file_bytes != 0
            && limits.max_total_bytes != 0,
        "prefix inventory limit values must be nonzero"
    );
    ensure!(
        files
            .len()
            .checked_add(links.len())
            .is_some_and(|count| count <= limits.max_files)
            && directories.len() <= limits.max_directories,
        "prefix inventory limit: expected entry count exceeded"
    );
    let mut total_path_bytes = 0usize;
    let mut total_bytes = 0u64;
    // Bound the complete public input before building a sorted index.
    for (path, mode) in files
        .iter()
        .map(|file| (file.relative.as_path(), file.mode))
        .chain(
            directories
                .iter()
                .map(|dir| (dir.relative.as_path(), dir.mode)),
        )
        .chain(links.iter().map(|link| (link.relative.as_path(), 0o777)))
    {
        let bytes = path.as_os_str().as_encoded_bytes();
        ensure!(
            !bytes.is_empty()
                && !path.is_absolute()
                && !bytes.contains(&0)
                && !bytes
                    .split(|byte| *byte == b'/')
                    .any(|part| part.is_empty() || matches!(part, b"." | b".."))
                && path
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                && mode & !0o777 == 0,
            "prefix inventory input contains an unsafe path or mode"
        );
        ensure!(
            path.components().count() <= limits.max_components
                && bytes.len() <= limits.max_path_bytes,
            "prefix inventory limit: expected path exceeded its budget"
        );
        total_path_bytes = total_path_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= limits.max_total_path_bytes)
            .ok_or_else(|| eyre::eyre!("prefix inventory limit: total path bytes exceeded"))?;
    }
    for file in files {
        ensure!(
            file.size <= limits.max_file_bytes,
            "prefix inventory limit: file bytes exceeded"
        );
        total_bytes = total_bytes
            .checked_add(file.size)
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or_else(|| eyre::eyre!("prefix inventory limit: total file bytes exceeded"))?;
    }
    let mut expected = BTreeMap::new();
    for file in files {
        ensure!(
            expected
                .insert(file.relative.as_path(), ExpectedEntry::File(file))
                .is_none(),
            "prefix inventory input has a duplicate or conflicting entry"
        );
    }
    for directory in directories {
        ensure!(
            expected
                .insert(
                    directory.relative.as_path(),
                    ExpectedEntry::Directory(directory)
                )
                .is_none(),
            "prefix inventory input has a duplicate or conflicting entry"
        );
    }
    for link in links {
        ensure!(
            expected
                .insert(link.relative.as_path(), ExpectedEntry::Symlink(link))
                .is_none(),
            "prefix inventory input has a duplicate or conflicting entry"
        );
    }
    for path in expected.keys() {
        let mut parent = path.parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            ensure!(
                matches!(expected.get(path), Some(ExpectedEntry::Directory(_))),
                "prefix inventory input omits a parent directory or has a file ancestor"
            );
            parent = path.parent();
        }
    }
    Ok((expected, total_bytes))
}

#[cfg(unix)]
fn verify_unix(
    prefix: &UnpublishedPrefix,
    files: &[PrefixExpectedFile],
    directories: &[PrefixExpectedDirectory],
    links: &[PrefixExpectedSymlink],
    limits: PrefixInventoryLimits,
) -> Result<PrefixInventoryReceipt> {
    let (expected, total_bytes) = preflight(files, directories, links, limits)?;
    ensure!(
        expected.len() == prefix.entries.len()
            && prefix.entries.len() <= prefix.limits.max_cleanup_entries,
        "prefix inventory entry count differs from the owned allocation"
    );
    let observed = observe_tree(prefix, limits, Some(&expected))?;
    ensure!(
        observed.entries.len() == expected.len(),
        "prefix inventory entry inventory differs from its expected entries"
    );
    let root = prefix.binding.root.metadata()?;
    let mut inventory = Sha256::new();
    inventory.update(b"pakro-prefix-inventory-v1\0");
    hash_field(&mut inventory, prefix.allocation_id().as_bytes());
    hash_field(
        &mut inventory,
        prefix.final_path().as_os_str().as_encoded_bytes(),
    );
    inventory.update(root.dev().to_le_bytes());
    inventory.update(root.ino().to_le_bytes());
    inventory.update(root.uid().to_le_bytes());
    inventory.update((root.mode() & 0o7777).to_le_bytes());
    for actual in &observed.entries {
        let relative = actual.relative.as_path();
        let expected = expected
            .get(relative)
            .ok_or_else(|| eyre::eyre!("prefix inventory entry is not expected"))?;
        let tracked = prefix
            .entries
            .get(relative)
            .ok_or_else(|| eyre::eyre!("prefix inventory entry is not owned by this allocation"))?;
        ensure!(
            actual.identity
                == [
                    tracked.identity.device as u64,
                    tracked.identity.inode as u64
                ],
            "prefix inventory entry identity or type changed"
        );
        hash_field(&mut inventory, relative.as_os_str().as_encoded_bytes());
        match expected {
            ExpectedEntry::Directory(directory) => {
                ensure!(
                    tracked.kind == super::EntryKind::Directory
                        && matches!(actual.kind, TreeInventoryKind::Directory),
                    "prefix inventory entry has a conflicting type"
                );
                inventory.update(b"d");
                inventory.update(directory.mode.to_le_bytes());
                inventory.update(actual.identity[0].to_le_bytes());
                inventory.update(actual.identity[1].to_le_bytes());
            }
            ExpectedEntry::File(file) => {
                ensure!(
                    tracked.kind == super::EntryKind::File
                        && matches!(actual.kind, TreeInventoryKind::File { .. }),
                    "prefix inventory entry has a conflicting type"
                );
                inventory.update(b"f");
                inventory.update(file.mode.to_le_bytes());
                inventory.update(file.size.to_le_bytes());
                inventory.update(file.sha256);
                inventory.update(tracked.identity.device.to_le_bytes());
                inventory.update(tracked.identity.inode.to_le_bytes());
            }
            ExpectedEntry::Symlink(link) => {
                ensure!(
                    tracked.kind == super::EntryKind::Symlink
                        && matches!(&actual.kind, TreeInventoryKind::Symlink { target } if target == &link.target),
                    "prefix inventory entry has a conflicting type or target"
                );
                inventory.update(b"l");
                hash_field(&mut inventory, link.target.as_os_str().as_encoded_bytes());
                inventory.update(tracked.identity.device.to_le_bytes());
                inventory.update(tracked.identity.inode.to_le_bytes());
            }
        }
    }
    prefix
        .check_write_ready()
        .wrap_err("prefix inventory allocation changed during verification")?;
    let after = prefix.binding.root.metadata()?;
    ensure!(
        after.uid() == root.uid() && after.mode() & 0o7777 == 0o700,
        "prefix inventory root changed during verification"
    );
    Ok(PrefixInventoryReceipt {
        allocation: prefix.allocation_id().into(),
        final_prefix: prefix.final_path().into(),
        inventory_sha256: inventory.finalize().into(),
        file_count: files.len() + links.len(),
        directory_count: directories.len(),
        total_bytes,
        tree_inventory: observed,
    })
}

#[cfg(unix)]
fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(unix)]
fn observe_file(
    root: &impl InventoryRoot,
    mut opened: &File,
    expected: Option<&PrefixExpectedFile>,
    max_bytes: u64,
) -> Result<(u64, [u8; 32])> {
    let before = opened.metadata()?;
    ensure!(
        before.is_file() && before.uid() == root.expected_uid(),
        "prefix inventory entry file identity, owner or type changed"
    );
    ensure!(
        before.nlink() == 1,
        "prefix inventory links are not independent"
    );
    ensure!(
        before.len() <= max_bytes,
        "prefix inventory limit: observed file bytes exceeded"
    );
    if let Some(expected) = expected {
        ensure!(
            before.mode() & 0o7777 == expected.mode,
            "prefix inventory mode differs for a file"
        );
        ensure!(
            before.len() == expected.size,
            "prefix inventory size differs"
        );
    }
    let exact_size = before.len();
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        root.check_binding()?;
        let request = (exact_size - size)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = opened
            .read(&mut buffer[..request])
            .wrap_err("prefix inventory entry file read failed")?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .filter(|size| *size <= exact_size)
            .ok_or_else(|| eyre::eyre!("prefix inventory size exceeded its exact bound"))?;
        digest.update(&buffer[..count]);
    }
    ensure!(size == exact_size, "prefix inventory size differs at EOF");
    let actual: [u8; 32] = digest.finalize().into();
    if let Some(expected) = expected {
        ensure!(actual == expected.sha256, "prefix inventory digest differs");
    }
    ensure!(
        same_file_facts(&before, &opened.metadata()?),
        "prefix inventory entry changed during its read"
    );
    root.check_binding()?;
    Ok((size, actual))
}

#[cfg(unix)]
fn same_file_facts(before: &Metadata, after: &Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.uid() == after.uid()
        && before.mode() == after.mode()
        && before.nlink() == after.nlink()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
#[path = "verify/tree_tests.rs"]
mod tree_tests;
