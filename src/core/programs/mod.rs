//! Limited application facade for the independent CLI crate. Implementation
//! modules and library IO boundaries are not part of this public surface.

#[cfg(unix)]
pub mod directory;
pub(crate) mod install;
#[cfg(unix)]
pub(crate) mod query;
pub(crate) mod shared;

pub use install::request::{
    InstallCommandIntent, InstallRequest, InvocationInputs, InvocationSnapshot, normalize_intent,
};
pub use shared::context::{
    OperationContext, OperationContextInputs, PythonSourceInputs, capture_operation_context,
};
pub use shared::errors::{MissingOperationInput, OperationError};
pub use shared::policy::{AllowedWrites, OperationPolicy, RequestBudget, RequestLimit};
pub use shared::report::{
    InterruptedOperation, OperationEffects, OperationReport, OperationState, OperationTermination,
};
pub use shared::types::{
    DeclarationEditIntent, DeclarationOperation, DeclarationTarget, RuntimeCandidate,
};
