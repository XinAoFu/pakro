//! Reentrant, data-only Python solving. Missing metadata is an input request,
//! never an empty dependency list and never installation authority.

use std::fmt;
use thiserror::Error;

#[path = "python_native/provider.rs"]
mod provider;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidateKey(pub [u8; 32]);
impl fmt::Debug for CandidateKey {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("CandidateKey(<local data identity>)")
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct CandidateVersion {
    pub key: CandidateKey,
    pub version: String,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Requirement {
    pub name: String,
    pub constraint: RequirementConstraint,
    pub requested_extras: Vec<String>,
}
#[derive(Clone, PartialEq, Eq)]
pub enum RequirementConstraint {
    Version(String),
    /// The original token remains with the actual app metadata owner.
    SourceReference,
}
/// Explicit Current2026 truth data. Validation belongs to the provider too;
/// constructing this value grants no source or resolution authority.
#[derive(Clone, PartialEq, Eq)]
pub struct ExtraTruth {
    pub extra_names: Vec<String>,
    pub enabled_masks: Vec<usize>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Dependency {
    pub requirement: Requirement,
    pub extra_truth: Option<ExtraTruth>,
}
impl Dependency {
    pub fn unconditional(requirement: Requirement) -> Self {
        Self {
            requirement,
            extra_truth: None,
        }
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CandidateMetadata {
    pub dependencies: Vec<Dependency>,
    /// An adapter's known compatibility verdict, never an instruction to read
    /// the host interpreter or an untrusted error message.
    pub rejection: Option<CandidateRejection>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateRejection {
    RequiresPython,
}
pub trait MetadataDepot {
    fn project(&self, name: &str) -> Option<&[CandidateVersion]>;
    fn metadata(&self, candidate: CandidateKey) -> Option<&CandidateMetadata>;
}
#[derive(Debug, Clone, Copy)]
pub struct NativeSolveLimits {
    pub max_projects: usize,
    pub max_candidates: usize,
    pub max_dependencies: usize,
    pub max_field_bytes: usize,
    pub max_total_utf8_bytes: usize,
    pub max_solver_calls: usize,
    pub max_extras_per_requirement: usize,
    pub max_truth_extras: usize,
    pub max_total_extra_references: usize,
    pub max_total_guard_states: usize,
    pub max_truth_work: usize,
}
impl Default for NativeSolveLimits {
    fn default() -> Self {
        Self {
            max_projects: 65_536,
            max_candidates: 262_144,
            max_dependencies: 1_048_576,
            max_field_bytes: 16_384,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            max_solver_calls: 4_194_304,
            max_extras_per_requirement: 256,
            max_truth_extras: 16,
            max_total_extra_references: 1_048_576,
            max_total_guard_states: 1_048_576,
            max_truth_work: 16_777_216,
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct SelectedCandidate {
    pub name: String,
    pub key: CandidateKey,
    pub version: String,
    pub requested_extras: Vec<String>,
}
impl fmt::Debug for SelectedCandidate {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("SelectedCandidate")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("requested_extras", &self.requested_extras)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePlan {
    pub selected: Vec<SelectedCandidate>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolveStep {
    NeedProject(String),
    NeedMetadata(CandidateKey),
    NeedSource {
        parent: CandidateKey,
        declaration: usize,
    },
    Complete(NativePlan),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NativeSolveError {
    #[error("native Python solve limits must be positive")]
    InvalidLimits,
    #[error("native Python solve budget exceeded")]
    Limit,
    #[error("invalid native Python solve input")]
    InvalidInput,
    #[error("native Python constraints have no solution")]
    NoSolution,
}

/// Candidate order is supplied explicitly by the policy adapter. This core
/// does not invent host, prerelease, yanked or source-selection policy.
pub fn solve_step(
    requests: &[Requirement],
    depot: &dyn MetadataDepot,
    limits: NativeSolveLimits,
) -> Result<SolveStep, NativeSolveError> {
    provider::run(requests, depot, limits)
}

#[cfg(test)]
mod tests;

#[path = "python_native/policy.rs"]
pub mod policy;
