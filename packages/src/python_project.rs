//! A current declaration-file observation; never source-tree or write authority.

#[path = "python_project/resolve.rs"]
pub(crate) mod resolve;

use super::python_declarations::{
    ParsedPythonProject, PythonDeclarationError, PythonDeclarationSelection, PythonExpansionLimits,
    PythonProjectLimits, PythonProjectSelection, parse_pyproject,
};
use crate::lockfile::transaction::{
    ReadOnlyFileError, ReadOnlyFileLimits, ReadOnlyFileObservation,
};
use std::fmt;
use std::path::Path;
use std::time::Instant;
use thiserror::Error;

pub(crate) type PythonProjectFileResult<T> = Result<T, PythonProjectFileError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum PythonProjectFileError {
    #[error("Python project file observation failed: {0}")]
    Observation(ReadOnlyFileError),
    #[error("Python project file is not valid UTF-8")]
    InvalidUtf8,
    #[error("Python project declaration is invalid: {0}")]
    Declaration(PythonDeclarationError),
    #[error("Python declaration selection belongs to another file owner")]
    WrongOwner,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PythonProjectFileLimits {
    pub document: PythonProjectLimits,
    pub max_path_bytes: usize,
    pub max_components: usize,
    pub max_symlink_hops: usize,
}
impl Default for PythonProjectFileLimits {
    fn default() -> Self {
        Self {
            document: Default::default(),
            max_path_bytes: 16 * 1024,
            max_components: 1024,
            max_symlink_hops: 64,
        }
    }
}

pub(crate) struct BoundPythonProjectFile {
    observation: ReadOnlyFileObservation,
    parsed: ParsedPythonProject,
}
impl BoundPythonProjectFile {
    pub(crate) fn open(
        path: &Path,
        limits: PythonProjectFileLimits,
        deadline: Instant,
    ) -> PythonProjectFileResult<Self> {
        limits
            .document
            .validate()
            .map_err(PythonProjectFileError::Declaration)?;
        let observation = ReadOnlyFileObservation::open(
            path,
            ReadOnlyFileLimits {
                max_bytes: limits.document.max_document_bytes,
                max_path_bytes: limits.max_path_bytes,
                max_components: limits.max_components,
                max_symlink_hops: limits.max_symlink_hops,
            },
            deadline,
        )
        .map_err(PythonProjectFileError::Observation)?;
        let raw = std::str::from_utf8(observation.bytes())
            .map_err(|_| PythonProjectFileError::InvalidUtf8)?;
        let parsed =
            parse_pyproject(raw, limits.document).map_err(PythonProjectFileError::Declaration)?;
        observation
            .recheck()
            .map_err(PythonProjectFileError::Observation)?;
        Ok(Self {
            observation,
            parsed,
        })
    }
    /// Parsed facts alone do not prove that the current file is unchanged.
    pub(crate) fn parsed(&self) -> &ParsedPythonProject {
        &self.parsed
    }
    /// The same bytes retained by this owner, without opening the path again.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.observation.bytes()
    }
    pub(crate) fn recheck(&self) -> PythonProjectFileResult<()> {
        self.observation
            .recheck()
            .map_err(PythonProjectFileError::Observation)
    }
    pub(crate) fn select(
        &self,
        selection: PythonProjectSelection<'_>,
        limits: PythonExpansionLimits,
    ) -> PythonProjectFileResult<BoundPythonDeclarationSelection<'_>> {
        self.recheck()?;
        let declarations = self
            .parsed
            .select(selection, limits)
            .map_err(PythonProjectFileError::Declaration)?;
        self.recheck()?;
        Ok(BoundPythonDeclarationSelection {
            owner: self,
            declarations,
        })
    }
}
impl fmt::Debug for BoundPythonProjectFile {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("BoundPythonProjectFile")
            .finish_non_exhaustive()
    }
}

pub(crate) struct BoundPythonDeclarationSelection<'a> {
    owner: &'a BoundPythonProjectFile,
    declarations: PythonDeclarationSelection,
}
impl BoundPythonDeclarationSelection<'_> {
    /// This is declaration data, not a worker or graph capability.
    pub(crate) fn declarations(&self) -> &PythonDeclarationSelection {
        &self.declarations
    }
    pub(crate) fn recheck(&self) -> PythonProjectFileResult<()> {
        self.owner.recheck()
    }
    pub(crate) fn recheck_for(
        &self,
        owner: &BoundPythonProjectFile,
    ) -> PythonProjectFileResult<()> {
        if !std::ptr::eq(self.owner, owner) {
            return Err(PythonProjectFileError::WrongOwner);
        }
        self.recheck()
    }
}
impl fmt::Debug for BoundPythonDeclarationSelection<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("BoundPythonDeclarationSelection")
            .field("declarations", &self.declarations)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "python_project/tests.rs"]
mod tests;
