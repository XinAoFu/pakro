use super::{
    journal::{Cursor, Session, durable, sync_namespace, sync_parent},
    object::*,
    *,
};
use crate::packages::python_environment::prefix::verify::{
    EntryStamp, InventoryRoot, PrefixInventoryLimits, TreeInventoryEntry, TreeInventoryKind,
    observe_inventory_for_removal, observe_tree_inventory,
};
use eyre::{ContextCompat, ensure};
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
fn allocation_children(owner: &OwnedObject, cursor: &Cursor) -> Result<()> {
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
            name == b"payload" && !cursor.payload_removed,
            "language allocation contains an unknown member"
        );
    }
    Ok(())
}
pub(super) fn verify_remaining(
    owner: &OwnedObject,
    context: &OperationContext,
    cursor: &Cursor,
) -> Result<()> {
    cursor.validate(
        owner.record.inventory.entries().len(),
        owner.record.payload.is_some(),
    )?;
    let order = ordered(&owner.record);
    ensure!(
        cursor.next <= order.len() && (!cursor.pending || cursor.next < order.len()),
        "reclaim deletion cursor is invalid"
    );
    ensure!(
        !cursor.allocation_removed || cursor.allocation_pending,
        "reclaim allocation progress is invalid"
    );
    ensure!(
        !cursor.payload_removed || cursor.payload_pending,
        "reclaim payload progress is invalid"
    );
    allocation_children(owner, cursor)?;
    let Some(_) = &owner.root else {
        ensure!(
            cursor.next == order.len() && (cursor.payload_pending || cursor.allocation_pending),
            "reclaim tree disappeared before its recorded deletion"
        );
        return Ok(());
    };
    let root = Root {
        owner,
        context,
        path: owner.record.root_path(&owner.store),
    };
    let actual = if cursor.next == 0 && !cursor.pending {
        observe_tree_inventory(&root, PrefixInventoryLimits::default())?
    } else {
        observe_inventory_for_removal(&root, PrefixInventoryLimits::default())?
    };
    ensure!(
        actual.same_root_binding(&owner.record.inventory),
        "reclaim root metadata changed"
    );
    let mut expected = order
        .iter()
        .skip(cursor.next)
        .map(|e| (e.relative.clone(), *e))
        .collect::<BTreeMap<_, _>>();
    if cursor.pending
        && !actual
            .entries()
            .iter()
            .any(|e| e.relative == order[cursor.next].relative)
    {
        expected.remove(&order[cursor.next].relative);
    }
    ensure!(
        actual.entries().len() == expected.len()
            && actual.entries().iter().all(|entry| expected
                .get(&entry.relative)
                .is_some_and(|old| **old == *entry)),
        "reclaim remaining inventory changed or has unknown members"
    );
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
    ) -> Result<&File> {
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
            if self.directories.len() > depth {
                let stat = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
                ensure!(
                    SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
                        && [stat.st_dev as u64, stat.st_ino as u64] == entry.identity,
                    "reclaim parent directory was replaced"
                );
            } else {
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
        Ok(if depth == 0 {
            &self.root
        } else {
            &self.directories[depth - 1].1
        })
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
pub(super) fn remove(
    session: &Session,
    index: usize,
    owner: &OwnedObject,
    context: &OperationContext,
    cursor: &mut Cursor,
    pin: &mut pakro_store::control::ControlSnapshot,
) -> Result<()> {
    owner.verify(context, cursor)?;
    let order = ordered(&owner.record);
    // Derive this lookup once from the verified immutable record while its
    // original EX is held. Cached directory descriptors retain their binding
    // checks against the original root on every use.
    let entries = owner
        .record
        .inventory
        .entries()
        .iter()
        .map(|entry| (entry.relative.as_path(), entry))
        .collect::<BTreeMap<_, _>>();
    #[cfg(test)]
    PARENT_INDEX_BUILDS.with(|count| count.set(count.get() + 1));
    if !cursor.revoked {
        cursor.revoke_pending = true;
        session.save_cursor(index, cursor, pin)?;
        durable("revoke-intent");
        let mut control = owner.store.writer()?.control(Default::default())?;
        if let Some(ready) = control.read(
            pakro_store::control::ControlNamespace::Catalog,
            &manifest_name(owner.record.object),
        )? {
            ensure!(
                ready.bytes() == owner.record.manifest
                    && pair_snapshot(&ready) == owner.record.manifest_identity,
                "reclaim original manifest changed before revocation"
            );
            control.remove_with_before_sync(&ready, || {
                durable("ready-unlink-unsynced");
                Ok(())
            })?;
        }
        sync_namespace(
            context,
            &control,
            pakro_store::control::ControlNamespace::Catalog,
            "ready-parent",
            Some(owner.record.object),
        )?;
        drop(control);
        durable("ready-removed");
        cursor.revoked = true;
        session.save_cursor(index, cursor, pin)?;
        durable("revoked");
    }
    if !cursor.captured {
        cursor.capture_pending = true;
        session.save_cursor(index, cursor, pin)?;
        durable("capture-intent");
        if stat_identity(&owner.store.root, &owner.record.captured)?.is_none() {
            pakro_store::control::ensure_entry_identity(
                &owner.store.root,
                &owner.record.allocation,
                identity(owner.record.allocation_identity)?,
            )?;
            pakro_store::control::rename_noclobber(
                &owner.store.root,
                std::ffi::OsStr::new(&owner.record.allocation),
                &owner.store.root,
                std::ffi::OsStr::new(&owner.record.captured),
            )?;
            durable("capture-rename-unsynced");
        }
        sync_parent(
            context,
            &owner.store.root,
            "capture-parent",
            Some(owner.record.object),
        )?;
        durable("capture-renamed");
        pakro_store::control::ensure_entry_identity(
            &owner.store.root,
            &owner.record.captured,
            identity(owner.record.allocation_identity)?,
        )?;
        cursor.captured = true;
        session.save_cursor(index, cursor, pin)?;
        durable("captured");
    }
    let mut parents = (cursor.next < order.len())
        .then(|| ParentCache::new(owner))
        .transpose()?;
    while cursor.next < order.len() {
        owner.check_held(context)?;
        let entry = order[cursor.next];
        let directory = parents
            .as_mut()
            .expect("members need a parent")
            .parent(&entries, &entry.relative)?;
        let existed = verify_entry(directory, entry, context)?;
        ensure!(
            existed || cursor.pending,
            "unstarted reclaim member disappeared"
        );
        if !cursor.pending {
            cursor.pending = true;
            session.save_cursor(index, cursor, pin)?;
        }
        durable("entry-intent");
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
            durable("entry-unlink-unsynced");
        }
        sync_parent(
            context,
            directory,
            "entry-parent",
            Some(owner.record.object),
        )?;
        durable("entry-unlinked");
        cursor.next += 1;
        cursor.pending = false;
        session.save_cursor(index, cursor, pin)?;
        durable("entry-removed");
    }
    if owner.record.payload.is_some() && !cursor.payload_removed {
        cursor.payload_pending = true;
        session.save_cursor(index, cursor, pin)?;
        durable("payload-intent");
        let allocation = owner
            .allocation
            .as_ref()
            .context("pending payload deletion has no original parent")?;
        if stat_identity(allocation, "payload")?.is_some() {
            pakro_store::control::ensure_entry_identity(
                allocation,
                "payload",
                identity(owner.record.payload.expect("language payload"))?,
            )?;
            unlinkat(allocation, "payload", UnlinkatFlags::RemoveDir)?;
            durable("payload-rmdir-unsynced");
        }
        sync_parent(
            context,
            allocation,
            "payload-parent",
            Some(owner.record.object),
        )?;
        durable("payload-unlinked");
        cursor.payload_removed = true;
        session.save_cursor(index, cursor, pin)?;
    }
    if !cursor.allocation_removed {
        cursor.allocation_pending = true;
        session.save_cursor(index, cursor, pin)?;
        durable("allocation-intent");
        if stat_identity(&owner.store.root, &owner.record.captured)?.is_some() {
            pakro_store::control::ensure_entry_identity(
                &owner.store.root,
                &owner.record.captured,
                identity(owner.record.allocation_identity)?,
            )?;
            unlinkat(
                &owner.store.root,
                owner.record.captured.as_str(),
                UnlinkatFlags::RemoveDir,
            )?;
            durable("allocation-rmdir-unsynced");
        }
        sync_parent(
            context,
            &owner.store.root,
            "allocation-parent",
            Some(owner.record.object),
        )?;
        durable("allocation-unlinked");
        cursor.allocation_removed = true;
        session.save_cursor(index, cursor, pin)?;
        durable("allocation-removed");
    }
    Ok(())
}
