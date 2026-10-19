use thiserror::Error;

use super::policy::RequestLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingOperationInput {
    PythonSources,
    PythonTargetDescription,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperationError {
    #[error("invalid install argument at index {index}")]
    InvalidArgument { index: usize },
    #[error("a package request needs a declared package-capable runtime or an explicit binding")]
    MissingRuntimeBinding,
    #[error(
        "package request has {candidate_count} runtime candidates; specify an explicit binding"
    )]
    AmbiguousRuntimeBinding { candidate_count: usize },
    #[error("locked mode does not permit declaration edits")]
    LockedDeclarationEdit,
    #[error("the operation deadline has elapsed")]
    DeadlineExceeded,
    #[error("request budget exceeded: {0:?}")]
    ResourceLimit(RequestLimit),
    #[error("the operation does not allow declaration writes")]
    DeclarationWriteDenied,
    #[error("a declaration edit needs an absolute document path and a nonempty field path")]
    InvalidDeclarationTarget,
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("operation input is missing: {0:?}")]
    MissingInput(MissingOperationInput),
    #[error(
        "library and cache locations must be distinct absolute paths without parent components"
    )]
    InvalidLibraryLocations,
    #[error("the explicit Python source policy is invalid")]
    InvalidSourceConfiguration,
}
