use super::*;
use sha2::{Digest as _, Sha256};

#[derive(Debug)]
pub struct ReadStore {
    anchor: Anchor,
    deadline: Instant,
}

impl ReadStore {
    /// Missing or genuinely empty roots are uninitialized, without side effects.
    pub fn open_readonly(
        root: &Path,
        uid: u32,
        limits: RootLimits,
        deadline: Instant,
    ) -> Result<Option<Self>> {
        let path = authorized_root(root, uid, limits, deadline)?;
        let directory = match open_trusted_store_with_deadline(&path, uid, false, deadline) {
            Ok(directory) => directory,
            Err(error) if is_missing(&error) => {
                check_deadline(deadline)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match open_existing_readonly_regular(&directory, LOCK_NAME, uid) {
            Ok(_) => {}
            Err(error) if is_missing(&error) && directory_empty(&directory)? => {
                check_deadline(deadline)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        Ok(Some(Self {
            anchor: existing_anchor(path, directory, uid, deadline)?,
            deadline,
        }))
    }
    pub fn control(&self, limits: ControlLimits) -> Result<ReadControl<'_>> {
        ReadControl::new(&self.anchor, limits, self.deadline, LeaseMode::Shared)
    }
    pub fn root_path(&self) -> &Path {
        &self.anchor.path
    }
    pub fn check_binding(&self) -> Result<()> {
        check_deadline(self.deadline)?;
        self.anchor.check_binding(self.deadline)?;
        check_deadline(self.deadline)
    }
}

/// A query scope retains actual SH protection until it is dropped. It exposes
/// bounded bytes and identities, never a writable handle or a writer factory.
pub struct ReadControl<'a> {
    pub(super) anchor: &'a Anchor,
    pub(super) limits: ControlLimits,
    deadline: Instant,
    mode: LeaseMode,
    locks: RefCell<OrderedLocks>,
}
impl<'a> ReadControl<'a> {
    pub(super) fn new(
        anchor: &'a Anchor,
        limits: ControlLimits,
        deadline: Instant,
        mode: LeaseMode,
    ) -> Result<Self> {
        let limits = limits.validate()?;
        check_deadline(deadline)?;
        anchor.check_binding(deadline)?;
        Ok(Self {
            anchor,
            limits,
            deadline,
            mode,
            locks: RefCell::new(OrderedLocks::new(deadline)),
        })
    }
    pub fn check_binding(&self) -> Result<()> {
        check_deadline(self.deadline)?;
        self.anchor.check_binding(self.deadline)?;
        check_deadline(self.deadline)
    }
    pub fn ensure_catalog(&self) -> Result<()> {
        self.check_binding()?;
        self.locks
            .borrow_mut()
            .catalog(&self.anchor.catalog, self.mode)?;
        self.check_binding()
    }
    pub fn try_object(
        &self,
        key: &str,
        lock: &StableLock,
        mode: LeaseMode,
    ) -> Result<Option<ObjectLease>> {
        ensure!(
            self.mode == LeaseMode::Exclusive || mode == LeaseMode::Shared,
            "readonly control cannot acquire write protection"
        );
        self.check_binding()?;
        ensure!(
            !key.is_empty()
                && key.len() <= 250
                && !key.as_bytes().contains(&b'/')
                && !key.as_bytes().contains(&0),
            "invalid store object key"
        );
        let name = format!("{key}.lock");
        lock.check_location(&self.anchor.build, &name)?;
        let result = self.locks.borrow_mut().try_object(key, lock, mode)?;
        lock.check_location(&self.anchor.build, &name)?;
        self.check_binding()?;
        Ok(result)
    }
    pub(super) fn acquire_project(&self, lock: &StableLock) -> Result<ObjectLease> {
        ensure!(
            self.mode == LeaseMode::Exclusive,
            "readonly control cannot acquire project write protection"
        );
        self.check_binding()?;
        lock.check_location(&self.anchor.build, lock.name())?;
        let result = self.locks.borrow_mut().acquire_project(lock)?;
        lock.check_location(&self.anchor.build, lock.name())?;
        self.check_binding()?;
        Ok(result)
    }
    pub fn store_identity(&self) -> Result<(u64, u64)> {
        self.ensure_catalog()?;
        let metadata = self.anchor.root.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    pub(super) fn validate_file(&self, file: &File) -> Result<()> {
        validate_metadata_file(file, self.anchor.uid)
    }
    pub(super) fn validate_name(&self, name: &str) -> Result<()> {
        validate_name(name, self.limits)
    }
    pub fn list_names(
        &self,
        namespace: ControlNamespace,
        max_entries: usize,
        max_total_name_bytes: usize,
    ) -> Result<Vec<String>> {
        ensure!(
            max_entries != 0 && max_total_name_bytes != 0,
            "environment control listing limits must be nonzero"
        );
        self.ensure_catalog()?;
        let parent = &self.anchor.namespaces[namespace.index()];
        let mut directory = nix::dir::Dir::openat(
            parent,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let mut names = Vec::new();
        let mut name_bytes = 0usize;
        for entry in directory.iter() {
            check_deadline(self.deadline)?;
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            ensure!(
                names.len() < max_entries,
                "environment control listing entry limit exceeded"
            );
            name_bytes = name_bytes
                .checked_add(bytes.len())
                .filter(|total| *total <= max_total_name_bytes)
                .ok_or_else(|| {
                    eyre::eyre!("environment control listing name byte limit exceeded")
                })?;
            let name = std::str::from_utf8(bytes)
                .map_err(|_| eyre::eyre!("invalid environment control metadata name"))?;
            self.validate_name(name)?;
            let file = File::from(openat(
                parent,
                name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?);
            self.validate_file(&file)?;
            ensure_entry_identity(parent, name, FileIdentity::of(&file)?)?;
            names.push(name.to_owned());
        }
        self.ensure_catalog()?;
        names.sort_unstable();
        Ok(names)
    }

    pub fn read_bounded(
        &self,
        namespace: ControlNamespace,
        name: &str,
        max_bytes: u64,
    ) -> Result<Option<ControlSnapshot>> {
        ensure!(
            max_bytes != 0,
            "environment control read budget must be nonzero"
        );
        self.read_with_limit(namespace, name, max_bytes.min(self.limits.max_file_bytes))
    }

    pub fn read(&self, namespace: ControlNamespace, name: &str) -> Result<Option<ControlSnapshot>> {
        self.read_with_limit(namespace, name, self.limits.max_file_bytes)
    }

    fn read_with_limit(
        &self,
        namespace: ControlNamespace,
        name: &str,
        max_bytes: u64,
    ) -> Result<Option<ControlSnapshot>> {
        self.read_with_limit_and_hook(namespace, name, max_bytes, || Ok(()))
    }

    pub(super) fn read_with_limit_and_hook(
        &self,
        namespace: ControlNamespace,
        name: &str,
        max_bytes: u64,
        before_open: impl FnOnce() -> Result<()>,
    ) -> Result<Option<ControlSnapshot>> {
        self.validate_name(name)?;
        self.ensure_catalog()?;
        before_open()?;
        let parent = &self.anchor.namespaces[namespace.index()];
        let file = match openat(
            parent,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(nix::errno::Errno::ENOENT) => {
                self.ensure_catalog()?;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        self.validate_file(&file)?;
        let identity = FileIdentity::of(&file)?;
        ensure_entry_identity(parent, name, identity)?;
        let size = file.metadata()?.len();
        ensure!(
            size <= max_bytes,
            "environment control metadata byte limit exceeded"
        );
        let mut bytes = Vec::new();
        (&file).take(max_bytes + 1).read_to_end(&mut bytes)?;
        ensure!(
            (bytes.len() as u64) <= max_bytes,
            "environment control metadata byte limit exceeded"
        );
        self.validate_file(&file)?;
        ensure!(
            file.metadata()?.len() == size && bytes.len() as u64 == size,
            "environment control metadata changed while reading"
        );
        ensure_entry_identity(parent, name, identity)?;
        self.ensure_catalog()?;
        Ok(Some(ControlSnapshot {
            namespace,
            name: name.into(),
            sha256: Sha256::digest(&bytes).into(),
            bytes,
            store_identity: FileIdentity::of(&self.anchor.root)?,
            parent_identity: FileIdentity::of(parent)?,
            entry_identity: identity,
            _inode_pin: file,
        }))
    }

    pub(super) fn validate_snapshot(
        &self,
        namespace: ControlNamespace,
        name: &str,
        expected: &ControlSnapshot,
    ) -> Result<()> {
        self.validate_name(name)?;
        self.ensure_catalog()?;
        ensure!(
            expected.namespace == namespace
                && expected.name == name
                && expected.store_identity == FileIdentity::of(&self.anchor.root)?
                && expected.parent_identity
                    == FileIdentity::of(&self.anchor.namespaces[namespace.index()])?,
            "environment control snapshot belongs to another destination"
        );
        let current = self
            .read(namespace, name)?
            .ok_or_else(|| eyre::eyre!("expected environment control metadata is missing"))?;
        ensure!(
            current.entry_identity == expected.entry_identity
                && current.sha256 == expected.sha256
                && current.bytes == expected.bytes,
            "environment control metadata changed since its snapshot"
        );
        Ok(())
    }
}
