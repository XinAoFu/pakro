//! Local location binding between a project identity and its directory.

use std::path::{Path, PathBuf};

use eyre::Result;

use super::snapshot::ObservedProjectInputs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectLocation {
    root: PathBuf,
}

impl ProjectLocation {
    /// Binds the observed root to its real directory identity. The canonical
    /// path is what project identity is derived from; non-ASCII roots stay in
    /// this local binding and never enter identity keys.
    pub(crate) fn bind(observed: &ObservedProjectInputs) -> Result<Self> {
        let root = observed.root();
        eyre::ensure!(
            root.is_absolute()
                && !root
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
            "project location must be an absolute path without parent components"
        );
        let canonical = std::fs::canonicalize(root)
            .map_err(|_| eyre::eyre!("project location is unavailable"))?;
        eyre::ensure!(
            canonical.is_dir(),
            "project location does not name a directory"
        );
        Ok(Self { root: canonical })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}
