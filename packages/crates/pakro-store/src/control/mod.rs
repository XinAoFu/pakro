//! Anchored IO shared by library readers, writers and lease holders.
//!
//! Readers never create control objects. Writers initialize an empty authorized
//! root once; existing roots must retain every recorded inode and stable lock.

use eyre::{Result, ensure};
use lease::{LeaseMode, ObjectLease, OrderedLocks, StableLock};
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::{Mode, fchmod, fstat, mkdirat};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub mod lease;
pub mod ownership;
mod read;
mod unix;
mod write;
pub use read::{ReadControl, ReadStore};
#[doc(hidden)]
pub use unix::*;
pub use write::{StoreWriter, WriterControl};

#[derive(Debug, Clone, Copy)]
pub struct RootLimits {
    pub max_components: usize,
    pub max_path_bytes: usize,
}
impl Default for RootLimits {
    fn default() -> Self {
        Self {
            max_components: 256,
            max_path_bytes: 16_384,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ControlLimits {
    pub max_file_bytes: u64,
    pub max_name_bytes: usize,
}
impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_name_bytes: 128,
        }
    }
}
impl ControlLimits {
    fn validate(self) -> Result<Self> {
        ensure!(
            self.max_file_bytes != 0
                && self.max_file_bytes.checked_add(1).is_some()
                && self.max_name_bytes != 0,
            "invalid environment control metadata limits"
        );
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlNamespace {
    Catalog,
    Journals,
    Seals,
    Ownership,
}
impl ControlNamespace {
    #[doc(hidden)]
    pub fn index(self) -> usize {
        match self {
            Self::Catalog => 0,
            Self::Journals => 1,
            Self::Seals => 2,
            Self::Ownership => 3,
        }
    }
    #[doc(hidden)]
    pub fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Journals => "journals",
            Self::Seals => "seals",
            Self::Ownership => "ownership",
        }
    }
}
const NAMESPACES: [ControlNamespace; 4] = [
    ControlNamespace::Catalog,
    ControlNamespace::Journals,
    ControlNamespace::Seals,
    ControlNamespace::Ownership,
];
const LOCK_NAME: &str = ".allocation.lock";
const MARKER_NAME: &str = ".identity";
const MARKER_MAGIC: &[u8; 8] = b"PKCTRL01";
const MARKER_LEN: usize = 8 + 9 * 16;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: nix::libc::dev_t,
    pub inode: nix::libc::ino_t,
}
impl FileIdentity {
    pub fn of(file: &File) -> Result<Self> {
        let value = fstat(file)?;
        Ok(Self {
            device: value.st_dev,
            inode: value.st_ino,
        })
    }
}

