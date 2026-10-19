//! Shared no-follow, owner/mode/link and identity checked Unix primitives.
use super::*;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use eyre::bail;
use eyre::{Context, ensure};
use nix::fcntl::{AtFlags, OFlag, open, openat};
use nix::sys::stat::{Mode, fchmod, fstatat, mkdirat};
use std::io;
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
pub fn open_directory<P: nix::NixPath + ?Sized>(parent: &File, name: &P) -> Result<File> {
    Ok(File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?))
}

#[cfg(unix)]
pub fn validate_ancestor(directory: &File, expected_uid: u32) -> Result<()> {
    let metadata = directory.metadata()?;
    ensure!(metadata.is_dir(), "environment ancestor is not a directory");
    ensure!(
        metadata.uid() == 0 || metadata.uid() == expected_uid,
        "environment ancestor has an untrusted owner"
    );
    ensure!(
        metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0,
        "environment ancestor is writable without sticky protection"
    );
    Ok(())
}

#[cfg(unix)]
pub fn validate_owned_directory(directory: &File, expected_uid: u32) -> Result<()> {
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.uid() == expected_uid && metadata.mode() & 0o022 == 0,
        "environment directory has unsafe type, owner or permissions"
    );
    Ok(())
}

#[cfg(unix)]
pub fn open_trusted_store(root: &Path, expected_uid: u32, create: bool) -> Result<File> {
    open_trusted_store_with_hook(root, expected_uid, create, None, |_| Ok(()))
}

pub fn open_trusted_store_with_deadline(
    root: &Path,
    expected_uid: u32,
    create: bool,
    deadline: Instant,
) -> Result<File> {
    open_trusted_store_with_hook(root, expected_uid, create, Some(deadline), |_| Ok(()))
}

pub(super) fn open_trusted_store_with_hook(
    root: &Path,
    expected_uid: u32,
    create: bool,
    deadline: Option<Instant>,
    mut before_component: impl FnMut(&std::ffi::OsStr) -> Result<()>,
) -> Result<File> {
    if let Some(deadline) = deadline {
        check_deadline(deadline)?;
    }
    use std::path::Component;
    let mut directory = File::from(open(
        Path::new("/"),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    for component in root.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        before_component(name)?;
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        validate_ancestor(&directory, expected_uid)?;
        directory = match open_directory(&directory, name) {
            Ok(opened) => opened,
            Err(error)
                if create
                    && error.downcast_ref::<nix::errno::Errno>()
                        == Some(&nix::errno::Errno::ENOENT) =>
            {
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                match mkdirat(&directory, name, Mode::from_bits_truncate(0o700)) {
                    Ok(()) => {
                        let opened = open_directory(&directory, name)?;
                        fchmod(&opened, Mode::from_bits_truncate(0o700))?;
                        validate_owned_directory(&opened, expected_uid)?;
                        opened.sync_all()?;
                        directory.sync_all()?;
                        opened
                    }
                    Err(nix::errno::Errno::EEXIST) => open_directory(&directory, name)?,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => {
                return Err(error)
                    .wrap_err("environment root cannot be opened without following links");
            }
        };
    }
    validate_owned_directory(&directory, expected_uid)?;
    if let Some(deadline) = deadline {
        check_deadline(deadline)?;
    }
    Ok(directory)
}

#[cfg(unix)]
pub fn open_private_directory(parent: &File, name: &str, expected_uid: u32) -> Result<File> {
    let created = match mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
        Ok(()) => true,
        Err(nix::errno::Errno::EEXIST) => false,
        Err(error) => return Err(error.into()),
    };
    let directory = open_directory(parent, name)?;
    if created {
        fchmod(&directory, Mode::from_bits_truncate(0o700))?;
    }
    validate_owned_directory(&directory, expected_uid)?;
    ensure!(
        directory.metadata()?.mode() & 0o077 == 0,
        "environment control directory is not private"
    );
    Ok(directory)
}

#[cfg(unix)]
pub fn validate_owned_regular(file: &File, expected_uid: u32) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == expected_uid
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1,
        "environment lock has unsafe type, owner, links or permissions"
    );
    Ok(())
}

#[cfg(unix)]
pub fn ensure_entry_identity(parent: &File, name: &str, identity: FileIdentity) -> Result<()> {
    let observed = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    ensure!(
        observed.st_dev == identity.device && observed.st_ino == identity.inode,
        "environment entry identity changed"
    );
    Ok(())
}
#[cfg(unix)]
pub fn rename_noclobber(
    source_dir: &impl std::os::fd::AsFd,
    source_name: &std::ffi::OsStr,
    target_dir: &impl std::os::fd::AsFd,
    target_name: &std::ffi::OsStr,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source_name.as_bytes())?;
    let target = std::ffi::CString::new(target_name.as_bytes())?;
    #[cfg(target_os = "macos")]
    // SAFETY: both dirfds remain borrowed for the call; both names are live,
    // NUL-terminated CStrings. RENAME_EXCL never replaces an existing entry.
    let result = unsafe {
        nix::libc::renameatx_np(
            source_dir.as_fd().as_raw_fd(),
            source.as_ptr(),
            target_dir.as_fd().as_raw_fd(),
            target.as_ptr(),
            nix::libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: valid borrowed dirfds and live CStrings, as above. Direct syscall
    // also supports musl without relying on a glibc-only renameat2 wrapper.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            source_dir.as_fd().as_raw_fd(),
            source.as_ptr(),
            target_dir.as_fd().as_raw_fd(),
            target.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if result == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    bail!("no-clobber environment rename is unsupported on this platform")
}

/// Open an existing private namespace without creating it.
pub fn open_existing_private_directory(parent: &File, name: &str, uid: u32) -> Result<File> {
    let directory = open_directory(parent, name)?;
    validate_owned_directory(&directory, uid)?;
    ensure!(
        directory.metadata()?.mode() & 0o7777 == 0o700,
        "environment control directory is not private"
    );
    ensure_entry_identity(parent, name, FileIdentity::of(&directory)?)?;
    Ok(directory)
}

/// Open an existing stable lock or metadata file for mutation. Missing entries
/// are errors; this function never creates a file.
pub fn open_existing_owned_regular(parent: &File, name: &str, uid: u32) -> Result<File> {
    open_existing_regular(parent, name, uid, OFlag::O_RDWR)
}

/// Open existing metadata or a shared protection inode without write access.
pub fn open_existing_readonly_regular(parent: &File, name: &str, uid: u32) -> Result<File> {
    open_existing_regular(parent, name, uid, OFlag::O_RDONLY)
}

fn open_existing_regular(parent: &File, name: &str, uid: u32, access: OFlag) -> Result<File> {
    let file = File::from(openat(
        parent,
        name,
        access | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    validate_owned_regular(&file, uid)?;
    ensure_entry_identity(parent, name, FileIdentity::of(&file)?)?;
    Ok(file)
}
