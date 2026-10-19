//! Stable kernel locks and the project → sorted objects → catalog protocol.

use super::{
    FileIdentity, ensure_entry_identity, open_existing_owned_regular,
    open_existing_readonly_regular, validate_owned_directory, validate_owned_regular,
};
use eyre::{Result, ensure};
use nix::fcntl::{Flock, FlockArg, OFlag, openat};
use nix::sys::stat::{Mode, fchmod};
use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct LockAnchor {
    parent: File,
    name: String,
    pin: File,
    uid: u32,
    identity: FileIdentity,
}

/// An existing inode retained independently of each acquisition. Drop never
/// removes its directory entry.
#[derive(Debug, Clone)]
pub struct StableLock(Arc<LockAnchor>);

impl StableLock {
    /// Initialize exactly one new lock. The caller must separately authorize
    /// creation; a missing registered lock must not be repaired here.
    pub fn create_new(parent: &File, name: &str, uid: u32) -> Result<Self> {
        validate_lock_parent(parent, name, uid)?;
        let pin = File::from(openat(
            parent,
            name,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        fchmod(&pin, Mode::from_bits_truncate(0o600))?;
        let identity = FileIdentity::of(&pin)?;
        let lock = Self(Arc::new(LockAnchor {
            parent: parent.try_clone()?,
            name: name.into(),
            pin,
            uid,
            identity,
        }));
        lock.check_binding()?;
        lock.0.pin.sync_all()?;
        parent.sync_all()?;
        lock.check_binding()?;
        Ok(lock)
    }

    pub fn open_existing(
        parent: &File,
        name: &str,
        uid: u32,
        expected: FileIdentity,
    ) -> Result<Self> {
        validate_lock_parent(parent, name, uid)?;
        let pin = open_existing_readonly_regular(parent, name, uid)?;
        ensure!(
            FileIdentity::of(&pin)? == expected,
            "stable lock identity changed"
        );
        let lock = Self(Arc::new(LockAnchor {
            parent: parent.try_clone()?,
            name: name.into(),
            pin,
            uid,
            identity: expected,
        }));
        lock.check_binding()?;
        Ok(lock)
    }
    pub fn identity(&self) -> FileIdentity {
        self.0.identity
    }
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub fn check_location(&self, parent: &File, leaf: &str) -> Result<()> {
        validate_lock_parent(parent, leaf, self.0.uid)?;
        ensure!(
            leaf == self.0.name && FileIdentity::of(parent)? == FileIdentity::of(&self.0.parent)?,
            "stable lock belongs to another object or namespace"
        );
        self.check_binding()
    }
    pub fn check_binding(&self) -> Result<()> {
        validate_lock_parent(&self.0.parent, &self.0.name, self.0.uid)?;
        validate_owned_regular(&self.0.pin, self.0.uid)?;
        ensure!(
            self.0.pin.metadata()?.mode() & 0o7777 == 0o600,
            "stable lock is not private"
        );
        ensure!(
            FileIdentity::of(&self.0.pin)? == self.0.identity,
            "stable lock identity changed"
        );
        ensure_entry_identity(&self.0.parent, &self.0.name, self.0.identity)
    }
    pub fn try_acquire(&self, mode: LeaseMode, deadline: Instant) -> Result<Option<ObjectLease>> {
        check_deadline(deadline)?;
        self.check_binding()?;
        // A fresh open creates an independent open-file description. Duplicating
        // the pin could let flock treat two acquisitions as the same owner.
        let file = match mode {
            LeaseMode::Shared => {
                open_existing_readonly_regular(&self.0.parent, &self.0.name, self.0.uid)?
            }
            LeaseMode::Exclusive => {
                open_existing_owned_regular(&self.0.parent, &self.0.name, self.0.uid)?
            }
        };
        ensure!(
            FileIdentity::of(&file)? == self.0.identity,
            "stable lock identity changed"
        );
        let held = try_lock_file(file, mode)?;
        check_deadline(deadline)?;
        self.check_binding()?;
        check_deadline(deadline)?;
        let Some(held) = held else {
            return Ok(None);
        };
        let lease = ObjectLease(Arc::new(HeldLock {
            anchor: self.clone(),
            held,
            mode,
        }));
        lease.check_binding()?;
        check_deadline(deadline)?;
        Ok(Some(lease))
    }
    pub fn acquire(&self, mode: LeaseMode, deadline: Instant) -> Result<ObjectLease> {
        loop {
            if let Some(lease) = self.try_acquire(mode, deadline)? {
                return Ok(lease);
            }
            wait_until_retry(deadline)?;
        }
    }
}

fn validate_lock_parent(parent: &File, name: &str, uid: u32) -> Result<()> {
    ensure!(
        uid == nix::unistd::geteuid().as_raw(),
        "stable lock uid is not authorized"
    );
    ensure!(
        !name.is_empty()
            && name.len() <= 255
            && !matches!(name, "." | "..")
            && !name.as_bytes().contains(&b'/')
            && !name.as_bytes().contains(&0),
        "invalid stable lock name"
    );
    validate_owned_directory(parent, uid)
}

/// The single flock entry point, including legacy allocated-prefix owners.
/// The caller owns file validation and post-acquisition namespace checks.
#[doc(hidden)]
pub fn try_lock_file(file: File, mode: LeaseMode) -> Result<Option<Flock<File>>> {
    let operation = match mode {
        LeaseMode::Shared => FlockArg::LockSharedNonblock,
        LeaseMode::Exclusive => FlockArg::LockExclusiveNonblock,
    };
    match Flock::lock(file, operation) {
        Ok(held) => Ok(Some(held)),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, error)) => Err(error.into()),
    }
}