pub struct ControlSnapshot {
    namespace: ControlNamespace,
    name: String,
    bytes: Vec<u8>,
    sha256: [u8; 32],
    store_identity: FileIdentity,
    parent_identity: FileIdentity,
    entry_identity: FileIdentity,
    // Hold the inode through CAS to prevent identity reuse after replacement.
    _inode_pin: File,
}
impl ControlSnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}
impl std::fmt::Debug for ControlSnapshot {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("ControlSnapshot")
            .field("size", &self.bytes.len())
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Anchor {
    path: PathBuf,
    uid: u32,
    root: File,
    control: File,
    namespaces: [File; 4],
    leases: File,
    build: File,
    catalog: StableLock,
    marker: File,
    marker_bytes: [u8; MARKER_LEN],
}
impl Anchor {
    fn check_binding(&self, deadline: Instant) -> Result<()> {
        check_deadline(deadline)?;
        let current = open_trusted_store_with_deadline(&self.path, self.uid, false, deadline)?;
        ensure!(
            FileIdentity::of(&current)? == FileIdentity::of(&self.root)?,
            "environment store identity changed"
        );
        self.catalog.check_binding()?;
        validate_owned_directory(&self.root, self.uid)?;
        validate_private_directory(&self.control, self.uid)?;
        ensure_entry_identity(&self.root, ".control", FileIdentity::of(&self.control)?)?;
        for namespace in NAMESPACES {
            let directory = &self.namespaces[namespace.index()];
            validate_private_directory(directory, self.uid)?;
            ensure_entry_identity(
                &self.control,
                namespace.name(),
                FileIdentity::of(directory)?,
            )?;
        }
        validate_private_directory(&self.leases, self.uid)?;
        validate_private_directory(&self.build, self.uid)?;
        ensure_entry_identity(&self.root, ".leases", FileIdentity::of(&self.leases)?)?;
        ensure_entry_identity(&self.leases, "build", FileIdentity::of(&self.build)?)?;
        ensure_entry_identity(&self.control, MARKER_NAME, FileIdentity::of(&self.marker)?)?;
        ensure!(
            read_marker(&self.control, self.uid)?.1 == self.marker_bytes,
            "environment control identity marker changed"
        );
        check_deadline(deadline)
    }
}

fn validate_private_directory(file: &File, uid: u32) -> Result<()> {
    validate_owned_directory(file, uid)?;
    ensure!(
        file.metadata()?.mode() & 0o7777 == 0o700,
        "environment control directory is not private"
    );
    Ok(())
}
fn validate_metadata_file(file: &File, uid: u32) -> Result<()> {
    validate_owned_regular(file, uid)?;
    ensure!(
        file.metadata()?.mode() & 0o7777 == 0o600,
        "environment control metadata permissions are not private"
    );
    Ok(())
}
fn validate_name(name: &str, limits: ControlLimits) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= limits.max_name_bytes
            && !matches!(name, "." | "..")
            && !name.starts_with(".tmp-")
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "invalid environment control metadata name"
    );
    Ok(())
}
fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "store operation deadline exceeded"
    );
    Ok(())
}
fn authorized_root(
    root: &Path,
    uid: u32,
    limits: RootLimits,
    deadline: Instant,
) -> Result<PathBuf> {
    check_deadline(deadline)?;
    ensure!(
        uid == nix::unistd::geteuid().as_raw(),
        "environment store uid is not authorized for this process"
    );
    ensure!(
        limits.max_components != 0 && limits.max_path_bytes != 0,
        "invalid store root limits"
    );
    let bytes = root.as_os_str().as_encoded_bytes();
    ensure!(
        root.is_absolute() && !bytes.contains(&0),
        "store root must be an absolute path"
    );
    ensure!(
        !bytes
            .split(|byte| *byte == b'/')
            .any(|part| part == b"." || part == b".."),
        "store root has an unsafe path component"
    );
    ensure!(
        bytes.len() <= limits.max_path_bytes && root.components().count() <= limits.max_components,
        "store root exceeds path limits"
    );
    // Normalize only lexical separators. Never follow a symlink to authorize a root.
    Ok(root.components().collect())
}
fn is_missing(error: &eyre::Report) -> bool {
    error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::ENOENT)
}
fn directory_empty(directory: &File) -> Result<bool> {
    let mut listing = nix::dir::Dir::openat(
        directory,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    for entry in listing.iter() {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}
fn identity_bytes(
    root: &File,
    catalog: &StableLock,
    control: &File,
    namespaces: &[File; 4],
    leases: &File,
    build: &File,
) -> Result<[u8; MARKER_LEN]> {
    let identities = [
        FileIdentity::of(root)?,
        catalog.identity(),
        FileIdentity::of(control)?,
        FileIdentity::of(&namespaces[0])?,
        FileIdentity::of(&namespaces[1])?,
        FileIdentity::of(&namespaces[2])?,
        FileIdentity::of(&namespaces[3])?,
        FileIdentity::of(leases)?,
        FileIdentity::of(build)?,
    ];
    let mut bytes = [0; MARKER_LEN];
    bytes[..8].copy_from_slice(MARKER_MAGIC);
    for (index, identity) in identities.iter().enumerate() {
        let offset = 8 + index * 16;
        bytes[offset..offset + 8].copy_from_slice(&(identity.device as u64).to_be_bytes());
        bytes[offset + 8..offset + 16].copy_from_slice(&(identity.inode as u64).to_be_bytes());
    }
    Ok(bytes)
}
fn read_marker(control: &File, uid: u32) -> Result<(File, [u8; MARKER_LEN])> {
    let mut file = File::from(openat(
        control,
        MARKER_NAME,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    validate_metadata_file(&file, uid)?;
    let identity = FileIdentity::of(&file)?;
    ensure!(
        file.metadata()?.len() == MARKER_LEN as u64,
        "invalid environment control identity marker length"
    );
    let mut bytes = [0; MARKER_LEN];
    file.read_exact(&mut bytes)?;
    ensure!(
        file.metadata()?.len() == MARKER_LEN as u64 && &bytes[..8] == MARKER_MAGIC,
        "invalid environment control identity marker"
    );
    validate_metadata_file(&file, uid)?;
    ensure_entry_identity(control, MARKER_NAME, identity)?;
    Ok((file, bytes))
}
fn existing_anchor(path: PathBuf, root: File, uid: u32, deadline: Instant) -> Result<Anchor> {
    let file = open_existing_readonly_regular(&root, LOCK_NAME, uid)?;
    let catalog = StableLock::open_existing(&root, LOCK_NAME, uid, FileIdentity::of(&file)?)?;
    loop {
        let held = catalog.acquire(LeaseMode::Shared, deadline)?;
        // The creator publishes this immutable marker last. A reader can see
        // the new lock before the creator obtains EX; release SH so that the
        // creator can continue, and keep the caller's original deadline.
        let initialized = (|| -> Result<(File, File, [u8; MARKER_LEN])> {
            let control = open_existing_private_directory(&root, ".control", uid)?;
            let (marker, marker_bytes) = read_marker(&control, uid)?;
            Ok((control, marker, marker_bytes))
        })();
        let (control, marker, marker_bytes) = match initialized {
            Ok(value) => value,
            Err(error) if is_missing(&error) => {
                drop(held);
                check_deadline(deadline)?;
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(std::time::Duration::from_millis(10)),
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        // Once the marker exists, missing/replaced namespaces are corruption.
        // An opener never recreates these entries or adopts different inodes.
        let namespaces = [
            open_existing_private_directory(&control, "catalog", uid)?,
            open_existing_private_directory(&control, "journals", uid)?,
            open_existing_private_directory(&control, "seals", uid)?,
            open_existing_private_directory(&control, "ownership", uid)?,
        ];
        let leases = open_existing_private_directory(&root, ".leases", uid)?;
        let build = open_existing_private_directory(&leases, "build", uid)?;
        ensure!(
            marker_bytes
                == identity_bytes(&root, &catalog, &control, &namespaces, &leases, &build)?,
            "environment control identity marker does not match existing objects"
        );
        let anchor = Anchor {
            path,
            uid,
            root,
            control,
            namespaces,
            leases,
            build,
            catalog,
            marker,
            marker_bytes,
        };
        anchor.check_binding(deadline)?;
        check_deadline(deadline)?;
        return Ok(anchor);
    }
}

fn create_directory(parent: &File, name: &str, uid: u32, deadline: Instant) -> Result<File> {
    check_deadline(deadline)?;
    mkdirat(parent, name, Mode::from_bits_truncate(0o700))?;
    let directory = open_directory(parent, name)?;
    fchmod(&directory, Mode::from_bits_truncate(0o700))?;
    validate_private_directory(&directory, uid)?;
    ensure_entry_identity(parent, name, FileIdentity::of(&directory)?)?;
    directory.sync_all()?;
    parent.sync_all()?;
    Ok(directory)
}
fn initialize_anchor(path: PathBuf, root: File, uid: u32, deadline: Instant) -> Result<Anchor> {
    initialize_anchor_with_hook(path, root, uid, deadline, || Ok(()))
}

fn initialize_anchor_with_hook(
    path: PathBuf,
    root: File,
    uid: u32,
    deadline: Instant,
    after_lock_created: impl FnOnce() -> Result<()>,
) -> Result<Anchor> {
    check_deadline(deadline)?;
    // Only the creator of this one stable inode may initialize namespaces.
    let catalog = match StableLock::create_new(&root, LOCK_NAME, uid) {
        Ok(lock) => lock,
        Err(error)
            if error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::EEXIST) =>
        {
            return existing_anchor(path, root, uid, deadline);
        }
        Err(error) => return Err(error),
    };
    after_lock_created()?;
    let _held = catalog.acquire(LeaseMode::Exclusive, deadline)?;
    let control = create_directory(&root, ".control", uid, deadline)?;
    let namespaces = [
        create_directory(&control, "catalog", uid, deadline)?,
        create_directory(&control, "journals", uid, deadline)?,
        create_directory(&control, "seals", uid, deadline)?,
        create_directory(&control, "ownership", uid, deadline)?,
    ];
    let leases = create_directory(&root, ".leases", uid, deadline)?;
    let build = create_directory(&leases, "build", uid, deadline)?;
    let marker_bytes = identity_bytes(&root, &catalog, &control, &namespaces, &leases, &build)?;
    check_deadline(deadline)?;
    let mut marker = File::from(openat(
        &control,
        MARKER_NAME,
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    fchmod(&marker, Mode::from_bits_truncate(0o600))?;
    validate_metadata_file(&marker, uid)?;
    marker.write_all(&marker_bytes)?;
    marker.flush()?;
    marker.sync_all()?;
    control.sync_all()?;
    root.sync_all()?;
    let anchor = Anchor {
        path,
        uid,
        root,
        control,
        namespaces,
        leases,
        build,
        catalog,
        marker,
        marker_bytes,
    };
    anchor.check_binding(deadline)?;
    check_deadline(deadline)?;
    Ok(anchor)
}

#[cfg(test)]
mod tests;
