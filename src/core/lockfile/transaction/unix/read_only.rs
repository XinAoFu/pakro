//! Readonly namespace observation, sharing the enclosing private Unix IO kernel.

use super::super::observation::{
    ReadOnlyFileError, ReadOnlyFileLimit, ReadOnlyFileLimits, ReadOnlyFileResult,
};
use super::*;
use std::collections::VecDeque;
use std::os::unix::ffi::OsStrExt;
use std::path::Component;

pub(crate) struct CapturedReadOnlyFile {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: [u8; 32],
    pub(crate) io: ReadOnlyIo,
}

pub(crate) struct ReadOnlyIo {
    namespace: Namespace,
    pin: PinnedFile,
    limits: ReadOnlyFileLimits,
    deadline: Instant,
}
impl ReadOnlyIo {
    pub(crate) fn recheck(&self, bytes: &[u8], sha256: &[u8; 32]) -> ReadOnlyFileResult<()> {
        clock(self.deadline)?;
        self.namespace.check(self.deadline)?;
        unchanged_pin(&self.pin)?;
        let (current, pin) = read_entry_with_policy(
            &self.namespace.parent,
            &self.namespace.name,
            self.limits.max_bytes,
            self.deadline,
            EntryReadPolicy::ReadOnly,
        )
        .map_err(|error| read_error(error, self.deadline))?
        .ok_or(ReadOnlyFileError::Changed)?;
        if pin.observed != self.pin.observed || current != bytes {
            return Err(ReadOnlyFileError::Changed);
        }
        let digest = observed_sha256(&current, self.deadline)
            .map_err(|error| read_error(error, self.deadline))?;
        if &digest != sha256 {
            return Err(ReadOnlyFileError::Changed);
        }
        unchanged_pin(&self.pin)?;
        self.namespace.check(self.deadline)?;
        clock(self.deadline)
    }
}

pub(crate) struct ReadOnlyPathIo {
    namespace: Namespace,
    deadline: Instant,
}
impl ReadOnlyPathIo {
    pub(crate) fn open(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
    ) -> ReadOnlyFileResult<Self> {
        validate_limits(limits)?;
        clock(deadline)?;
        let namespace =
            Namespace::resolve(path, limits, deadline, NamespacePurpose::DirectoryOrMissing)?;
        namespace.check(deadline)?;
        Ok(Self {
            namespace,
            deadline,
        })
    }
    pub(crate) fn exists(&self) -> bool {
        !matches!(self.namespace.terminal, NamespaceTerminal::Missing)
    }
    pub(crate) fn recheck(&self) -> ReadOnlyFileResult<()> {
        self.namespace.check(self.deadline)
    }
}

fn unchanged_pin(pin: &PinnedFile) -> ReadOnlyFileResult<()> {
    if Stamp::file(&pin.file).map_err(|_| ReadOnlyFileError::Changed)? != pin.observed {
        return Err(ReadOnlyFileError::Changed);
    }
    Ok(())
}
fn clock(deadline: Instant) -> ReadOnlyFileResult<()> {
    if Instant::now() >= deadline {
        return Err(ReadOnlyFileError::Expired);
    }
    Ok(())
}
fn read_error(error: eyre::Report, deadline: Instant) -> ReadOnlyFileError {
    if Instant::now() >= deadline {
        return ReadOnlyFileError::Expired;
    }
    match error.downcast_ref::<ReadEntryFailure>() {
        Some(ReadEntryFailure::Bytes) => ReadOnlyFileError::Limit(ReadOnlyFileLimit::Bytes),
        Some(ReadEntryFailure::Changed) => ReadOnlyFileError::Changed,
        None => ReadOnlyFileError::Unavailable,
    }
}

pub(crate) fn capture(
    path: &Path,
    limits: ReadOnlyFileLimits,
    deadline: Instant,
) -> ReadOnlyFileResult<CapturedReadOnlyFile> {
    capture_with_hook(path, limits, deadline, || {})
}
pub(crate) fn capture_with_hook(
    path: &Path,
    limits: ReadOnlyFileLimits,
    deadline: Instant,
    mut after_bytes: impl FnMut(),
) -> ReadOnlyFileResult<CapturedReadOnlyFile> {
    validate_limits(limits)?;
    clock(deadline)?;
    let namespace = Namespace::resolve(path, limits, deadline, NamespacePurpose::RegularFile)?;
    namespace.check(deadline)?;
    let (bytes, pin) = read_entry_with_policy(
        &namespace.parent,
        &namespace.name,
        limits.max_bytes,
        deadline,
        EntryReadPolicy::ReadOnly,
    )
    .map_err(|error| read_error(error, deadline))?
    .ok_or(ReadOnlyFileError::Changed)?;
    after_bytes();
    unchanged_pin(&pin)?;
    namespace.check(deadline)?;
    let sha256 = observed_sha256(&bytes, deadline).map_err(|error| read_error(error, deadline))?;
    unchanged_pin(&pin)?;
    namespace.check(deadline)?;
    clock(deadline)?;
    Ok(CapturedReadOnlyFile {
        bytes,
        sha256,
        io: ReadOnlyIo {
            namespace,
            pin,
            limits,
            deadline,
        },
    })
}

