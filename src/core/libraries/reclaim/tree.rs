use super::{object::*, *};
use crate::packages::python_environment::prefix::verify::{
    EntryStamp, InventoryRoot, PrefixInventoryLimits, TreeInventoryEntry, TreeInventoryKind,
    observe_inventory_for_removal, observe_tree_inventory,
};
use eyre::{ContextCompat, WrapErr, ensure};
use nix::{
    fcntl::{AtFlags, OFlag, openat, readlinkat},
    sys::stat::{Mode, SFlag, fstatat},
    unistd::{UnlinkatFlags, unlinkat},
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
#[cfg(test)]
thread_local! {
    static PARENT_INDEX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) fn parent_index_builds() -> usize {
    PARENT_INDEX_BUILDS.with(|count| count.get())
}
fn ordered(record: &Record) -> Vec<&TreeInventoryEntry> {
    let mut order = record.inventory.entries().iter().collect::<Vec<_>>();
    order.sort_by(|a, b| {
        b.relative
            .components()
            .count()
            .cmp(&a.relative.components().count())
            .then_with(|| a.relative.cmp(&b.relative))
    });
    order
}
struct Root<'a> {
    owner: &'a OwnedObject,
    context: &'a OperationContext,
    path: PathBuf,
}
impl InventoryRoot for Root<'_> {
    fn root_file(&self) -> &File {
        self.owner.root.as_ref().expect("checked root")
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        &self.owner.record.allocation
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.owner.check_held(self.context)
    }
}
fn allocation_children(owner: &OwnedObject) -> Result<()> {
    if owner.record.payload.is_none() {
        return Ok(());
    }
    let Some(root) = &owner.allocation else {
        return Ok(());
    };
    let mut dir = nix::dir::Dir::openat(
        root,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        ensure!(
            name == b"payload",
            "language allocation contains an unknown member: {}",
            owner
                .store
                .path
                .join(&owner.record.allocation)
                .join(std::ffi::OsStr::from_bytes(name))
                .display()
        );
    }
    Ok(())
}
pub(super) fn verify_remaining(owner: &OwnedObject, context: &OperationContext) -> Result<()> {
    allocation_children(owner)?;
    let Some(_) = &owner.root else { return Ok(()) };
    let root = Root {
        owner,
        context,
        path: owner.record.root_path(&owner.store),
    };
    let actual = if !owner.record.pending {
        observe_tree_inventory(&root, PrefixInventoryLimits::default())?
    } else {
        observe_inventory_for_removal(&root, PrefixInventoryLimits::default())?
    };
    ensure!(
        actual.same_root_binding(&owner.record.inventory),
        "reclaim root metadata changed"
    );
    let mut expected = owner
        .record
        .inventory
        .entries()
        .iter()
        .map(|entry| (entry.relative.as_path(), entry))
        .collect::<BTreeMap<_, _>>();
    for entry in actual.entries() {
        ensure!(
            expected.remove(entry.relative.as_path()) == Some(entry),
            "reclaim member changed or is not recorded: {}",
            root.path.join(&entry.relative).display()
        );
    }
    if !owner.record.pending {
        ensure!(
            expected.is_empty(),
            "reclaim recorded member disappeared: {}",
            root.path
                .join(expected.keys().next().copied().unwrap_or(Path::new("")))
                .display()
        );
    }
    Ok(())
}
pub(super) struct ParentCache {
    root: File,
    directories: Vec<(std::ffi::OsString, File)>,
    #[cfg(test)]
    pub(super) opens: usize,
}
impl ParentCache {
    pub(super) fn new(owner: &OwnedObject) -> Result<Self> {
        Ok(Self {
            root: owner
                .root
                .as_ref()
                .context("reclaim payload is absent")?
                .try_clone()?,
            directories: Vec::new(),
            #[cfg(test)]
            opens: 0,
        })
    }

