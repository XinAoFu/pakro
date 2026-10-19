use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLimit {
    Arguments,
    ArgumentBytes,
    Declarations,
    RuntimeCandidates,
}

/// Request-layer limits. Source, graph and build budgets belong to their stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestBudget {
    pub max_arguments: usize,
    pub max_total_argument_bytes: usize,
    pub max_declarations: usize,
    pub max_runtime_candidates: usize,
}

/// Allowed kinds of change, not publication or file-write capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowedWrites {
    pub libraries: bool,
    pub project_selection: bool,
    pub lockfile: bool,
    pub declarations: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationPolicy {
    pub locked: bool,
    pub offline: bool,
    pub deadline: Instant,
    pub budget: RequestBudget,
    pub writes: AllowedWrites,
}