fn validate_limits(limits: ReadOnlyFileLimits) -> ReadOnlyFileResult<()> {
    if limits.max_bytes == 0
        || limits.max_bytes.checked_add(1).is_none()
        || limits.max_path_bytes == 0
        || limits.max_path_bytes.checked_add(1).is_none()
        || limits.max_components == 0
        || limits.max_symlink_hops == 0
    {
        return Err(ReadOnlyFileError::InvalidLimits);
    }
    Ok(())
}

// Directory mtimes are intentionally not a current-config condition: a future
// cooperating lock may create a sibling guard. Named type/identity and held
// directory identity still bind each traversed component.
struct DirectoryEdge {
    parent: File,
    name: OsString,
    file: File,
    observed: Stamp,
}
impl DirectoryEdge {
    fn check(&self) -> ReadOnlyFileResult<()> {
        let held = Stamp::file(&self.file).map_err(|_| ReadOnlyFileError::Changed)?;
        let named = Stamp::of(
            fstatat(
                &self.parent,
                self.name.as_os_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|_| ReadOnlyFileError::Changed)?,
        );
        if held.kind() != SFlag::S_IFDIR
            || named.kind() != SFlag::S_IFDIR
            || held.identity() != self.observed.identity()
            || named.identity() != self.observed.identity()
        {
            return Err(ReadOnlyFileError::Changed);
        }
        Ok(())
    }
}
struct Namespace {
    root: File,
    root_stamp: Stamp,
    directories: Vec<DirectoryEdge>,
    aliases: Vec<AliasPin>,
    parent: Parent,
    name: OsString,
    terminal: NamespaceTerminal,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum NamespacePurpose {
    RegularFile,
    DirectoryOrMissing,
}
enum NamespaceTerminal {
    RegularFile,
    Directory,
    Missing,
}
enum Step {
    Root,
    Parent,
    Name(OsString),
    DirectoryBoundary,
}
fn steps(path: &Path, maximum: usize) -> ReadOnlyFileResult<VecDeque<Step>> {
    let raw = path.as_os_str().as_bytes();
    let boundary = raw.ends_with(b"/") || raw.ends_with(b"/.") || raw == b".";
    if path
        .components()
        .count()
        .checked_add(usize::from(boundary))
        .is_none_or(|count| count > maximum)
    {
        return Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Components));
    }
    let mut result = VecDeque::new();
    for component in path.components() {
        let step = match component {
            Component::RootDir => Step::Root,
            Component::ParentDir => Step::Parent,
            Component::Normal(name) => Step::Name(name.to_owned()),
            Component::CurDir => continue,
            Component::Prefix(_) => return Err(ReadOnlyFileError::Unavailable),
        };
        result
            .try_reserve(1)
            .map_err(|_| ReadOnlyFileError::Unavailable)?;
        result.push_back(step);
    }
    if boundary {
        result
            .try_reserve(1)
            .map_err(|_| ReadOnlyFileError::Unavailable)?;
        result.push_back(Step::DirectoryBoundary);
    }
    Ok(result)
}
fn root_parent(root: &File, observed: Stamp) -> ReadOnlyFileResult<Parent> {
    Ok(Parent {
        path: PathBuf::from("/"),
        file: root
            .try_clone()
            .map_err(|_| ReadOnlyFileError::Unavailable)?,
        observed,
    })
}
impl Namespace {
    fn resolve(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
        purpose: NamespacePurpose,
    ) -> ReadOnlyFileResult<Self> {
        if path.as_os_str().is_empty() {
            return Err(ReadOnlyFileError::Unavailable);
        }
        if path.as_os_str().as_bytes().len() > limits.max_path_bytes {
            return Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::PathBytes));
        }
        let absolute = std::path::absolute(path).map_err(|_| ReadOnlyFileError::Unavailable)?;
        let mut path_bytes = absolute.as_os_str().as_bytes().len();
        if path_bytes > limits.max_path_bytes {
            return Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::PathBytes));
        }
        let mut pending = steps(&absolute, limits.max_components)?;
        let root = File::from(
            open(
                Path::new("/"),
                READ_FLAGS | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| ReadOnlyFileError::Unavailable)?,
        );
        let root_stamp = Stamp::file(&root).map_err(|_| ReadOnlyFileError::Unavailable)?;
        let mut parent = root_parent(&root, root_stamp)?;
        let mut directories = Vec::new();
        let mut aliases = Vec::new();
        let mut work = 0usize;
        while let Some(step) = pending.pop_front() {
            clock(deadline)?;
            work = work
                .checked_add(1)
                .filter(|value| *value <= limits.max_components)
                .ok_or(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Components))?;
            let name = match step {
                Step::Root => {
                    parent = root_parent(&root, root_stamp)?;
                    continue;
                }
                Step::DirectoryBoundary => continue,
                Step::Parent => OsString::from(".."),
                Step::Name(name) => name,
            };
            let observed = parent
                .named(&name)
                .map_err(|_| ReadOnlyFileError::Unavailable)?;
            let Some(observed) = observed else {
                if purpose == NamespacePurpose::RegularFile {
                    return Err(ReadOnlyFileError::Unavailable);
                }
                // A missing ancestor binds the first absent entry. Its creation
                // revokes the observation even when the final candidate is absent.
                let result = Self {
                    root,
                    root_stamp,
                    directories,
                    aliases,
                    parent,
                    name,
                    terminal: NamespaceTerminal::Missing,
                };
                result.check(deadline)?;
                return Ok(result);
            };
            if observed.kind() == SFlag::S_IFLNK {
                if aliases.len() >= limits.max_symlink_hops {
                    return Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Symlinks));
                }
                let alias = AliasPin::open_with_policy(
                    parent.duplicate().map_err(|_| ReadOnlyFileError::Changed)?,
                    name,
                    false,
                    Some(limits.max_path_bytes - path_bytes),
                )
                .map_err(|error| {
                    if error.downcast_ref::<AliasReadFailure>().is_some() {
                        ReadOnlyFileError::Limit(ReadOnlyFileLimit::PathBytes)
                    } else {
                        ReadOnlyFileError::Changed
                    }
                })?;
                path_bytes = path_bytes
                    .checked_add(alias.link.as_os_str().as_bytes().len())
                    .filter(|value| *value <= limits.max_path_bytes)
                    .ok_or(ReadOnlyFileError::Limit(ReadOnlyFileLimit::PathBytes))?;
                let remaining = limits
                    .max_components
                    .checked_sub(work)
                    .and_then(|value| value.checked_sub(pending.len()))
                    .ok_or(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Components))?;
                let mut target = steps(Path::new(&alias.link), remaining)?;
                target.append(&mut pending);
                pending = target;
                aliases
                    .try_reserve(1)
                    .map_err(|_| ReadOnlyFileError::Unavailable)?;
                aliases.push(alias);
                continue;
            }
            if observed.kind() == SFlag::S_IFDIR
                && (!pending.is_empty() || purpose == NamespacePurpose::DirectoryOrMissing)
            {
                let file = File::from(
                    openat(
                        &parent.file,
                        name.as_os_str(),
                        READ_FLAGS | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    )
                    .map_err(|_| ReadOnlyFileError::Changed)?,
                );
                if Stamp::file(&file).map_err(|_| ReadOnlyFileError::Changed)? != observed
                    || parent
                        .named(&name)
                        .map_err(|_| ReadOnlyFileError::Changed)?
                        != Some(observed)
                {
                    return Err(ReadOnlyFileError::Changed);
                }
                let held = file
                    .try_clone()
                    .map_err(|_| ReadOnlyFileError::Unavailable)?;
                directories
                    .try_reserve(1)
                    .map_err(|_| ReadOnlyFileError::Unavailable)?;
                directories.push(DirectoryEdge {
                    parent: parent
                        .file
                        .try_clone()
                        .map_err(|_| ReadOnlyFileError::Unavailable)?,
                    name: name.clone(),
                    file: held,
                    observed,
                });
                if name == OsStr::new("..") {
                    parent.path.pop();
                } else {
                    parent.path.push(&name);
                }
                parent.file = file;
                parent.observed = observed;
                continue;
            }
            if observed.kind() != SFlag::S_IFREG
                || !pending.is_empty()
                || purpose != NamespacePurpose::RegularFile
            {
                return Err(ReadOnlyFileError::Unavailable);
            }
            let result = Self {
                root,
                root_stamp,
                directories,
                aliases,
                parent,
                name,
                terminal: NamespaceTerminal::RegularFile,
            };
            result.check(deadline)?;
            clock(deadline)?;
            return Ok(result);
        }
        if purpose == NamespacePurpose::RegularFile {
            return Err(ReadOnlyFileError::Unavailable);
        }
        let result = Self {
            root,
            root_stamp,
            directories,
            aliases,
            parent,
            name: OsString::from("."),
            terminal: NamespaceTerminal::Directory,
        };
        result.check(deadline)?;
        Ok(result)
    }
    fn check(&self, deadline: Instant) -> ReadOnlyFileResult<()> {
        clock(deadline)?;
        let held = Stamp::file(&self.root).map_err(|_| ReadOnlyFileError::Changed)?;
        if held.kind() != SFlag::S_IFDIR || held.identity() != self.root_stamp.identity() {
            return Err(ReadOnlyFileError::Changed);
        }
        for directory in &self.directories {
            clock(deadline)?;
            directory.check()?;
        }
        for alias in &self.aliases {
            clock(deadline)?;
            alias.check().map_err(|_| ReadOnlyFileError::Changed)?;
        }
        if matches!(self.terminal, NamespaceTerminal::Missing)
            && self
                .parent
                .named(&self.name)
                .map_err(|_| ReadOnlyFileError::Changed)?
                .is_some()
        {
            return Err(ReadOnlyFileError::Changed);
        }
        clock(deadline)
    }
}
