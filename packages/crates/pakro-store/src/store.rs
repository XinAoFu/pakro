use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::{Digest, PackageIdentity};
use crate::{Result, StoreError};

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
        self.entry_path(id).join(".pakro-complete").is_file()
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
        fs::write(tmp.join(".pakro-complete"), b"")?;
        fs::write(tmp.join(".pakro-meta.json"), serde_json::to_vec(id)?)?;
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

/// Directory component for a store entry.
pub fn entry_dir(root: &Path, id: &PackageIdentity) -> PathBuf {
    root.join(id.eco.dir_name())
        .join(id.dir_name())
        .join(&id.version)
        .join(id.digest.to_string())
}

/// Recursively recreates `src` at `dst`: directories are created, regular
/// files are hardlinked when the filesystem allows it (same volume) and
/// copied otherwise, and symlinks are recreated pointing at their original
/// targets. Hardlinks keep the store the single owner of the bytes while
/// making the copy behave like a real directory tree for tools that resolve
/// realpaths (node's resolver).
pub fn hardlink_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = entry.metadata()?;
        if meta.is_dir() {
            hardlink_tree(&from, &to)?;
        } else if entry.file_type()?.is_symlink() {
            let target = std::fs::read_link(&from)?;
            if std::fs::symlink_metadata(&to).is_ok() {
                std::fs::remove_file(&to)?;
            }
            std::os::unix::fs::symlink(target, &to)?;
        } else {
            // a stale link at `to` must go first: when `from` and `to` are
            // already links to the same inode, `fs::copy` would truncate the
            // shared file and zero the store entry itself
            if std::fs::symlink_metadata(&to).is_ok() {
                std::fs::remove_file(&to)?;
            }
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to)?;
            }
        }
    }
    Ok(())
}

const REFS_FILE: &str = ".pakro-refs.json";
const COMPLETE_FILE: &str = ".pakro-complete";

/// Hardlinks one file to `dst`, replacing whatever held the destination name.
/// Falls back to a copy across filesystems (EXDEV). Removing the stale name
/// first matters for the same reason as in [`hardlink_tree`]: copy would
/// truncate an inode shared with the store.
pub fn hardlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dst).is_ok() {
        std::fs::remove_file(dst)?;
    }
    match std::fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        // EXDEV: link across filesystems — a real copy keeps the layout valid
        Err(err) if err.raw_os_error() == Some(18) => std::fs::copy(src, dst).map(|_| ()),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refs {
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<PathBuf>,
    pub freed_bytes: u64,
}

impl Store {
    pub fn add_ref(&self, id: &PackageIdentity, project_root: &str) -> Result<()> {
        let mut refs = self.refs(id)?;
        refs.projects.push(project_root.to_string());
        refs.projects.sort();
        refs.projects.dedup();
        self.save_refs(id, &refs)
    }

    pub fn remove_ref(&self, id: &PackageIdentity, project_root: &str) -> Result<()> {
        let mut refs = self.refs(id)?;
        refs.projects.retain(|project| project != project_root);
        self.save_refs(id, &refs)
    }

    /// Projects currently referencing an entry. A missing refs file means an
    /// entry that was unpacked but never linked into a project.
    pub fn refs(&self, id: &PackageIdentity) -> Result<Refs> {
        read_refs_at(&self.entry_path(id).join(REFS_FILE))
    }

    fn save_refs(&self, id: &PackageIdentity, refs: &Refs) -> Result<()> {
        let path = self.entry_path(id).join(REFS_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(refs)?)?;
        Ok(())
    }
}

/// Removes completed entries whose refcount reached zero, plus leftover
/// `.tmp-*` directories from interrupted unpacks. Entries without a
/// `.pakro-complete` marker are left alone: they are either mid-unpack from a
/// concurrent process or already handled by the tmp pass.
pub fn gc(store: &Store) -> Result<GcReport> {
    let mut report = GcReport::default();
    for eco_dir in subdirs(&store.root)? {
        for name_dir in subdirs(&eco_dir)? {
            for version_dir in subdirs(&name_dir)? {
                for entry_dir in subdirs(&version_dir)? {
                    if !entry_dir.join(COMPLETE_FILE).is_file() {
                        continue;
                    }
                    if !read_refs_at(&entry_dir.join(REFS_FILE))?
                        .projects
                        .is_empty()
                    {
                        continue;
                    }
                    remove(&mut report, &entry_dir)?;
                }
            }
        }
    }
    if let Ok(children) = fs::read_dir(&store.root) {
        for child in children.flatten() {
            let path = child.path();
            let is_tmp = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".tmp-"));
            if is_tmp && path.is_dir() {
                remove(&mut report, &path)?;
            }
        }
    }
    Ok(report)
}

fn remove(report: &mut GcReport, path: &Path) -> Result<()> {
    report.freed_bytes += dir_size(path);
    fs::remove_dir_all(path)?;
    report.removed.push(path.to_path_buf());
    Ok(())
}

fn read_refs_at(path: &Path) -> Result<Refs> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Refs::default()),
        Err(err) => Err(err.into()),
    }
}

fn subdirs(dir: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut out = vec![];
            for entry in entries {
                let path = entry?.path();
                let visible = path
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with('.'));
                if path.is_dir() && visible {
                    out.push(path);
                }
            }
            Ok(out)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(err) => Err(err.into()),
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                total += dir_size(&child);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}
