use super::*;

#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_REPLACE_SECOND_CAS: RefCell<Option<Box<dyn FnOnce() -> Result<()>>>> =
        const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct StoreWriter {
    anchor: Anchor,
    deadline: Instant,
    initialized_here: bool,
}
impl StoreWriter {
    /// Nonblocking maintenance gate over the anchored immutable marker.
    /// It writes no marker bytes and grants no reference/model authority.
    /// The catalog uses a different stable inode.
    pub fn try_maintenance_exclusive(&self) -> Result<Option<ObjectLease>> {
        self.check_binding()?;
        let expected = FileIdentity::of(&self.anchor.marker)?;
        let lock = StableLock::open_existing(
            &self.anchor.control,
            MARKER_NAME,
            self.anchor.uid,
            expected,
        )?;
        // try_acquire independently reopens the original inode for every
        // request, so separate requests never share one flock description.
        let held = lock.try_acquire(LeaseMode::Exclusive, self.deadline)?;
        self.check_binding()?;
        if let Some(held) = &held {
            held.check_binding()?;
        }
        Ok(held)
    }
    /// Open an already initialized controlled store without creating or
    /// repairing any directory, marker, namespace or lock.
    pub fn open_existing(
        root: &Path,
        uid: u32,
        limits: RootLimits,
        deadline: Instant,
    ) -> Result<Self> {
        let path = authorized_root(root, uid, limits, deadline)?;
        let directory = open_trusted_store_with_deadline(&path, uid, false, deadline)?;
        let anchor = existing_anchor(path, directory, uid, deadline)?;
        Ok(Self {
            anchor,
            deadline,
            initialized_here: false,
        })
    }
    /// Initialize only an empty authorized root. Existing state is never repaired
    /// by silently recreating a missing protection object.
    pub fn open_writer(
        root: &Path,
        uid: u32,
        limits: RootLimits,
        deadline: Instant,
    ) -> Result<Self> {
        let path = authorized_root(root, uid, limits, deadline)?;
        let directory = open_trusted_store_with_deadline(&path, uid, true, deadline)?;
        let mut initialized_here = false;
        let anchor = match open_existing_readonly_regular(&directory, LOCK_NAME, uid) {
            Ok(_) => existing_anchor(path, directory, uid, deadline)?,
            Err(error) if is_missing(&error) && directory_empty(&directory)? => {
                initialize_anchor_with_hook(path, directory, uid, deadline, || {
                    initialized_here = true;
                    Ok(())
                })?
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            anchor,
            deadline,
            initialized_here,
        })
    }
    /// Ephemeral creation fact from winning the original stable inode
    /// `create_new`. Reopening or losing initialization never grants it.
    /// Domain writers may use it to publish their first real catalog; it
    /// is neither a persistent authority nor permission to repair a store.
    pub fn initialized_here(&self) -> bool {
        self.initialized_here
    }
    pub fn control(&self, limits: ControlLimits) -> Result<WriterControl<'_>> {
        Ok(WriterControl {
            read: ReadControl::new(&self.anchor, limits, self.deadline, LeaseMode::Exclusive)?,
        })
    }
    pub fn root_path(&self) -> &Path {
        &self.anchor.path
    }
    pub fn check_binding(&self) -> Result<()> {
        check_deadline(self.deadline)?;
        self.anchor.check_binding(self.deadline)?;
        check_deadline(self.deadline)
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    #[doc(hidden)]
    pub fn root_handle(&self) -> &File {
        &self.anchor.root
    }
    #[doc(hidden)]
    pub fn control_handle(&self) -> &File {
        &self.anchor.control
    }
    #[doc(hidden)]
    pub fn leases_handle(&self) -> &File {
        &self.anchor.leases
    }
    #[doc(hidden)]
    pub fn build_handle(&self) -> &File {
        &self.anchor.build
    }
    #[doc(hidden)]
    pub fn namespace_handle(&self, namespace: ControlNamespace) -> &File {
        &self.anchor.namespaces[namespace.index()]
    }
    #[doc(hidden)]
    pub fn catalog_lock(&self) -> &StableLock {
        &self.anchor.catalog
    }
    #[doc(hidden)]
    pub fn expected_uid(&self) -> u32 {
        self.anchor.uid
    }
}