/// Wait using the original absolute deadline. Legacy callers must separately
/// validate the file and recheck its binding after acquiring the guard.
#[doc(hidden)]
pub fn lock_file_with_deadline(
    file: File,
    mode: LeaseMode,
    deadline: Instant,
) -> Result<Flock<File>> {
    loop {
        check_deadline(deadline)?;
        if let Some(held) = try_lock_file(file.try_clone()?, mode)? {
            check_deadline(deadline)?;
            return Ok(held);
        }
        wait_until_retry(deadline)?;
    }
}

pub(crate) fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "store operation deadline exceeded"
    );
    Ok(())
}
fn wait_until_retry(deadline: Instant) -> Result<()> {
    check_deadline(deadline)?;
    std::thread::sleep(
        Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
    );
    check_deadline(deadline)
}

#[derive(Debug)]
struct HeldLock {
    anchor: StableLock,
    held: Flock<File>,
    mode: LeaseMode,
}

/// Clones retain the same kernel guard. No method exposes its writable file.
#[derive(Debug, Clone)]
pub struct ObjectLease(Arc<HeldLock>);
impl ObjectLease {
    pub fn check_binding(&self) -> Result<()> {
        self.0.anchor.check_binding()?;
        validate_owned_regular(&self.0.held, self.0.anchor.0.uid)?;
        ensure!(
            self.0.held.metadata()?.mode() & 0o7777 == 0o600,
            "held lock is not private"
        );
        ensure!(
            FileIdentity::of(&self.0.held)? == self.identity(),
            "held lock identity changed"
        );
        Ok(())
    }
    pub fn identity(&self) -> FileIdentity {
        self.0.anchor.identity()
    }
    pub fn mode(&self) -> LeaseMode {
        self.0.mode
    }
}

/// Bounds both retained object keys and raw batch input before deduplication.
#[derive(Debug, Clone, Copy)]
pub struct LockLimits {
    pub max_objects: usize,
    pub max_total_key_bytes: usize,
}

impl Default for LockLimits {
    fn default() -> Self {
        Self {
            max_objects: 1024,
            max_total_key_bytes: 1024 * 1024,
        }
    }
}