    pub(super) fn parent(
        &mut self,
        entries: &BTreeMap<&Path, &TreeInventoryEntry>,
        relative: &Path,
    ) -> Result<Option<&File>> {
        let mut partial = PathBuf::new();
        let mut depth = 0;
        for component in relative
            .parent()
            .context("reclaim member has no parent")?
            .components()
        {
            let std::path::Component::Normal(name) = component else {
                eyre::bail!("unsafe reclaim parent")
            };
            partial.push(name);
            let entry = entries
                .get(partial.as_path())
                .context("untracked reclaim parent")?;
            ensure!(
                matches!(entry.kind, TreeInventoryKind::Directory),
                "reclaim parent is not a directory"
            );
            if self
                .directories
                .get(depth)
                .is_some_and(|(cached, _)| cached != name)
            {
                self.directories.truncate(depth);
            }
            let parent = if depth == 0 {
                &self.root
            } else {
                &self.directories[depth - 1].1
            };
            let stat = match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(nix::errno::Errno::ENOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            ensure!(
                SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
                    && [stat.st_dev as u64, stat.st_ino as u64] == entry.identity,
                "reclaim parent directory was replaced: {}",
                partial.display()
            );
            if self.directories.len() <= depth {
                let next = pakro_store::control::open_directory(parent, name)?;
                ensure!(
                    pair(&next)? == entry.identity,
                    "reclaim parent directory was replaced"
                );
                self.directories.push((name.to_os_string(), next));
                #[cfg(test)]
                {
                    self.opens += 1;
                }
            }
            depth += 1;
        }
        self.directories.truncate(depth);
        Ok(Some(if depth == 0 {
            &self.root
        } else {
            &self.directories[depth - 1].1
        }))
    }
}

fn verify_entry(
    directory: &File,
    entry: &TreeInventoryEntry,
    context: &OperationContext,
) -> Result<bool> {
    context.checkpoint()?;
    let name = entry
        .relative
        .file_name()
        .context("reclaim member has no name")?;
    let stat = match fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(nix::errno::Errno::ENOENT) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let before = EntryStamp::of(stat);
    ensure!(
        [stat.st_dev as u64, stat.st_ino as u64] == entry.identity
            && stat.st_uid == entry.uid
            && (stat.st_mode as u32 & 0o7777) == entry.mode,
        "reclaim entry identity, owner or mode changed"
    );
    let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
    match &entry.kind {
        TreeInventoryKind::Directory => {
            ensure!(kind == SFlag::S_IFDIR, "reclaim directory type changed")
        }
        TreeInventoryKind::Symlink { target } => {
            ensure!(kind == SFlag::S_IFLNK, "reclaim symlink type changed");
            ensure!(
                readlinkat(directory, name)?.as_bytes() == target.as_os_str().as_bytes(),
                "reclaim symlink target changed"
            );
        }
        TreeInventoryKind::File { size, sha256 } => {
            ensure!(
                kind == SFlag::S_IFREG && stat.st_nlink == 1 && stat.st_size as u64 == *size,
                "reclaim file type, links or size changed"
            );
            let mut file = File::from(openat(
                directory,
                name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                Mode::empty(),
            )?);
            ensure!(
                EntryStamp::file(&file)? == before,
                "reclaim file changed during open"
            );
            let mut hash = Sha256::new();
            let mut total = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                context.checkpoint()?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                total = total
                    .checked_add(count as u64)
                    .filter(|n| *n <= *size)
                    .context("reclaim file grew")?;
                hash.update(&buffer[..count]);
            }
            ensure!(
                total == *size
                    && <[u8; 32]>::from(hash.finalize()) == *sha256
                    && EntryStamp::file(&file)? == before,
                "reclaim file content or read stamp changed"
            );
        }
    }
    ensure!(
        EntryStamp::of(fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?) == before,
        "reclaim entry namespace or read stamp changed"
    );
    context.checkpoint()?;
    Ok(true)
}
pub(super) fn remove(owner: &OwnedObject, context: &OperationContext) -> Result<()> {
    let order = ordered(&owner.record);
    let entries = owner
        .record
        .inventory
        .entries()
        .iter()
        .map(|entry| (entry.relative.as_path(), entry))
        .collect::<BTreeMap<_, _>>();
    #[cfg(test)]
    PARENT_INDEX_BUILDS.with(|count| count.set(count.get() + 1));
    let mut verification = std::time::Duration::ZERO;
    if owner.root.is_some() {
        let mut parents = ParentCache::new(owner)?;
        for entry in order {
            owner.check_held(context)?;
            let path = owner.record.root_path(&owner.store).join(&entry.relative);
            let Some(directory) = parents
                .parent(&entries, &entry.relative)
                .wrap_err_with(|| format!("reclaim parent changed: {}", path.display()))?
            else {
                continue;
            };
            let started = std::time::Instant::now();
            let existed = verify_entry(directory, entry, context)
                .wrap_err_with(|| format!("reclaim member changed: {}", path.display()))?;
            verification += started.elapsed();
            if existed {
                unlinkat(
                    directory,
                    entry.relative.file_name().expect("validated name"),
                    if matches!(entry.kind, TreeInventoryKind::Directory) {
                        UnlinkatFlags::RemoveDir
                    } else {
                        UnlinkatFlags::NoRemoveDir
                    },
                )?;
            }
        }
    }
    debug!(
        "reclaim entry verification {}: {:.6}s",
        owner.record.object.label(),
        verification.as_secs_f64()
    );
    if let Some(expected) = owner.record.payload.filter(|_| owner.root.is_some()) {
        pakro_store::control::ensure_entry_identity(
            owner.allocation.as_ref().expect("payload parent"),
            "payload",
            identity(expected)?,
        )
        .wrap_err_with(|| {
            format!(
                "reclaim payload was replaced: {}",
                owner.record.root_path(&owner.store).display()
            )
        })?;
        unlinkat(
            owner.allocation.as_ref().expect("payload parent"),
            "payload",
            UnlinkatFlags::RemoveDir,
        )?;
    }
    if owner.allocation.is_some() {
        pakro_store::control::ensure_entry_identity(
            &owner.store.root,
            &owner.record.allocation,
            identity(owner.record.allocation_identity)?,
        )?;
        unlinkat(
            &owner.store.root,
            owner.record.allocation.as_str(),
            UnlinkatFlags::RemoveDir,
        )?;
    }
    owner.store.root.sync_all()?;
    Ok(())
}
