//! Private project-lock IO. No Python graph, scope or publication authority.

use std::path::{Path, PathBuf};
use std::time::Instant;

use eyre::Result;

mod dangling;
pub(crate) use dangling::PreparedNativeDanglingLock;
mod observation;
pub(crate) use observation::{
    ReadOnlyFileError, ReadOnlyFileLimits, ReadOnlyFileObservation, ReadOnlyPathObservation,
};

#[cfg(unix)]
use unix::*;

pub(crate) struct ProjectLockSession {
    target: PathBuf,
    #[cfg(unix)]
    io: SessionIo,
}

impl std::fmt::Debug for ProjectLockSession {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("ProjectLockSession")
            .finish_non_exhaustive()
    }
}

pub(crate) struct LockImage {
    bytes: Option<Vec<u8>>,
    sha256: Option<[u8; 32]>,
    #[cfg(unix)]
    session: std::sync::Arc<()>,
    #[cfg(unix)]
    pin: Option<PinnedFile>,
}
impl LockImage {
    /// Descriptive identity only; mutation still requires this session's image.
    pub(crate) fn file_identity(&self) -> Option<(u64, u64)> {
        #[cfg(unix)]
        return self.pin.as_ref().map(PinnedFile::identity);
        #[cfg(not(unix))]
        None
    }
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
    pub(crate) fn sha256(&self) -> Option<&[u8; 32]> {
        self.sha256.as_ref()
    }
    pub(crate) fn exists(&self) -> bool {
        self.bytes.is_some()
    }
}
impl std::fmt::Debug for LockImage {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("LockImage")
            .field("exists", &self.exists())
            .field("size", &self.bytes.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

impl ProjectLockSession {
    pub(crate) fn acquire(path: &Path, max_bytes: usize, deadline: Instant) -> Result<Self> {
        #[cfg(unix)]
        return unix::acquire(path, max_bytes, deadline);
        #[cfg(not(unix))]
        {
            let _ = (path, max_bytes, deadline);
            eyre::bail!("project lock IO requires Unix");
        }
    }
    pub(crate) fn canonical_target(&self) -> &Path {
        &self.target
    }
    pub(crate) fn snapshot(&self) -> Result<LockImage> {
        #[cfg(unix)]
        return self.io.snapshot();
        #[cfg(not(unix))]
        eyre::bail!("project lock IO requires Unix")
    }
    pub(crate) fn recheck(&self, expected: &LockImage) -> Result<()> {
        #[cfg(unix)]
        return self.io.recheck(expected);
        #[cfg(not(unix))]
        {
            let _ = expected;
            eyre::bail!("project lock IO requires Unix");
        }
    }
    pub(crate) fn compare_and_replace(
        &mut self,
        expected: &LockImage,
        new_bytes: &[u8],
    ) -> Result<LockImage> {
        #[cfg(unix)]
        return self.io.replace(expected, new_bytes, |_| {});
        #[cfg(not(unix))]
        {
            let _ = (expected, new_bytes);
            eyre::bail!("project lock IO requires Unix");
        }
    }
    /// Persist the prepared inode in the caller's journal before publication.
    pub(crate) fn compare_and_replace_journaled(
        &mut self,
        expected: &LockImage,
        new_bytes: &[u8],
        staged: impl FnOnce(&Path, (u64, u64)) -> Result<()>,
    ) -> Result<LockImage> {
        #[cfg(unix)]
        return self.io.replace_journaled(expected, new_bytes, staged);
        #[cfg(not(unix))]
        {
            let _ = (expected, new_bytes, staged);
            eyre::bail!("project lock IO requires Unix")
        }
    }
    /// Install publication of the prepared bytes. The current entry's content
    /// is not a write precondition, and the write result is taken from the
    /// temporary this session itself wrote and renamed; the current entry is
    /// never re-read to define the outcome.
    pub(crate) fn replace_journaled_install(
        &mut self,
        expected: &LockImage,
        new_bytes: &[u8],
        staged: impl FnOnce(&Path, (u64, u64)) -> Result<()>,
    ) -> Result<LockImage> {
        #[cfg(unix)]
        return self
            .io
            .replace_journaled_install(expected, new_bytes, staged);
        #[cfg(not(unix))]
        {
            let _ = (expected, new_bytes, staged);
            eyre::bail!("project lock IO requires Unix")
        }
    }
    /// Binding, session-state and deadline check only. Unlike
    /// [`Self::recheck`], this never compares the observed entry's current
    /// on-disk bytes, digest, inode or existence.
    pub(crate) fn check_binding(&self) -> Result<()> {
        #[cfg(unix)]
        return self.io.check_binding();
        #[cfg(not(unix))]
        Ok(())
    }
    pub(crate) fn compare_and_remove(&mut self, expected: &LockImage) -> Result<LockImage> {
        #[cfg(unix)]
        return self.io.remove(expected);
        #[cfg(not(unix))]
        {
            let _ = expected;
            eyre::bail!("project lock IO requires Unix")
        }
    }
    pub(crate) fn cleanup_staged(
        &self,
        path: &Path,
        identity: (u64, u64),
        bytes: &[u8],
    ) -> Result<()> {
        #[cfg(unix)]
        return self.io.cleanup_staged(path, identity, bytes);
        #[cfg(not(unix))]
        {
            let _ = (path, identity, bytes);
            eyre::bail!("project lock IO requires Unix")
        }
    }
    #[cfg(all(test, unix))]
    fn acquire_with_guard_hook(
        path: &Path,
        maximum: usize,
        deadline: Instant,
        hook: impl FnMut(GuardAcquireStage),
    ) -> Result<Self> {
        unix::acquire_with_guard_hook(path, maximum, deadline, hook)
    }
    #[cfg(all(test, unix))]
    fn compare_with_hook(
        &mut self,
        expected: &LockImage,
        new_bytes: &[u8],
        hook: impl FnOnce(&Path),
    ) -> Result<LockImage> {
        self.io.replace(expected, new_bytes, hook)
    }
}

#[cfg(unix)]
mod unix {
    pub(super) mod read_only;
    use super::*;
    use eyre::{Context, ContextCompat, ensure};
    use nix::errno::Errno;
    use nix::fcntl::{AtFlags, Flock, FlockArg, OFlag, open, openat, readlinkat, renameat};
    use nix::sys::stat::{FileStat, Mode, SFlag, fchmod, fstat, fstatat};
    use nix::unistd::{UnlinkatFlags, unlinkat};
    use sha2::{Digest as _, Sha256};
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::time::Duration;

    const GUARD: &str = ".pakro.lock.guard";
    const READ_FLAGS: OFlag = OFlag::O_RDONLY
        .union(OFlag::O_NOFOLLOW)
        .union(OFlag::O_NONBLOCK)
        .union(OFlag::O_CLOEXEC);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Stamp {
        device: nix::libc::dev_t,
        inode: nix::libc::ino_t,
        mode: nix::libc::mode_t,
        links: nix::libc::nlink_t,
        owner: nix::libc::uid_t,
        group: nix::libc::gid_t,
        size: nix::libc::off_t,
        modified: nix::libc::time_t,
        modified_ns: nix::libc::c_long,
        changed: nix::libc::time_t,
        changed_ns: nix::libc::c_long,
    }
    impl Stamp {
        fn of(stat: FileStat) -> Self {
            Self {
                device: stat.st_dev,
                inode: stat.st_ino,
                mode: stat.st_mode,
                links: stat.st_nlink,
                owner: stat.st_uid,
                group: stat.st_gid,
                size: stat.st_size,
                modified: stat.st_mtime,
                modified_ns: stat.st_mtime_nsec,
                changed: stat.st_ctime,
                changed_ns: stat.st_ctime_nsec,
            }
        }
        fn file(file: &File) -> Result<Self> {
            Ok(Self::of(fstat(file)?))
        }
        fn kind(self) -> SFlag {
            SFlag::from_bits_truncate(self.mode) & SFlag::S_IFMT
        }
        fn identity(self) -> (nix::libc::dev_t, nix::libc::ino_t) {
            (self.device, self.inode)
        }
        fn regular(self) -> Result<()> {
            ensure!(
                self.kind() == SFlag::S_IFREG
                    && self.links == 1
                    && self.mode & 0o7000 == 0
                    && self.size >= 0,
                "project lock target must be a regular single-link file without privileged mode bits"
            );
            Ok(())
        }
        fn guard(self) -> Result<()> {
            self.regular()?;
            ensure!(
                self.owner == nix::unistd::geteuid().as_raw()
                    && self.mode & 0o7777 == 0o600
                    && self.size == 0,
                "project lock guard must be owned, private and empty"
            );
            Ok(())
        }
    }

    struct Parent {
        path: PathBuf,
        file: File,
        observed: Stamp,
    }
    impl Parent {
        fn open(path: &Path) -> Result<Self> {
            let path = path
                .canonicalize()
                .context("project lock parent cannot be resolved")?;
            let file = File::from(open(&path, READ_FLAGS | OFlag::O_DIRECTORY, Mode::empty())?);
            let observed = Stamp::file(&file)?;
            ensure!(
                observed.kind() == SFlag::S_IFDIR,
                "project lock parent is not a directory"
            );
            let parent = Self {
                path,
                file,
                observed,
            };
            parent.check()?;
            Ok(parent)
        }
        fn check(&self) -> Result<()> {
            let current = File::from(
                open(&self.path, READ_FLAGS | OFlag::O_DIRECTORY, Mode::empty())
                    .context("project lock parent namespace changed")?,
            );
            ensure!(
                Stamp::file(&current)?.identity() == self.observed.identity()
                    && Stamp::file(&self.file)?.identity() == self.observed.identity(),
                "project lock parent identity changed"
            );
            Ok(())
        }
        fn named(&self, name: &OsStr) -> Result<Option<Stamp>> {
            match fstatat(&self.file, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(stat) => Ok(Some(Stamp::of(stat))),
                Err(Errno::ENOENT) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
        fn duplicate(&self) -> Result<Self> {
            self.check()?;
            Ok(Self {
                path: self.path.clone(),
                file: self.file.try_clone()?,
                observed: self.observed,
            })
        }
    }

    pub(super) struct AliasPin {
        parent: Parent,
        name: OsString,
        file: File,
        observed: Stamp,
        link: OsString,
        link_limit: Option<usize>,
    }
    impl AliasPin {
        pub(super) fn capture(path: &Path) -> Result<Self> {
            let name = target_name(path)?;
            let parent = Parent::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new(".")),
            )?;
            Self::open(parent, name)
        }
        fn open(parent: Parent, name: OsString) -> Result<Self> {
            Self::open_with_policy(parent, name, true, None)
        }
        fn open_with_policy(
            parent: Parent,
            name: OsString,
            single_link: bool,
            link_limit: Option<usize>,
        ) -> Result<Self> {
            let before = parent
                .named(&name)?
                .context("project lock alias disappeared")?;
            ensure!(
                before.kind() == SFlag::S_IFLNK && (!single_link || before.links == 1),
                "project lock alias is not a single-link symlink"
            );
            #[cfg(target_os = "macos")]
            let flags = OFlag::O_RDONLY
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC
                | OFlag::from_bits_retain(nix::libc::O_SYMLINK);
            #[cfg(target_os = "linux")]
            let flags = OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            eyre::bail!("project lock symlink pinning is unavailable on this Unix platform");
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                let file = File::from(openat(
                    &parent.file,
                    name.as_os_str(),
                    flags,
                    Mode::empty(),
                )?);
                ensure!(
                    Stamp::file(&file)? == before,
                    "project lock alias changed while opening"
                );
                let link = read_alias_link(&parent.file, name.as_os_str(), link_limit)?;
                let alias = Self {
                    parent,
                    name,
                    file,
                    observed: before,
                    link,
                    link_limit,
                };
                alias.check()?;
                Ok(alias)
            }
        }
        fn check(&self) -> Result<()> {
            self.parent.check()?;
            ensure!(
                self.parent.named(&self.name)? == Some(self.observed)
                    && Stamp::file(&self.file)? == self.observed
                    && read_alias_link(&self.parent.file, self.name.as_os_str(), self.link_limit)?
                        == self.link,
                "project lock original alias changed"
            );
            Ok(())
        }
        fn path(&self) -> PathBuf {
            self.parent.path.join(&self.name)
        }
        pub(super) fn check_dangling(&self) -> Result<()> {
            self.check()?;
            match self.path().canonicalize() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("native dangling lock target is not proven absent");
                }
                Ok(_) => eyre::bail!("native dangling lock target now exists"),
            }
            self.check()
        }
    }

    #[derive(Debug, thiserror::Error)]
    enum AliasReadFailure {
        #[error("project alias link byte limit exceeded")]
        Bytes,
    }
    fn read_alias_link(parent: &File, name: &OsStr, maximum: Option<usize>) -> Result<OsString> {
        let Some(maximum) = maximum else {
            return Ok(readlinkat(parent, name)?);
        };
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let capacity = maximum.checked_add(1).ok_or(AliasReadFailure::Bytes)?;
        let name = std::ffi::CString::new(name.as_bytes())
            .context("project alias entry name is invalid")?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| eyre::eyre!("project alias bounded allocation failed"))?;
        bytes.resize(capacity, 0u8);
        // readlinkat writes at most capacity bytes and does not append a NUL.
        let count = unsafe {
            nix::libc::readlinkat(
                parent.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            return Err(Errno::last().into());
        }
        let count = usize::try_from(count).map_err(|_| AliasReadFailure::Bytes)?;
        ensure!(count <= maximum, AliasReadFailure::Bytes);
        bytes.truncate(count);
        Ok(OsString::from_vec(bytes))
    }

    struct Alias {
        pin: AliasPin,
        target: PathBuf,
    }
    impl Alias {
        fn open(parent: Parent, name: OsString) -> Result<Self> {
            let pin = AliasPin::open(parent, name)?;
            let target = pin
                .path()
                .canonicalize()
                .context("project lock alias is dangling or unavailable")?;
            let alias = Self { pin, target };
            alias.check()?;
            Ok(alias)
        }
        fn check(&self) -> Result<()> {
            self.pin.check()?;
            ensure!(
                self.pin
                    .path()
                    .canonicalize()
                    .context("project lock alias target disappeared")?
                    == self.target,
                "project lock original alias target changed"
            );
            self.pin.check()
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum GuardAcquireStage {
        CreatedBeforeInitialize,
        ObservedBeforeCheck,
    }

    struct GuardDomain {
        parent: Parent,
        guard: Flock<File>,
        observed: Stamp,
    }
    impl GuardDomain {
        fn acquire(parent: Parent, limit: Instant) -> Result<Self> {
            Self::acquire_with_hook(parent, limit, |_| {})
        }
        fn acquire_with_hook(
            parent: Parent,
            limit: Instant,
            mut hook: impl FnMut(GuardAcquireStage),
        ) -> Result<Self> {
            deadline(limit)?;
            let (file, created) = match openat(
                &parent.file,
                GUARD,
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_NONBLOCK
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            ) {
                Ok(file) => (File::from(file), true),
                Err(Errno::EEXIST) => (
                    File::from(openat(
                        &parent.file,
                        GUARD,
                        OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )?),
                    false,
                ),
                Err(error) => return Err(error.into()),
            };
            if created {
                hook(GuardAcquireStage::CreatedBeforeInitialize);
                let initial = Stamp::file(&file)?;
                ensure!(
                    initial.kind() == SFlag::S_IFREG
                        && initial.links == 1
                        && initial.owner == nix::unistd::geteuid().as_raw()
                        && initial.size == 0,
                    "new project lock guard identity is invalid"
                );
                if initial.mode & 0o7777 != 0o600 {
                    fchmod(&file, Mode::from_bits_truncate(0o600))?;
                }
                file.sync_all()?;
                parent.file.sync_all()?;
            }
            let observed = Stamp::file(&file)?;
            observed.guard()?;
            hook(GuardAcquireStage::ObservedBeforeCheck);
            let mut pending = file;
            let guard = loop {
                deadline(limit)?;
                parent.check()?;
                let named = parent.named(OsStr::new(GUARD))?;
                ensure!(
                    named == Some(observed),
                    "project lock guard identity changed: observed={observed:?}, named={named:?}"
                );
                match Flock::lock(pending, FlockArg::LockExclusiveNonblock) {
                    Ok(lock) => break lock,
                    Err((file, error)) if error == Errno::EWOULDBLOCK || error == Errno::EAGAIN => {
                        pending = file;
                        deadline(limit)?;
                        std::thread::sleep(
                            limit
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(5)),
                        );
                    }
                    Err((_, error)) => return Err(error.into()),
                }
            };
            let domain = Self {
                parent,
                guard,
                observed,
            };
            domain.check(limit)?;
            Ok(domain)
        }
        fn check(&self, limit: Instant) -> Result<()> {
            deadline(limit)?;
            self.parent.check()?;
            let current = Stamp::file(&self.guard)?;
            current.guard()?;
            let named = self.parent.named(OsStr::new(GUARD))?;
            ensure!(
                current == self.observed && named == Some(self.observed),
                "project lock guard binding changed: observed={:?}, current={current:?}, named={named:?}",
                self.observed
            );
            Ok(())
        }
    }

    pub(super) struct PinnedFile {
        file: File,
        observed: Stamp,
    }
    impl PinnedFile {
        pub(super) fn identity(&self) -> (u64, u64) {
            (self.observed.device as u64, self.observed.inode as u64)
        }
    }
    pub(super) struct SessionIo {
        domain: GuardDomain,
        name: OsString,
        alias: Option<Alias>,
        token: Arc<()>,
        max_bytes: usize,
        deadline: Instant,
        failed: bool,
    }

    fn deadline(limit: Instant) -> Result<()> {
        ensure!(Instant::now() < limit, "project lock deadline expired");
        Ok(())
    }
    fn target_name(path: &Path) -> Result<OsString> {
        let name = path
            .file_name()
            .context("project lock target filename is missing")?;
        ensure!(
            name != OsStr::new(GUARD),
            "project lock target uses the reserved guard name"
        );
        Ok(name.to_owned())
    }

    pub(super) fn acquire(
        path: &Path,
        max_bytes: usize,
        limit: Instant,
    ) -> Result<ProjectLockSession> {
        acquire_with_guard_hook(path, max_bytes, limit, |_| {})
    }
    pub(super) fn acquire_with_guard_hook(
        path: &Path,
        max_bytes: usize,
        limit: Instant,
        hook: impl FnMut(GuardAcquireStage),
    ) -> Result<ProjectLockSession> {
        ensure!(
            max_bytes != 0 && max_bytes.checked_add(1).is_some(),
            "project lock byte limit is invalid"
        );
        deadline(limit)?;
        let name = target_name(path)?;
        let input_parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = Parent::open(input_parent)?;
        let (parent, name, alias) = if parent
            .named(&name)?
            .is_some_and(|s| s.kind() == SFlag::S_IFLNK)
        {
            let alias = Alias::open(parent, name)?;
            let target_name = target_name(&alias.target)?;
            let target_parent = Parent::open(
                alias
                    .target
                    .parent()
                    .context("project lock target parent is missing")?,
            )?;
            (target_parent, target_name, Some(alias))
        } else {
            (parent, name, None)
        };
        let target = parent.path.join(&name);
        let domain = GuardDomain::acquire_with_hook(parent, limit, hook)?;
        let session = ProjectLockSession {
            target,
            io: SessionIo {
                domain,
                name,
                alias,
                token: Arc::new(()),
                max_bytes,
                deadline: limit,
                failed: false,
            },
        };
        session.snapshot()?;
        Ok(session)
    }

    #[derive(Clone, Copy)]
    enum EntryReadPolicy {
        LockTarget,
        ReadOnly,
    }
    #[derive(Debug, thiserror::Error)]
    enum ReadEntryFailure {
        #[error("project file byte limit exceeded")]
        Bytes,
        #[error("project file entry changed while reading")]
        Changed,
    }
    fn observed_sha256(bytes: &[u8], limit: Instant) -> Result<[u8; 32]> {
        let mut digest = Sha256::new();
        for chunk in bytes.chunks(64 * 1024) {
            deadline(limit)?;
            digest.update(chunk);
        }
        deadline(limit)?;
        Ok(digest.finalize().into())
    }
    fn read_entry(
        parent: &Parent,
        name: &OsStr,
        max: usize,
        limit: Instant,
    ) -> Result<Option<(Vec<u8>, PinnedFile)>> {
        read_entry_with_policy(parent, name, max, limit, EntryReadPolicy::LockTarget)
    }
    fn read_entry_with_policy(
        parent: &Parent,
        name: &OsStr,
        max: usize,
        limit: Instant,
        policy: EntryReadPolicy,
    ) -> Result<Option<(Vec<u8>, PinnedFile)>> {
        deadline(limit)?;
        let mut file = match openat(&parent.file, name, READ_FLAGS, Mode::empty()) {
            Ok(file) => File::from(file),
            Err(Errno::ENOENT) => {
                ensure!(
                    parent.named(name)?.is_none(),
                    "project lock missing entry changed"
                );
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let observed = Stamp::file(&file)?;
        match policy {
            EntryReadPolicy::LockTarget => observed.regular()?,
            EntryReadPolicy::ReadOnly => ensure!(
                observed.kind() == SFlag::S_IFREG && observed.size >= 0,
                "readonly project file must be a regular entry"
            ),
        }
        let size = usize::try_from(observed.size).map_err(|_| ReadEntryFailure::Bytes)?;
        ensure!(size <= max, ReadEntryFailure::Bytes);
        ensure!(
            parent.named(name)? == Some(observed),
            ReadEntryFailure::Changed
        );
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| eyre::eyre!("project lock bounded allocation failed"))?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            deadline(limit)?;
            let request = buffer.len().min(max - bytes.len() + 1);
            let count = match file.read(&mut buffer[..request]) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                value => value?,
            };
            if count == 0 {
                break;
            }
            let total = bytes
                .len()
                .checked_add(count)
                .filter(|n| *n <= max)
                .ok_or(ReadEntryFailure::Bytes)?;
            bytes
                .try_reserve(total - bytes.len())
                .map_err(|_| eyre::eyre!("project lock bounded allocation failed"))?;
            bytes.extend_from_slice(&buffer[..count]);
        }
        ensure!(
            bytes.len() == size
                && Stamp::file(&file)? == observed
                && parent.named(name)? == Some(observed),
            ReadEntryFailure::Changed
        );
        Ok(Some((bytes, PinnedFile { file, observed })))
    }

    impl SessionIo {
        fn check(&self) -> Result<()> {
            ensure!(
                !self.failed,
                "project lock session failed after a write; acquire a new session"
            );
            deadline(self.deadline)?;
            self.domain.check(self.deadline)?;
            if let Some(alias) = &self.alias {
                alias.check()?;
            }
            Ok(())
        }
        pub(super) fn snapshot(&self) -> Result<LockImage> {
            self.check()?;
            let current = read_entry(
                &self.domain.parent,
                &self.name,
                self.max_bytes,
                self.deadline,
            )?;
            self.check()?;
            let (bytes, pin, sha256) = match current {
                Some((bytes, pin)) => {
                    let hash = observed_sha256(&bytes, self.deadline)?;
                    (Some(bytes), Some(pin), Some(hash))
                }
                None => (None, None, None),
            };
            Ok(LockImage {
                bytes,
                sha256,
                pin,
                session: Arc::clone(&self.token),
            })
        }
        pub(super) fn recheck(&self, expected: &LockImage) -> Result<()> {
            self.check()?;
            ensure!(
                Arc::ptr_eq(&self.token, &expected.session),
                "project lock image belongs to another session"
            );
            if let Some(pin) = &expected.pin {
                ensure!(
                    Stamp::file(&pin.file)? == pin.observed,
                    "project lock original image observation changed"
                );
            }
            let current = self.snapshot()?;
            ensure!(
                expected.bytes == current.bytes
                    && expected.sha256 == current.sha256
                    && expected.pin.as_ref().map(|p| p.observed)
                        == current.pin.as_ref().map(|p| p.observed),
                "project lock image is stale"
            );
            self.check()
        }
        pub(super) fn check_binding(&self) -> Result<()> {
            self.check()
        }
        pub(super) fn replace(
            &mut self,
            expected: &LockImage,
            bytes: &[u8],
            hook: impl FnOnce(&Path),
        ) -> Result<LockImage> {
            self.replace_journaled(expected, bytes, |path, _| {
                hook(path);
                Ok(())
            })
        }
        pub(super) fn replace_journaled(
            &mut self,
            expected: &LockImage,
            bytes: &[u8],
            hook: impl FnOnce(&Path, (u64, u64)) -> Result<()>,
        ) -> Result<LockImage> {
            ensure!(
                bytes.len() <= self.max_bytes,
                "project lock replacement byte limit exceeded"
            );
            self.recheck(expected)?;
            let mode = expected.pin.as_ref().map(|pin| pin.observed.mode & 0o777);
            let gate = || self.recheck(expected);
            let result = guarded_write(
                &self.domain,
                &self.name,
                mode,
                bytes,
                self.max_bytes,
                self.deadline,
                Some(&gate as &dyn Fn() -> Result<()>),
                InstallWrite::No,
                |path| {
                    let metadata = std::fs::symlink_metadata(path)?;
                    use std::os::unix::fs::MetadataExt;
                    hook(path, (metadata.dev(), metadata.ino()))
                },
            )
            .and_then(|(bytes, pin)| {
                // The published inode and bytes come from the guarded write
                // itself. A later observation may only recheck that owner;
                // it must not define a new baseline after the side effect.
                let image = LockImage {
                    sha256: Some(observed_sha256(&bytes, self.deadline)?),
                    bytes: Some(bytes),
                    pin: Some(pin),
                    session: Arc::clone(&self.token),
                };
                self.recheck(&image)
                    .context("project lock replacement may be committed; owned image changed")?;
                Ok(image)
            });
            if result.is_err() {
                self.failed = true;
            }
            result
        }
        pub(super) fn replace_journaled_install(
            &mut self,
            expected: &LockImage,
            bytes: &[u8],
            hook: impl FnOnce(&Path, (u64, u64)) -> Result<()>,
        ) -> Result<LockImage> {
            ensure!(
                bytes.len() <= self.max_bytes,
                "project lock replacement byte limit exceeded"
            );
            // Real session preconditions, without comparing the observed
            // entry's on-disk content: a failed session cannot write again,
            // and the backup must belong to this session.
            self.check()?;
            ensure!(
                Arc::ptr_eq(&self.token, &expected.session),
                "project lock image belongs to another session"
            );
            let mode = expected.pin.as_ref().map(|pin| pin.observed.mode & 0o777);
            let result = guarded_write(
                &self.domain,
                &self.name,
                mode,
                bytes,
                self.max_bytes,
                self.deadline,
                None,
                InstallWrite::Yes,
                |path| {
                    let metadata = std::fs::symlink_metadata(path)?;
                    use std::os::unix::fs::MetadataExt;
                    hook(path, (metadata.dev(), metadata.ino()))
                },
            )
            .and_then(|(bytes, pin)| {
                let image = LockImage {
                    sha256: Some(observed_sha256(&bytes, self.deadline)?),
                    bytes: Some(bytes),
                    pin: Some(pin),
                    session: Arc::clone(&self.token),
                };
                Ok(image)
            });
            if result.is_err() {
                self.failed = true;
            }
            result
        }
        pub(super) fn remove(&mut self, expected: &LockImage) -> Result<LockImage> {
            self.recheck(expected)?;
            ensure!(
                expected.exists(),
                "cannot remove an absent project lock image"
            );
            unlinkat(
                &self.domain.parent.file,
                self.name.as_os_str(),
                UnlinkatFlags::NoRemoveDir,
            )?;
            self.domain.parent.file.sync_all()?;
            self.check()?;
            ensure!(
                self.domain.parent.named(&self.name)?.is_none(),
                "removed project lock entry reappeared"
            );
            let image = LockImage {
                bytes: None,
                sha256: None,
                pin: None,
                session: Arc::clone(&self.token),
            };
            self.recheck(&image)?;
            Ok(image)
        }
        pub(super) fn cleanup_staged(
            &self,
            path: &Path,
            identity: (u64, u64),
            bytes: &[u8],
        ) -> Result<()> {
            self.check()?;
            ensure!(
                path.parent() == Some(self.domain.parent.path.as_path()),
                "staged lock parent differs"
            );
            let name = path
                .file_name()
                .context("staged lock filename is missing")?;
            ensure!(
                name.to_str()
                    .is_some_and(|name| name.starts_with(".pakro.lock.txn-")),
                "invalid staged lock name"
            );
            let Some((current, pin)) =
                read_entry(&self.domain.parent, name, self.max_bytes, self.deadline)?
            else {
                return Ok(());
            };
            ensure!(
                (pin.observed.device as u64, pin.observed.inode as u64) == identity
                    && current == bytes,
                "staged lock identity or bytes changed; evidence retained"
            );
            self.check()?;
            ensure!(
                self.domain.parent.named(name)? == Some(pin.observed),
                "staged lock changed before cleanup"
            );
            unlinkat(&self.domain.parent.file, name, UnlinkatFlags::NoRemoveDir)?;
            self.domain.parent.file.sync_all()?;
            self.check()
        }
    }

    pub(super) fn validate_publication(bytes: &[u8], maximum: usize, limit: Instant) -> Result<()> {
        ensure!(
            maximum != 0 && maximum.checked_add(1).is_some(),
            "project lock byte limit is invalid"
        );
        ensure!(
            bytes.len() <= maximum,
            "project lock replacement byte limit exceeded"
        );
        deadline(limit)
    }

    pub(super) fn publish_dangling(
        pin: AliasPin,
        bytes: &[u8],
        maximum: usize,
        limit: Instant,
        hook: impl FnOnce(&Path),
    ) -> Result<()> {
        validate_publication(bytes, maximum, limit)?;
        pin.check_dangling()?;
        let domain = GuardDomain::acquire(pin.parent.duplicate()?, limit)?;
        pin.check_dangling()?;
        let gate = || pin.check_dangling();
        guarded_write(
            &domain,
            &pin.name,
            None,
            bytes,
            maximum,
            limit,
            Some(&gate as &dyn Fn() -> Result<()>),
            InstallWrite::No,
            |path| {
                hook(path);
                Ok(())
            },
        )
        .map(|_| ())
    }

    // Both writers need this actual exclusive domain. Their independent
    // predicates bind either a strict image or the native original alias; the
    // install publication passes no predicate and takes its write result from
    // the temporary it wrote, never from a re-read of the replaced entry.
    enum InstallWrite {
        No,
        Yes,
    }
    fn null_file() -> Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/null")
            .context("project lock write result handle is unavailable")
    }
    fn guarded_write(
        domain: &GuardDomain,
        name: &OsStr,
        mode: Option<nix::libc::mode_t>,
        bytes: &[u8],
        maximum: usize,
        limit: Instant,
        predicate: Option<&dyn Fn() -> Result<()>>,
        install: InstallWrite,
        hook: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<(Vec<u8>, PinnedFile)> {
        let run_predicate = || -> Result<()> {
            if let Some(predicate) = predicate {
                predicate()?;
            }
            Ok(())
        };
        validate_publication(bytes, maximum, limit)?;
        domain.check(limit)?;
        run_predicate()?;
        let parent = &domain.parent;
        let mut temporary = Temporary::create(parent)?;
        let result = (|| {
            deadline(limit)?;
            for block in bytes.chunks(64 * 1024) {
                deadline(limit)?;
                temporary.file.write_all(block)?;
            }
            if let Some(mode) = mode {
                fchmod(&temporary.file, Mode::from_bits_truncate(mode))?;
            }
            temporary.file.sync_all()?;
            let observed = Stamp::file(&temporary.file)?;
            hook(&parent.path.join(&temporary.name))?;
            run_predicate()?;
            domain.check(limit)?;
            let current = read_entry(parent, &temporary.name, maximum, limit)?
                .context("project lock temporary disappeared")?;
            ensure!(
                current.1.observed == observed
                    && Stamp::file(&temporary.file)? == observed
                    && current.0 == bytes,
                "project lock temporary binding or bytes changed"
            );
            run_predicate()?;
            domain.check(limit)?;
            deadline(limit)?;
            renameat(&parent.file, temporary.name.as_os_str(), &parent.file, name)?;
            temporary.active = false;
            parent
                .file
                .sync_all()
                .context("project lock replacement is visible but parent durability failed")?;
            domain.check(limit)?;
            let committed = match install {
                InstallWrite::Yes => {
                    // Test cutpoint at the exact instant after the install
                    // rename is durable but before this write returns its own
                    // result; compiled out of production builds.
                    install_write_renamed_cutpoint();
                    // The temporary this session wrote is the published entry
                    // after the rename; its own handle and stamp are the write
                    // result. The entry itself is not re-read.
                    let written = std::mem::replace(&mut temporary.file, null_file()?);
                    (
                        bytes.to_vec(),
                        PinnedFile {
                            file: written,
                            observed,
                        },
                    )
                }
                InstallWrite::No => {
                    let committed = read_entry(parent, name, maximum, limit)?.context(
                        "project lock replacement may be committed; current entry disappeared",
                    )?;
                    ensure!(
                        committed.0 == bytes
                            && committed.1.observed == Stamp::file(&temporary.file)?,
                        "project lock replacement may be committed; current entry changed"
                    );
                    domain.check(limit)?;
                    committed
                }
            };
            domain.check(limit)?;
            Ok(committed)
        })();
        if result.is_err() && temporary.cleanup().is_err() {
            return result
                .wrap_err("project lock write failed; temporary cleanup could not be proven");
        }
        result
    }

    struct Temporary {
        parent: File,
        name: OsString,
        file: File,
        active: bool,
    }
    impl Temporary {
        fn create(parent: &Parent) -> Result<Self> {
            let parent_pin = parent.file.try_clone()?;
            for _ in 0..8 {
                let name =
                    OsString::from(format!(".pakro.lock.txn-{}", uuid::Uuid::now_v7().simple()));
                match openat(
                    &parent.file,
                    name.as_os_str(),
                    OFlag::O_RDWR
                        | OFlag::O_CREAT
                        | OFlag::O_EXCL
                        | OFlag::O_NOFOLLOW
                        | OFlag::O_NONBLOCK
                        | OFlag::O_CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                ) {
                    Ok(file) => {
                        return Ok(Self {
                            parent: parent_pin,
                            name,
                            file: File::from(file),
                            active: true,
                        });
                    }
                    Err(Errno::EEXIST) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            eyre::bail!("project lock unique temporary creation failed")
        }
        fn cleanup(&mut self) -> Result<()> {
            if !self.active {
                return Ok(());
            }
            self.active = false;
            let held = Stamp::file(&self.file)?;
            let named = Stamp::of(
                fstatat(
                    &self.parent,
                    self.name.as_os_str(),
                    AtFlags::AT_SYMLINK_NOFOLLOW,
                )
                .context("project lock temporary cleanup entry disappeared; evidence retained")?,
            );
            ensure!(
                named.identity() == held.identity()
                    && named.kind() == SFlag::S_IFREG
                    && named.links == 1,
                "project lock temporary cleanup identity changed; evidence retained"
            );
            unlinkat(
                &self.parent,
                self.name.as_os_str(),
                UnlinkatFlags::NoRemoveDir,
            )?;
            self.parent.sync_all()?;
            Ok(())
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }
}

/// Tests inject an external replacement of the lock entry at the exact
/// instant after the install rename is durable but before the write returns
/// its own result. No environment-triggered cutpoint is compiled into
/// production.
#[cfg(all(test, unix))]
thread_local! {
    static INSTALL_WRITE_RENAMED_HOOK:
        std::cell::RefCell<Option<Box<dyn FnMut() + 'static>>> =
        const { std::cell::RefCell::new(None) };
}
#[cfg(all(test, unix))]
pub(crate) fn with_install_write_renamed_hook<T>(
    hook: impl FnMut() + 'static,
    body: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INSTALL_WRITE_RENAMED_HOOK.with(|hook| *hook.borrow_mut() = None);
        }
    }
    INSTALL_WRITE_RENAMED_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
    let _reset = Reset;
    body()
}
fn install_write_renamed_cutpoint() {
    #[cfg(all(test, unix))]
    INSTALL_WRITE_RENAMED_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
    #[cfg(not(all(test, unix)))]
    {}
}

#[cfg(all(test, unix))]
mod tests;