/// One operation scope, with no API for releasing an earlier phase and
/// continuing under stale observations. Drop releases the retained guards.
#[derive(Debug)]
pub struct OrderedLocks {
    deadline: Instant,
    limits: LockLimits,
    key_bytes: usize,
    project: Option<ObjectLease>,
    objects: BTreeMap<String, ObjectLease>,
    catalog: Option<ObjectLease>,
}
impl OrderedLocks {
    pub fn with_limits(deadline: Instant, limits: LockLimits) -> Result<Self> {
        check_deadline(deadline)?;
        ensure!(
            limits.max_objects != 0 && limits.max_total_key_bytes != 0,
            "lock budgets must be nonzero"
        );
        Ok(Self {
            limits,
            ..Self::new(deadline)
        })
    }
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            limits: LockLimits::default(),
            key_bytes: 0,
            project: None,
            objects: BTreeMap::new(),
            catalog: None,
        }
    }
    pub fn acquire_project(&mut self, lock: &StableLock) -> Result<ObjectLease> {
        check_deadline(self.deadline)?;
        ensure!(
            self.objects.is_empty() && self.catalog.is_none(),
            "project lock must precede objects and catalog"
        );
        lock.check_binding()?;
        if let Some(held) = &self.project {
            check_same_lease(held, lock, LeaseMode::Exclusive)?;
            return Ok(held.clone());
        }
        let held = lock.acquire(LeaseMode::Exclusive, self.deadline)?;
        self.project = Some(held.clone());
        Ok(held)
    }
    pub fn try_object(
        &mut self,
        key: &str,
        lock: &StableLock,
        mode: LeaseMode,
    ) -> Result<Option<ObjectLease>> {
        check_deadline(self.deadline)?;
        self.check_earlier_bindings()?;
        if let Some(held) = self.check_object(key, lock, mode)? {
            return Ok(Some(held));
        }
        let Some(held) = lock.try_acquire(mode, self.deadline)? else {
            return Ok(None);
        };
        self.check_earlier_bindings()?;
        self.key_bytes += key.len();
        self.objects.insert(key.into(), held.clone());
        Ok(Some(held))
    }
    pub fn acquire_objects(
        &mut self,
        objects: &[(&str, &StableLock, LeaseMode)],
    ) -> Result<Vec<ObjectLease>> {
        check_deadline(self.deadline)?;
        ensure!(self.catalog.is_none(), "objects must precede catalog");
        // Reject oversized raw input before building a map, sorting or dropping
        // duplicate keys. Duplicates do not exempt caller input from limits.
        ensure!(
            objects.len() <= self.limits.max_objects,
            "object batch count budget exceeded"
        );
        let mut raw_key_bytes = 0usize;
        for (key, _, _) in objects {
            raw_key_bytes = raw_key_bytes
                .checked_add(key.len())
                .filter(|total| *total <= self.limits.max_total_key_bytes)
                .ok_or_else(|| eyre::eyre!("object batch key byte budget exceeded"))?;
        }
        self.check_earlier_bindings()?;
        let mut sorted = BTreeMap::<&str, (&StableLock, LeaseMode)>::new();
        for &(key, lock, mode) in objects {
            self.check_object(key, lock, mode)?;
            if let Some((previous, previous_mode)) = sorted.get(key) {
                ensure!(
                    previous.identity() == lock.identity() && *previous_mode == mode,
                    "conflicting object request"
                );
            } else {
                ensure!(
                    !sorted
                        .values()
                        .any(|(other, _)| other.identity() == lock.identity()),
                    "one object lock has multiple ordering keys"
                );
                sorted.insert(key, (lock, mode));
            }
        }
        // The raw batch is bounded; now ensure the union with already retained
        // locks also fits before acquiring any new member of this batch.
        let mut retained_count = self.objects.len();
        let mut retained_bytes = self.key_bytes;
        for key in sorted
            .keys()
            .filter(|key| !self.objects.contains_key(**key))
        {
            retained_count = retained_count
                .checked_add(1)
                .filter(|total| *total <= self.limits.max_objects)
                .ok_or_else(|| eyre::eyre!("retained object count budget exceeded"))?;
            retained_bytes = retained_bytes
                .checked_add(key.len())
                .filter(|total| *total <= self.limits.max_total_key_bytes)
                .ok_or_else(|| eyre::eyre!("retained object key byte budget exceeded"))?;
        }
        let mut result = Vec::with_capacity(sorted.len());
        for (key, (lock, mode)) in sorted {
            if let Some(held) = self.check_object(key, lock, mode)? {
                result.push(held);
            } else {
                let held = lock.acquire(mode, self.deadline)?;
                self.key_bytes += key.len();
                self.objects.insert(key.into(), held.clone());
                result.push(held);
            }
        }
        self.check_earlier_bindings()?;
        Ok(result)
    }
    pub fn catalog(&mut self, lock: &StableLock, mode: LeaseMode) -> Result<ObjectLease> {
        check_deadline(self.deadline)?;
        lock.check_binding()?;
        self.check_earlier_bindings()?;
        if let Some(held) = &self.catalog {
            check_same_lease(held, lock, mode)?;
            return Ok(held.clone());
        }
        ensure!(
            !self
                .project
                .as_ref()
                .is_some_and(|held| held.identity() == lock.identity())
                && !self
                    .objects
                    .values()
                    .any(|held| held.identity() == lock.identity()),
            "catalog lock aliases an earlier lock role"
        );
        let held = lock.acquire(mode, self.deadline)?;
        // An earlier protection can be replaced while this scope waits for
        // catalog admission. Recheck it before returning commit capability.
        self.check_earlier_bindings()?;
        self.catalog = Some(held.clone());
        Ok(held)
    }
    fn check_earlier_bindings(&self) -> Result<()> {
        check_deadline(self.deadline)?;
        for held in self.project.iter().chain(self.objects.values()) {
            check_deadline(self.deadline)?;
            held.check_binding()?;
        }
        check_deadline(self.deadline)
    }
    fn check_object(
        &self,
        key: &str,
        lock: &StableLock,
        mode: LeaseMode,
    ) -> Result<Option<ObjectLease>> {
        check_deadline(self.deadline)?;
        ensure!(self.catalog.is_none(), "objects must precede catalog");
        ensure!(
            !key.is_empty() && key.len() <= 4096 && !key.as_bytes().contains(&0),
            "invalid object ordering key"
        );
        lock.check_binding()?;
        if let Some(held) = self.objects.get(key) {
            check_same_lease(held, lock, mode)?;
            return Ok(Some(held.clone()));
        }
        ensure!(
            self.objects.len() < self.limits.max_objects,
            "retained object count budget exceeded"
        );
        ensure!(
            self.key_bytes
                .checked_add(key.len())
                .is_some_and(|total| total <= self.limits.max_total_key_bytes),
            "retained object key byte budget exceeded"
        );
        ensure!(
            self.objects
                .last_key_value()
                .is_none_or(|(last, _)| last.as_str() < key),
            "object locks must follow stable key order"
        );
        ensure!(
            !self
                .objects
                .values()
                .any(|held| held.identity() == lock.identity()),
            "one object lock has multiple ordering keys"
        );
        ensure!(
            !self
                .project
                .as_ref()
                .is_some_and(|held| held.identity() == lock.identity()),
            "object lock aliases project protection"
        );
        Ok(None)
    }
}

fn check_same_lease(held: &ObjectLease, lock: &StableLock, mode: LeaseMode) -> Result<()> {
    ensure!(
        held.identity() == lock.identity(),
        "lock identity changed within operation"
    );
    ensure!(
        held.mode() == mode,
        "lock mode changes and upgrades are forbidden"
    );
    held.check_binding()
}

#[cfg(test)]
pub(crate) mod tests;
