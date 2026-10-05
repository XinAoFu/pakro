use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolverError {
    #[error("dependency cycle involving {0}")]
    Cycle(String),
    #[error("node {0} is referenced as a dependency but absent from the graph")]
    MissingNode(String),
    #[error("node {0} appears more than once; a resolved graph holds one version per name")]
    DuplicateNode(String),
}

/// Failures of the version solving step.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveFailure {
    #[error("no version satisfies the constraints:\n{report}")]
    NoSolution { report: String },
    #[error("top-level request {name} must be an exact version, got {constraint:?}")]
    NotPinned { name: String, constraint: String },
}
