//! Private library boundary. Locations here are requested descriptions only;
//! R05 must bind actual directory identities before granting any IO authority.

use std::path::{Component, Path, PathBuf};

pub(crate) mod language;
pub(crate) mod package;
pub(crate) mod project;
pub(crate) mod reclaim;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidLibraryLocations;

#[derive(Clone)]
pub(crate) struct RequestedLibraryLocations {
    data_root: PathBuf,
    cache_root: PathBuf,
}

impl RequestedLibraryLocations {
    pub(crate) fn new(
        data_root: PathBuf,
        cache_root: PathBuf,
    ) -> Result<Self, InvalidLibraryLocations> {
        if [&data_root, &cache_root].iter().any(|path| {
            !path.is_absolute() || path.components().any(|part| part == Component::ParentDir)
        }) || data_root == cache_root
        {
            return Err(InvalidLibraryLocations);
        }
        Ok(Self {
            data_root,
            cache_root,
        })
    }

    pub(crate) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(crate) fn cache_root(&self) -> &Path {
        &self.cache_root
    }
}

#[cfg(test)]
mod tests;
