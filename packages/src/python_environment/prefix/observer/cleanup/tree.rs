//! Verified physical inventory shared by ordinary and fresh cleanup.

use super::*;

pub(in super::super) struct Tree<'facts> {
    pub(super) entries: BTreeMap<PathBuf, TrackedEntry>,
    pub(super) facts: BTreeMap<&'facts Path, &'facts OwnershipEntryFacts>,
}

impl<'facts> Tree<'facts> {
    pub(in super::super) fn load(
        root: Option<&File>,
        state: &'facts OwnershipRecoveryState,
        limits: PrefixLimits,
        final_prefix_bytes: usize,
        expected_uid: u32,
    ) -> Result<Self> {
        ensure!(
            state.entries().len() <= limits.max_entries
                && state.entries().len() <= limits.max_cleanup_entries,
            "cleanup inventory entry limit exceeded"
        );
        let facts: BTreeMap<_, _> = state
            .entries()
            .iter()
            .map(|fact| {
                (
                    Path::new(std::ffi::OsStr::from_bytes(fact.relative())),
                    fact,
                )
            })
            .collect();
        let mut tree = Self {
            entries: BTreeMap::new(),
            facts,
        };
        if let Some(root) = root {
            let mut observed = 0usize;
            tree.scan(
                root,
                Path::new(""),
                limits,
                final_prefix_bytes,
                expected_uid,
                &mut observed,
            )?;
            let pending = state
                .pending_remove()
                .map(|remove| Path::new(std::ffi::OsStr::from_bytes(remove.relative())));
            for (relative, fact) in &tree.facts {
                if !fact.removed() && !tree.entries.contains_key(*relative) {
                    ensure!(
                        pending == Some(*relative),
                        "owned cleanup entry is missing without durable remove intent"
                    );
                }
            }
        } else {
            ensure!(
                state.entries().iter().all(|entry| entry.removed())
                    && state.pending_create().is_none()
                    && state.pending_remove().is_none(),
                "missing root lacks durable complete entry removal"
            );
        }
        Ok(tree)
    }

    fn scan(
        &mut self,
        directory: &File,
        relative: &Path,
        limits: PrefixLimits,
        final_prefix_bytes: usize,
        expected_uid: u32,
        observed: &mut usize,
    ) -> Result<()> {
        let mut listing = nix::dir::Dir::openat(
            directory,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        for item in listing.iter() {
            let item = item?;
            let name = item.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            *observed = observed
                .checked_add(1)
                .filter(|count| {
                    *count <= limits.max_cleanup_entries && *count <= limits.max_entries
                })
                .ok_or_else(|| eyre::eyre!("cleanup inventory entry limit exceeded"))?;
            ensure!(
                relative.components().count() < limits.max_components,
                "cleanup inventory path component limit exceeded"
            );
            let bytes = relative
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add(1)
                .and_then(|size| size.checked_add(name.len()))
                .and_then(|size| size.checked_add(final_prefix_bytes));
            ensure!(
                bytes.is_some_and(|size| size <= limits.max_path_bytes),
                "cleanup inventory path byte limit exceeded"
            );
            let name = std::ffi::OsStr::from_bytes(name);
            let path = relative.join(name);
            let fact = *self
                .facts
                .get(path.as_path())
                .ok_or_else(|| eyre::eyre!("untracked environment entry cannot be deleted"))?;
            ensure!(
                !fact.removed(),
                "already removed ownership path was recreated"
            );
            let actual = fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
            let tracked = TrackedEntry {
                identity: FileIdentity {
                    device: actual.st_dev,
                    inode: actual.st_ino,
                },
                kind: match fact.kind() {
                    OwnershipEntryKind::Directory => EntryKind::Directory,
                    OwnershipEntryKind::File => EntryKind::File,
                },
            };
            verify_entry(directory, name, tracked, fact, expected_uid)?;
            ensure!(
                self.entries.insert(path.clone(), tracked).is_none(),
                "cleanup inventory path occurs twice"
            );
            if tracked.kind == EntryKind::Directory {
                let opened = open_directory(directory, name)?;
                ensure!(
                    FileIdentity::of(&opened)? == tracked.identity,
                    "cleanup inventory directory identity changed"
                );
                self.scan(
                    &opened,
                    &path,
                    limits,
                    final_prefix_bytes,
                    expected_uid,
                    observed,
                )?;
                ensure_tracked_identity(directory, name, tracked)?;
            }
        }
        Ok(())
    }

    pub(super) fn open_parent(&self, root: &File, relative: &Path) -> Result<File> {
        let mut directory = root.try_clone()?;
        let mut partial = PathBuf::new();
        let parent = relative
            .parent()
            .ok_or_else(|| eyre::eyre!("missing cleanup entry parent"))?;
        for part in parent.components() {
            let std::path::Component::Normal(name) = part else {
                bail!("unsafe cleanup path")
            };
            partial.push(name);
            let tracked = self
                .entries
                .get(&partial)
                .ok_or_else(|| eyre::eyre!("cleanup parent is not an owned remaining directory"))?;
            ensure!(
                tracked.kind == EntryKind::Directory,
                "cleanup parent is not a directory"
            );
            let opened = open_directory(&directory, name)?;
            ensure!(
                FileIdentity::of(&opened)? == tracked.identity,
                "cleanup parent identity changed"
            );
            directory = opened;
        }
        Ok(directory)
    }

    pub(super) fn removal_order(&self) -> Vec<PathBuf> {
        let mut removal: Vec<_> = self.entries.keys().cloned().collect();
        removal.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        removal
    }
}