/// A write scope obtains the catalog EX only at its first metadata operation.
/// Project and object leases must therefore be acquired before that operation.
pub struct WriterControl<'a> {
    read: ReadControl<'a>,
}
impl WriterControl<'_> {
    pub fn read(&self, namespace: ControlNamespace, name: &str) -> Result<Option<ControlSnapshot>> {
        self.read.read(namespace, name)
    }
    pub fn read_bounded(
        &self,
        namespace: ControlNamespace,
        name: &str,
        max_bytes: u64,
    ) -> Result<Option<ControlSnapshot>> {
        self.read.read_bounded(namespace, name, max_bytes)
    }
    pub fn list_names(
        &self,
        namespace: ControlNamespace,
        max_entries: usize,
        max_total_name_bytes: usize,
    ) -> Result<Vec<String>> {
        self.read
            .list_names(namespace, max_entries, max_total_name_bytes)
    }
    pub fn store_identity(&self) -> Result<(u64, u64)> {
        self.read.store_identity()
    }
    pub fn check_binding(&self) -> Result<()> {
        self.read.check_binding()
    }
    pub fn ensure_catalog(&self) -> Result<()> {
        self.read.ensure_catalog()
    }
    pub fn try_object(
        &self,
        key: &str,
        lock: &StableLock,
        mode: LeaseMode,
    ) -> Result<Option<ObjectLease>> {
        self.read.try_object(key, lock, mode)
    }
    pub fn acquire_project(&self, lock: &StableLock) -> Result<ObjectLease> {
        self.read.acquire_project(lock)
    }
    #[doc(hidden)]
    pub fn root_handle(&self) -> &File {
        &self.read.anchor.root
    }
    #[doc(hidden)]
    pub fn control_handle(&self) -> &File {
        &self.read.anchor.control
    }
    #[doc(hidden)]
    pub fn leases_handle(&self) -> &File {
        &self.read.anchor.leases
    }
    #[doc(hidden)]
    pub fn build_handle(&self) -> &File {
        &self.read.anchor.build
    }
    #[doc(hidden)]
    pub fn namespace_handle(&self, namespace: ControlNamespace) -> &File {
        &self.read.anchor.namespaces[namespace.index()]
    }
    #[doc(hidden)]
    pub fn validate_file(&self, file: &File) -> Result<()> {
        self.read.validate_file(file)
    }
    #[doc(hidden)]
    pub fn validate_name(&self, name: &str) -> Result<()> {
        self.read.validate_name(name)
    }
    fn validate_snapshot(
        &self,
        namespace: ControlNamespace,
        name: &str,
        snapshot: &ControlSnapshot,
    ) -> Result<()> {
        self.read.validate_snapshot(namespace, name, snapshot)
    }
    pub fn replace(
        &mut self,
        namespace: ControlNamespace,
        name: &str,
        expected: Option<&ControlSnapshot>,
        bytes: &[u8],
    ) -> Result<ControlSnapshot> {
        self.validate_name(name)?;
        ensure!(
            bytes.len() as u64 <= self.read.limits.max_file_bytes,
            "environment control metadata byte limit exceeded"
        );
        self.ensure_catalog()?;
        if let Some(snapshot) = expected {
            self.read.validate_snapshot_for_replace(namespace, name, snapshot)?;
        }
        let parent = &self.read.anchor.namespaces[namespace.index()];
        let temporary_name = format!(".tmp-{}", uuid::Uuid::now_v7().simple());
        let mut file = File::from(openat(
            parent,
            temporary_name.as_str(),
            OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let identity = FileIdentity::of(&file)?;
        let result = (|| {
            fchmod(&file, Mode::from_bits_truncate(0o600))?;
            self.validate_file(&file)?;
            file.write_all(bytes)?;
            file.flush()?;
            file.sync_all()?;
            ensure!(
                file.metadata()?.len() == bytes.len() as u64,
                "environment control metadata write size mismatch"
            );
            ensure_entry_identity(parent, temporary_name.as_str(), identity)?;
            #[cfg(test)]
            if let Some(hook) = BEFORE_REPLACE_SECOND_CAS.with(|value| value.borrow_mut().take()) {
                hook()?;
            }
            self.ensure_catalog()?;
            if expected.is_none() {
                rename_noclobber(
                    parent,
                    std::ffi::OsStr::new(&temporary_name),
                    parent,
                    std::ffi::OsStr::new(name),
                )?;
            } else {
                self.read.validate_snapshot_for_replace(namespace, name, expected.expect("checked Some branch"))?;
                nix::fcntl::renameat(parent, temporary_name.as_str(), parent, name)?;
            }
            parent.sync_all()?;
            self.read(namespace, name)?
                .ok_or_else(|| eyre::eyre!("published control metadata is missing"))
        })();
        if result.is_err()
            && ensure_entry_identity(parent, temporary_name.as_str(), identity).is_ok()
        {
            let _ = nix::unistd::unlinkat(
                parent,
                temporary_name.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            );
        }
        result
    }
    /// Publish the already pinned journal inode as a catalog replacement.
    /// Callers can durably record its identity before the catalog CAS.
    pub fn replace_from_journal(
        &mut self,
        name: &str,
        expected: &ControlSnapshot,
        prepared: &ControlSnapshot,
    ) -> Result<ControlSnapshot> {
        self.validate_name(name)?;
        ensure!(
            expected.namespace == ControlNamespace::Catalog
                && expected.name == name
                && prepared.namespace == ControlNamespace::Journals,
            "prepared catalog replacement requires catalog and journal snapshots"
        );
        self.ensure_catalog()?;
        self.validate_snapshot(ControlNamespace::Catalog, name, expected)?;
        self.validate_snapshot(ControlNamespace::Journals, &prepared.name, prepared)?;
        let catalog = self.namespace_handle(ControlNamespace::Catalog);
        let journals = self.namespace_handle(ControlNamespace::Journals);
        nix::fcntl::renameat(journals, prepared.name.as_str(), catalog, name)?;
        catalog.sync_all()?;
        journals.sync_all()?;
        let published = self
            .read(ControlNamespace::Catalog, name)?
            .ok_or_else(|| eyre::eyre!("prepared catalog publication disappeared"))?;
        ensure!(
            published.file_identity() == prepared.file_identity()
                && published.sha256() == prepared.sha256()
                && published.bytes() == prepared.bytes(),
            "prepared catalog publication identity or bytes changed"
        );
        Ok(published)
    }

    pub fn remove(&mut self, expected: &ControlSnapshot) -> Result<()> {
        self.remove_with_before_sync(expected, || Ok(()))
    }

    /// The target remains the same validated snapshot. The hook can only
    /// interrupt the syscall-to-durability boundary; it receives no IO rights.
    #[doc(hidden)]
    pub fn remove_with_before_sync(
        &mut self,
        expected: &ControlSnapshot,
        before_sync: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.validate_snapshot(expected.namespace, &expected.name, expected)?;
        let parent = &self.read.anchor.namespaces[expected.namespace.index()];
        nix::unistd::unlinkat(
            parent,
            expected.name.as_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )?;
        before_sync()?;
        parent.sync_all()?;
        self.ensure_catalog()?;
        Ok(())
    }
    /// Make an already observed metadata namespace durable, including when a
    /// recorded removal happened before interruption and its leaf is absent.
    pub fn sync_namespace(&self, namespace: ControlNamespace) -> Result<()> {
        self.ensure_catalog()?;
        self.read.anchor.namespaces[namespace.index()].sync_all()?;
        self.check_binding()
    }
}
