//! Executable and bin-directory projections of one selected language.

use std::path::{Path, PathBuf};

use super::snapshot::LanguageCandidate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectionPaths {
    root: PathBuf,
    bin_dir: PathBuf,
    executable: PathBuf,
}

impl SelectionPaths {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }
}

/// Projects the real manifest entrypoint and payload root of the candidate.
/// The bin directory is the entrypoint's parent inside the shared payload.
pub(crate) fn selection_paths(candidate: &LanguageCandidate) -> SelectionPaths {
    let executable = candidate.entrypoint().to_path_buf();
    let bin_dir = executable
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| candidate.root().to_path_buf());
    SelectionPaths {
        root: candidate.root().to_path_buf(),
        bin_dir,
        executable,
    }
}

#[cfg(test)]
mod tests;
