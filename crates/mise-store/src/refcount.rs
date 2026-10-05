use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::PackageIdentity;
use crate::store::Store;

const REFS_FILE: &str = ".mise-refs.json";
const COMPLETE_FILE: &str = ".mise-complete";

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
/// `.mise-complete` marker are left alone: they are either mid-unpack from a
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
