//! Lossless marker syntax with explicit, versioned evaluation semantics.
//!
//! Inputs never acquire host defaults. Parsing retains comparisons which an
//! algebraic dependency approximation may discard; no graph authority follows.

use std::fmt;
use std::ops::Range;

use thiserror::Error;

mod eval;
mod parse;

pub type PythonMarkerResult<T> = Result<T, PythonMarkerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerSemantics {
    Current2026,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerField {
    OsName,
    SysPlatform,
    PlatformRelease,
    PlatformSystem,
    PlatformVersion,
    PlatformMachine,
    PlatformPythonImplementation,
    PythonVersion,
    PythonFullVersion,
    ImplementationName,
    ImplementationVersion,
    Extra,
    Extras,
    DependencyGroups,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerOperator {
    Less,
    LessEqual,
    NotEqual,
    ArbitraryEqual,
    Equal,
    GreaterEqual,
    Greater,
    Compatible,
    In,
    NotIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerLimitKind {
    InputBytes,
    LiteralBytes,
    Tokens,
    Comparisons,
    Nodes,
    GroupDepth,
    EnvironmentBytes,
    EnvironmentFieldBytes,
    SetEntries,
    EvaluationSteps,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PythonMarkerError {
    #[error("marker limits must be positive")]
    InvalidLimits,
    #[error("marker limit exceeded: {0:?}")]
    LimitExceeded(PythonMarkerLimitKind),
    #[error("invalid marker syntax at byte {offset}")]
    Syntax { offset: usize },
    #[error("unknown marker field at byte {offset}")]
    UnknownField { offset: usize },
    #[error("marker field is unavailable in this explicit context: {0:?}")]
    ContextUnavailable(PythonMarkerField),
    #[error("invalid marker context name for {field:?} at item {index}")]
    InvalidContextName {
        field: PythonMarkerField,
        index: usize,
    },
    #[error("invalid marker extra name at comparison {comparison}")]
    InvalidExtraName { comparison: usize },
}

#[derive(Debug, Clone, Copy)]
pub struct PythonMarkerLimits {
    pub max_input_bytes: usize,
    pub max_literal_bytes: usize,
    pub max_tokens: usize,
    pub max_nodes: usize,
    pub max_group_depth: usize,
    pub max_environment_bytes: usize,
    pub max_environment_field_bytes: usize,
    pub max_set_entries: usize,
    pub max_evaluation_steps: usize,
}
impl Default for PythonMarkerLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024,
            max_literal_bytes: 4 * 1024,
            max_tokens: 1024,
            max_nodes: 512,
            max_group_depth: 64,
            max_environment_bytes: 64 * 1024,
            max_environment_field_bytes: 16 * 1024,
            max_set_entries: 1024,
            max_evaluation_steps: 4096,
        }
    }
}
impl PythonMarkerLimits {
    pub fn validate(self) -> PythonMarkerResult<()> {
        validate_python_marker_limits(self)
    }
}

/// All eleven base fields are mandatory inputs; values are never repaired
/// from the current process or silently substituted for another interpreter.
#[derive(Clone, Copy)]
pub struct PythonMarkerEnvironment<'a> {
    pub os_name: &'a str,
    pub sys_platform: &'a str,
    pub platform_release: &'a str,
    pub platform_system: &'a str,
    pub platform_version: &'a str,
    pub platform_machine: &'a str,
    pub platform_python_implementation: &'a str,
    pub python_version: &'a str,
    pub python_full_version: &'a str,
    pub implementation_name: &'a str,
    pub implementation_version: &'a str,
}
impl fmt::Debug for PythonMarkerEnvironment<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("PythonMarkerEnvironment(<eleven explicit fields>)")
    }
}

#[derive(Clone, Copy)]
pub enum PythonMarkerScope<'a> {
    Environment,
    Dependency {
        requested_extras: &'a [&'a str],
    },
    Lock {
        selected_extras: &'a [&'a str],
        dependency_groups: &'a [&'a str],
    },
}
impl fmt::Debug for PythonMarkerScope<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => output.write_str("PythonMarkerScope::Environment"),
            Self::Dependency { requested_extras } => output
                .debug_struct("PythonMarkerScope::Dependency")
                .field("requested_count", &requested_extras.len())
                .finish(),
            Self::Lock {
                selected_extras,
                dependency_groups,
            } => output
                .debug_struct("PythonMarkerScope::Lock")
                .field("selected_extra_count", &selected_extras.len())
                .field("selected_group_count", &dependency_groups.len())
                .finish(),
        }
    }
}

