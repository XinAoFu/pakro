//! Pure policy queries over actual active declarations, never source authority.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActiveRequirementOrigin {
    Root {
        declaration: usize,
    },
    Dependency {
        parent: CandidateKey,
        declaration: usize,
    },
}
pub struct ActiveVersionConstraint<'a> {
    pub origin: ActiveRequirementOrigin,
    pub raw: &'a str,
}
pub struct PolicyCandidate<'a> {
    pub candidate: &'a CandidateVersion,
    pub metadata: Option<&'a CandidateMetadata>,
}
pub struct ActivePolicyQuery<'a> {
    pub name: &'a str,
    pub selected: CandidateKey,
    pub constraints: &'a [ActiveVersionConstraint<'a>],
    pub candidates: &'a [PolicyCandidate<'a>],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePolicyDecision {
    Allow,
    Reject,
    /// Reject this assignment while advising a known compatible candidate of
    /// the same project during retries of this immutable input snapshot. This
    /// is ordering advice, never a pin, a new constraint, cached policy truth,
    /// metadata evidence or a selected-candidate fact. Every new assignment is
    /// evaluated again; advice is discarded when this solve step returns.
    /// The complete candidate universe and all alternative parent branches
    /// remain available if the advised candidate cannot satisfy the solve.
    RejectWithAlternative(CandidateKey),
    NeedMetadata(CandidateKey),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePolicyError {
    InvalidInput,
    Limit,
}
pub trait ActiveCandidatePolicy {
    fn evaluate(
        &self,
        query: ActivePolicyQuery<'_>,
    ) -> Result<ActivePolicyDecision, ActivePolicyError>;
}
#[derive(Debug, Clone, Copy)]
pub struct NativePolicyLimits {
    pub max_rounds: usize,
    pub max_nogood_terms: usize,
    pub max_active_references: usize,
    pub max_propagation_steps: usize,
}
impl Default for NativePolicyLimits {
    fn default() -> Self {
        Self {
            max_rounds: 4096,
            max_nogood_terms: 1_048_576,
            max_active_references: 1_048_576,
            max_propagation_steps: 16_777_216,
        }
    }
}
impl NativePolicyLimits {
    pub fn validate(self) -> Result<(), NativeSolveError> {
        if [
            self.max_rounds,
            self.max_nogood_terms,
            self.max_active_references,
            self.max_propagation_steps,
        ]
        .contains(&0)
        {
            return Err(NativeSolveError::InvalidLimits);
        }
        Ok(())
    }
}
pub fn solve_step_with_policy(
    roots: &[Requirement],
    depot: &dyn MetadataDepot,
    policy: &dyn ActiveCandidatePolicy,
    solver_limits: NativeSolveLimits,
    policy_limits: NativePolicyLimits,
) -> Result<SolveStep, NativeSolveError> {
    super::provider::run_with_policy(roots, depot, policy, solver_limits, policy_limits)
}

#[cfg(test)]
#[path = "policy/tests.rs"]
mod tests;
