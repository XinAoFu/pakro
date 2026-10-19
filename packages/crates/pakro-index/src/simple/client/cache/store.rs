//! Private cache filesystem operations, with regular no-follow/nonblocking reads.

use super::*;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

pub(super) struct Store {
    directory: PathBuf,
    _held: File,
}
impl Store {
    pub(super) fn open(root: &Path, create: bool) -> SimpleClientResult<Option<Self>> {
        #[cfg(not(unix))]
        {
            let _ = (root, create);
            return Err(SimpleClientError::InvalidCache(
                "private cache requires a supported Unix platform",
            ));
        }
        #[cfg(unix)]
        {
            if create {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(root)
                    .map_err(|_| SimpleClientError::Io)?;
            }
            let Some(_root) = private_directory(root)? else {
                return Ok(None);
            };
            let directory = root.join("simple-v1");
            if create {
                match fs::DirBuilder::new().mode(0o700).create(&directory) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(SimpleClientError::Io),
                }
            }
            let Some(held) = private_directory(&directory)? else {
                return Ok(None);
            };
            Ok(Some(Self {
                directory,
                _held: held,
            }))
        }
    }
    pub(super) fn read(&self, key: SlotKey) -> SimpleClientResult<Option<File>> {
        #[cfg(unix)]
        {
            let path = self.directory.join(format!("{}.page", key.hex()));
            let file = match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => {
                    return Err(SimpleClientError::InvalidCache(
                        "entry cannot be opened safely",
                    ));
                }
            };
            let metadata = file.metadata().map_err(|_| SimpleClientError::Io)?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o7777 != 0o600
            {
                return Err(SimpleClientError::InvalidCache(
                    "entry has unsafe type, ownership or permissions",
                ));
            }
            Ok(Some(file))
        }
        #[cfg(not(unix))]
        {
            let _ = key;
            Err(SimpleClientError::Io)
        }
    }
    pub(super) fn write(&self, key: SlotKey, header: &[u8], body: &[u8]) -> SimpleClientResult<()> {
        // Check an existing entry without following a substituted symlink. A
        // complete concurrent writer may replace this same logical cache key.
        self.read(key)?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.directory).map_err(|_| SimpleClientError::Io)?;
        #[cfg(unix)]
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| SimpleClientError::Io)?;
        file.write_all(MAGIC)
            .and_then(|()| file.write_all(&(header.len() as u64).to_be_bytes()))
            .and_then(|()| file.write_all(header))
            .and_then(|()| file.write_all(body))
            .and_then(|()| file.flush())
            .map_err(|_| SimpleClientError::Io)?;
        file.as_file()
            .sync_all()
            .map_err(|_| SimpleClientError::Io)?;
        // Atomic rename is the last fallible publication operation. This cache
        // promises complete entries, not crash-durable environment publication.
        file.persist(self.directory.join(format!("{}.page", key.hex())))
            .map_err(|_| SimpleClientError::Io)?;
        Ok(())
    }
    pub(super) fn remove(&self, key: SlotKey) -> SimpleClientResult<()> {
        if self.read(key)?.is_some() {
            fs::remove_file(self.directory.join(format!("{}.page", key.hex())))
                .map_err(|_| SimpleClientError::Io)?;
        }
        Ok(())
    }
}
#[cfg(unix)]
fn private_directory(path: &Path) -> SimpleClientResult<Option<File>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(SimpleClientError::InvalidCache(
                "private directory cannot be opened safely",
            ));
        }
    };
    let metadata = file.metadata().map_err(|_| SimpleClientError::Io)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(SimpleClientError::InvalidCache(
            "directory has unsafe type, ownership or permissions",
        ));
    }
    Ok(Some(file))
}