enum Operand {
    Field {
        field: PythonMarkerField,
        span: Range<usize>,
    },
    Literal {
        span: Range<usize>,
        value_span: Range<usize>,
    },
}
struct Comparison {
    left: Operand,
    operator: PythonMarkerOperator,
    operator_span: Range<usize>,
    right: Operand,
    span: Range<usize>,
}
enum MarkerNode {
    Comparison(Comparison),
    And(usize, usize),
    Or(usize, usize),
    Group(usize),
}

pub struct PythonMarkerExpression {
    raw: String,
    nodes: Vec<MarkerNode>,
    root: usize,
    token_count: usize,
    group_depth: usize,
}
impl PythonMarkerExpression {
    pub fn raw_for_semantic_resolution(&self) -> &str {
        &self.raw
    }
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn comparisons(&self) -> impl Iterator<Item = PythonMarkerComparison<'_>> {
        self.nodes.iter().filter_map(|node| match node {
            MarkerNode::Comparison(comparison) => Some(PythonMarkerComparison {
                raw: &self.raw,
                comparison,
            }),
            _ => None,
        })
    }
}
impl fmt::Debug for PythonMarkerExpression {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonMarkerExpression")
            .field("input_bytes", &self.raw.len())
            .field("nodes", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerOperand<'a> {
    Field(PythonMarkerField),
    Literal(&'a str),
}
impl fmt::Debug for PythonMarkerOperand<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(field) => fmt::Debug::fmt(field, output),
            Self::Literal(_) => output.write_str("Literal(<private value>)"),
        }
    }
}
pub struct PythonMarkerComparison<'a> {
    raw: &'a str,
    comparison: &'a Comparison,
}
impl<'a> PythonMarkerComparison<'a> {
    fn operand(&self, value: &'a Operand) -> PythonMarkerOperand<'a> {
        match value {
            Operand::Field { field, .. } => PythonMarkerOperand::Field(*field),
            Operand::Literal { value_span, .. } => {
                PythonMarkerOperand::Literal(&self.raw[value_span.clone()])
            }
        }
    }
    pub fn left(&self) -> PythonMarkerOperand<'a> {
        self.operand(&self.comparison.left)
    }
    pub fn right(&self) -> PythonMarkerOperand<'a> {
        self.operand(&self.comparison.right)
    }
    pub fn operator(&self) -> PythonMarkerOperator {
        self.comparison.operator
    }
    pub fn span(&self) -> Range<usize> {
        self.comparison.span.clone()
    }
    pub fn operator_span(&self) -> Range<usize> {
        self.comparison.operator_span.clone()
    }
    pub fn left_span(&self) -> Range<usize> {
        match &self.comparison.left {
            Operand::Field { span, .. } | Operand::Literal { span, .. } => span.clone(),
        }
    }
    pub fn right_span(&self) -> Range<usize> {
        match &self.comparison.right {
            Operand::Field { span, .. } | Operand::Literal { span, .. } => span.clone(),
        }
    }
}
impl fmt::Debug for PythonMarkerComparison<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonMarkerComparison")
            .field("left", &self.left())
            .field("operator", &self.operator())
            .field("right", &self.right())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerWarningKind {
    StringOrderedComparison,
    StringCompatibleEquality,
    VersionStringFallback,
    VersionContainment,
    UnsupportedExtraComparison,
    UnsupportedSetComparison,
    LiteralOnlyComparison,
    VariableVariableComparison,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythonMarkerWarning {
    pub comparison_index: usize,
    pub kind: PythonMarkerWarningKind,
}
#[derive(Debug)]
pub struct PythonMarkerEvaluation {
    value: bool,
    semantics: MarkerSemantics,
    warnings: Vec<PythonMarkerWarning>,
}
impl PythonMarkerEvaluation {
    pub fn value(&self) -> bool {
        self.value
    }
    pub fn semantics(&self) -> MarkerSemantics {
        self.semantics
    }
    pub fn warnings(&self) -> &[PythonMarkerWarning] {
        &self.warnings
    }
}

pub fn parse_python_marker(
    raw: &str,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<PythonMarkerExpression> {
    parse::parse(raw, limits)
}

pub(crate) fn validate_python_marker_limits(limits: PythonMarkerLimits) -> PythonMarkerResult<()> {
    parse::validate_limits(limits)
}

/// Share the real lexer with declaration preflight. No expression, owned raw
/// text or library marker tree is constructed while checking its budgets.
pub(crate) fn preflight_python_marker(
    raw: &str,
    limits: PythonMarkerLimits,
    max_comparisons: usize,
) -> PythonMarkerResult<usize> {
    parse::preflight(raw, limits, max_comparisons)
}

pub fn evaluate_python_marker(
    expression: &PythonMarkerExpression,
    environment: PythonMarkerEnvironment<'_>,
    scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<PythonMarkerEvaluation> {
    eval::evaluate(expression, environment, scope, semantics, limits)
}

#[cfg(test)]
#[path = "python_markers/tests.rs"]
mod tests;
