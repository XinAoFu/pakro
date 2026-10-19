//! Current readonly bytes, distinct from a guarded lock image or write session.

use std::fmt;
use std::path::Path;
use std::time::Instant;
use thiserror::Error;

pub(crate) type ReadOnlyFileResult<T> = Result<T, ReadOnlyFileError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadOnlyFileLimit {
    Bytes,
    PathBytes,
    Components,
    Symlinks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ReadOnlyFileError {
    #[cfg(not(unix))]
    #[error("readonly file observation is unavailable on this platform")]
    Unsupported,
    #[error("readonly file limits are invalid")]
    InvalidLimits,
    #[error("readonly file observation budget exceeded: {0:?}")]
    Limit(ReadOnlyFileLimit),
    #[error("readonly file is unavailable or has an unsupported entry type")]
    Unavailable,
    #[error("readonly file observation changed")]
    Changed,
    #[error("readonly file observation deadline expired")]
    Expired,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadOnlyFileLimits {
    pub max_bytes: usize,
    pub max_path_bytes: usize,
    pub max_components: usize,
    pub max_symlink_hops: usize,
}
impl Default for ReadOnlyFileLimits {
    fn default() -> Self {
        Self {
            max_bytes: 4 * 1024 * 1024,
            max_path_bytes: 16 * 1024,
            max_components: 1024,
            max_symlink_hops: 64,
        }
    }
}

pub(crate) struct ReadOnlyFileObservation {
    bytes: Vec<u8>,
    sha256: [u8; 32],
    #[cfg(unix)]
    io: super::unix::read_only::ReadOnlyIo,
}
impl ReadOnlyFileObservation {
    pub(crate) fn open(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
    ) -> ReadOnlyFileResult<Self> {
        #[cfg(unix)]
        {
            let captured = super::unix::read_only::capture(path, limits, deadline)?;
            Ok(Self {
                bytes: captured.bytes,
                sha256: captured.sha256,
                io: captured.io,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, limits, deadline);
            Err(ReadOnlyFileError::Unsupported)
        }
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn recheck(&self) -> ReadOnlyFileResult<()> {
        #[cfg(unix)]
        return self.io.recheck(&self.bytes, &self.sha256);
        #[cfg(not(unix))]
        Err(ReadOnlyFileError::Unsupported)
    }

    #[cfg(all(test, unix))]
    fn open_with_hook(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
        hook: impl FnMut(),
    ) -> ReadOnlyFileResult<Self> {
        let captured = super::unix::read_only::capture_with_hook(path, limits, deadline, hook)?;
        Ok(Self {
            bytes: captured.bytes,
            sha256: captured.sha256,
            io: captured.io,
        })
    }
}
impl fmt::Debug for ReadOnlyFileObservation {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ReadOnlyFileObservation")
            .field("size", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// A namespace observation for a directory or a missing declaration candidate.
/// It grants no file-content or publication authority.
pub(crate) struct ReadOnlyPathObservation {
    exists: bool,
    #[cfg(unix)]
    io: super::unix::read_only::ReadOnlyPathIo,
}

impl ReadOnlyPathObservation {
    pub(crate) fn open(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
    ) -> ReadOnlyFileResult<Self> {
        #[cfg(unix)]
        {
            let io = super::unix::read_only::ReadOnlyPathIo::open(path, limits, deadline)?;
            Ok(Self {
                exists: io.exists(),
                io,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, limits, deadline);
            Err(ReadOnlyFileError::Unsupported)
        }
    }
    pub(crate) fn exists(&self) -> bool {
        self.exists
    }
    pub(crate) fn recheck(&self) -> ReadOnlyFileResult<()> {
        #[cfg(unix)]
        return self.io.recheck();
        #[cfg(not(unix))]
        Err(ReadOnlyFileError::Unsupported)
    }
}

impl fmt::Debug for ReadOnlyPathObservation {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ReadOnlyPathObservation")
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, unix))]
mod path_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    fn observe(path: &Path) -> ReadOnlyPathObservation {
        ReadOnlyPathObservation::open(
            path,
            Default::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap()
    }

    #[test]
    fn r03_directory_alias_replacement_revokes_namespace() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("actual")).unwrap();
        let alias = root.path().join("project");
        symlink("actual", &alias).unwrap();
        let observed = observe(&alias);
        assert!(observed.exists());
        observed.recheck().unwrap();
        symlink("actual", root.path().join("replacement")).unwrap();
        fs::rename(root.path().join("replacement"), &alias).unwrap();
        assert_eq!(observed.recheck(), Err(ReadOnlyFileError::Changed));
    }

    #[test]
    fn r03_missing_candidate_parent_alias_replacement_revokes_namespace() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("actual")).unwrap();
        let alias = root.path().join("config");
        symlink("actual", &alias).unwrap();
        let observed = observe(&alias.join("pakro.local.toml"));
        assert!(!observed.exists());
        observed.recheck().unwrap();
        symlink("actual", root.path().join("replacement")).unwrap();
        fs::rename(root.path().join("replacement"), &alias).unwrap();
        assert_eq!(observed.recheck(), Err(ReadOnlyFileError::Changed));
    }

    #[test]
    fn r03_first_missing_ancestor_is_retained_without_creating_directories() {
        let root = tempfile::tempdir().unwrap();
        let observed = observe(&root.path().join("missing/nested/pakro.toml"));
        assert!(!observed.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        fs::write(root.path().join("unrelated"), b"unrelated").unwrap();
        observed.recheck().unwrap();
        fs::create_dir(root.path().join("missing")).unwrap();
        assert_eq!(observed.recheck(), Err(ReadOnlyFileError::Changed));
    }

    #[test]
    fn r03_directory_replacement_revokes_namespace_without_using_mtime() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("project");
        fs::create_dir(&path).unwrap();
        let observed = observe(&path);
        fs::write(path.join("unrelated"), b"unrelated").unwrap();
        observed.recheck().unwrap();
        fs::rename(&path, root.path().join("retained")).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(observed.recheck(), Err(ReadOnlyFileError::Changed));
    }

    #[test]
    fn r03_namespace_rejects_fifo_and_enforces_limits() {
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("pipe");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(ReadOnlyPathObservation::open(&fifo, Default::default(), deadline).is_err());
        assert!(matches!(
            ReadOnlyPathObservation::open(root.path(), Default::default(), Instant::now()),
            Err(ReadOnlyFileError::Expired)
        ));
        let limits = ReadOnlyFileLimits {
            max_components: 1,
            ..Default::default()
        };
        assert!(matches!(
            ReadOnlyPathObservation::open(root.path(), limits, deadline),
            Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Components))
        ));
        let alias = root.path().join("cycle");
        symlink("cycle", &alias).unwrap();
        let limits = ReadOnlyFileLimits {
            max_symlink_hops: 2,
            ..Default::default()
        };
        assert!(matches!(
            ReadOnlyPathObservation::open(&alias, limits, deadline),
            Err(ReadOnlyFileError::Limit(ReadOnlyFileLimit::Symlinks))
        ));
    }
}

#[cfg(all(test, unix))]
#[path = "observation/tests.rs"]
mod tests;
