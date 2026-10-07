//! On-disk cache of raw registry responses. One JSON file per package
//! version; freshness is the file's mtime, checked by [`MetadataCache::read_with_ttl`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::Result;

pub struct MetadataCache {
    root: PathBuf,
}

impl MetadataCache {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn path(&self, eco: &str, name: &str, version: &str) -> PathBuf {
        self.root
            .join(eco)
            .join(name)
            .join(format!("{version}.json"))
    }

    pub fn read(&self, eco: &str, name: &str, version: &str) -> Result<Option<Vec<u8>>> {
        match fs::read(self.path(eco, name, version)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Like [`Self::read`] but treats entries older than `ttl` as absent so
    /// the caller refetches.
    pub fn read_with_ttl(
        &self,
        eco: &str,
        name: &str,
        version: &str,
        ttl: Duration,
    ) -> Result<Option<Vec<u8>>> {
        let path = self.path(eco, name, version);
        let modified = match fs::metadata(&path) {
            Ok(meta) => meta.modified()?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if age > ttl {
            return Ok(None);
        }
        self.read(eco, name, version)
    }

    pub fn write(&self, eco: &str, name: &str, version: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path(eco, name, version);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
        Ok(())
    }
}
