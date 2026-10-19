//! Source-bound inputs for the native Python dependency-resolution pipeline.
//!
//! These owners are neither an effective dependency graph nor lock or
//! installation authority. Existing JSON-based adapters remain independent.

pub(crate) mod candidate_policy;
pub(crate) mod graph_check;
pub(crate) mod lock_model;
pub(crate) mod lock_session;
pub(crate) mod marker_truth;
pub(crate) mod metadata_input;
pub(crate) mod root_scopes;
pub(crate) mod source;
