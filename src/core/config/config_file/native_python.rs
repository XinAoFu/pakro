//! One actual native configuration source, never the whole effective Config.
//!
//! Plain parsing, clones and proposed/dry-run configurations cannot construct
//! this owner. Its parsed value is private because PakroToml has mutating
//! methods through shared references.
//!
//! Opening retains the legacy parser's trust and logging behavior. The source
//! observation creates no lock guard; it is not a noninteractive query policy.

use super::pakro_toml::PakroToml;
use crate::lockfile::transaction::{
    ReadOnlyFileError, ReadOnlyFileLimits, ReadOnlyFileObservation,
};
use crate::packages::PackageInstallRequest;
use std::fmt;
use std::path::Path;
use std::time::Instant;
use thiserror::Error;

pub(crate) type NativeConfigResult<T> = Result<T, NativeConfigError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum NativeConfigError {
    #[error("native configuration observation is unavailable on this platform")]
    Unsupported,
    #[error("native configuration source observation failed: {0}")]
    Source(ReadOnlyFileError),
    #[error("native configuration path exceeds its input budget")]
    PathBudget,
    #[error("native configuration source is not valid UTF-8")]
    InvalidUtf8,
    #[error("native configuration source could not be parsed or trusted")]
    InvalidDocument,
    #[error("native configuration package declarations are invalid")]
    InvalidPackages,
    #[error("native configuration receipt belongs to another actual file owner")]
    WrongOwner,
}

pub(crate) struct NativePythonConfigFile {
    // No reference to this mutable legacy data is exposed outside the owner.
    parsed: PakroToml,
    source: ReadOnlyFileObservation,
}
impl NativePythonConfigFile {
    pub(crate) fn open(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
    ) -> NativeConfigResult<Self> {
        Self::open_with_finish(path, limits, deadline, || {})
    }
    fn open_with_finish(
        path: &Path,
        limits: ReadOnlyFileLimits,
        deadline: Instant,
        after_parse: impl FnOnce(),
    ) -> NativeConfigResult<Self> {
        if path.as_os_str().len() > limits.max_path_bytes {
            return Err(NativeConfigError::PathBudget);
        }
        if path.to_str().is_none() {
            return Err(NativeConfigError::InvalidDocument);
        }
        // Freeze a relative input once, keeping its alias spelling for both
        // the observation and the parser instead of consulting CWD twice.
        let path = std::path::absolute(path)
            .map_err(|_| NativeConfigError::Source(ReadOnlyFileError::Unavailable))?;
        let source = ReadOnlyFileObservation::open(&path, limits, deadline)
            .map_err(NativeConfigError::Source)?;
        // Legacy template/trust setup assumes UTF-8 paths. Canonicalization
        // here checks compatibility only: it never replaces the observed alias
        // or serves as evidence that the source is still current.
        let resolved = path
            .canonicalize()
            .map_err(|_| NativeConfigError::Source(ReadOnlyFileError::Changed))?;
        if path.to_str().is_none()
            || resolved.to_str().is_none()
            || super::config_root::config_root(&path).to_str().is_none()
        {
            return Err(NativeConfigError::InvalidDocument);
        }
        source.recheck().map_err(NativeConfigError::Source)?;
        let body =
            std::str::from_utf8(source.bytes()).map_err(|_| NativeConfigError::InvalidUtf8)?;
        let parsed =
            PakroToml::from_str(body, &path).map_err(|_| NativeConfigError::InvalidDocument)?;
        after_parse();
        source.recheck().map_err(NativeConfigError::Source)?;
        Ok(Self { parsed, source })
    }
    pub(crate) fn recheck(&self) -> NativeConfigResult<()> {
        self.source.recheck().map_err(NativeConfigError::Source)
    }
    /// These returned requests are data. They cannot mint a current Config.
    pub(crate) fn checked_package_requests(
        &self,
    ) -> NativeConfigResult<Vec<PackageInstallRequest>> {
        self.recheck()?;
        let requests = self
            .parsed
            .checked_package_requests()
            .map_err(|_| NativeConfigError::InvalidPackages);
        self.recheck()?;
        requests
    }
    pub(crate) fn for_python(&self) -> NativeConfigResult<NativePythonConfigReceipt<'_>> {
        self.recheck()?;
        Ok(NativePythonConfigReceipt { owner: self })
    }
}
impl fmt::Debug for NativePythonConfigFile {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("NativePythonConfigFile")
            .finish_non_exhaustive()
    }
}

pub(crate) struct NativePythonConfigReceipt<'a> {
    owner: &'a NativePythonConfigFile,
}
impl NativePythonConfigReceipt<'_> {
    pub(crate) fn recheck_for(&self, owner: &NativePythonConfigFile) -> NativeConfigResult<()> {
        if !std::ptr::eq(self.owner, owner) {
            return Err(NativeConfigError::WrongOwner);
        }
        self.owner.recheck()
    }
}
impl fmt::Debug for NativePythonConfigReceipt<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("NativePythonConfigReceipt")
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, unix))]
#[path = "native_python/tests.rs"]
mod tests;
