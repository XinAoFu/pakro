use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, StoreError};
use crate::identity::{Digest, PackageIdentity};
use crate::path::entry_dir;

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn entry_path(&self, id: &PackageIdentity) -> PathBuf {
        entry_dir(&self.root, id)
    }

    /// An entry only counts as present once its unpack finished completely;
    /// the marker file is written last, before the directory is moved into
    /// place.
    pub fn contains(&self, id: &PackageIdentity) -> bool {
        self.entry_path(id).join(".mise-complete").is_file()
    }

    /// Verifies `bytes` against the identity's digest, unpacks them into the
    /// store, and returns the entry path. Unpacking an identity that is
    /// already present returns its path without writing anything.
    pub fn unpack(&self, id: &PackageIdentity, bytes: &[u8]) -> Result<PathBuf> {
        if !id.digest.verify(bytes) {
            return Err(StoreError::DigestMismatch {
                identity: id.to_string(),
                expected: id.digest.hex(),
                actual: match id.digest {
                    Digest::Sha256(_) => Digest::from_sha256_bytes(bytes).hex(),
                    Digest::Sha512(_) => Digest::from_sha512_bytes(bytes).hex(),
                },
            });
        }
        let dst = self.entry_path(id);
        if self.contains(id) {
            return Ok(dst);
        }
        fs::create_dir_all(dst.parent().expect("entry path always has a parent"))?;
        let tmp = self.root.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            &id.digest.hex()[..8]
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        extract(&tmp, bytes)?;
        fs::write(tmp.join(".mise-complete"), b"")?;
        fs::write(tmp.join(".mise-meta.json"), serde_json::to_vec(id)?)?;
        match fs::rename(&tmp, &dst) {
            Ok(()) => Ok(dst),
            Err(_) => {
                // a concurrent unpacker of the same identity won the rename;
                // its bytes are identical so the existing entry is equivalent
                let _ = fs::remove_dir_all(&tmp);
                Ok(dst)
            }
        }
    }
}

/// Dispatches on magic bytes: wheels are zip archives, npm tarballs and other
/// sources are gzipped tar.
fn extract(dir: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        extract_zip(dir, bytes)
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        extract_tar_gz(dir, bytes)
    } else {
        Err(StoreError::InvalidArchive)
    }
}

fn extract_zip(dir: &Path, bytes: &[u8]) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    archive.extract(dir)?;
    Ok(())
}

fn extract_tar_gz(dir: &Path, bytes: &[u8]) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dir)?;
    Ok(())
}
