//! Finite extra-set conditions compiled from the exact marker AST.
//! This is pure metadata, never source, graph or installation authority.

use pakro_index::python_markers::{
    MarkerSemantics, PythonMarkerEnvironment, PythonMarkerError, PythonMarkerExpression,
    PythonMarkerLimits, PythonMarkerWarning,
};
use pakro_index::python_markers::{
    PythonMarkerField, PythonMarkerOperand, PythonMarkerOperator, PythonMarkerScope,
    evaluate_python_marker,
};
use pakro_index::python_source::NormalizedPythonName;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExtraTruthLimits {
    pub max_relevant_extras: usize,
    pub max_truth_states: usize,
    pub max_total_evaluation_nodes: usize,
    pub max_name_bytes: usize,
}
impl Default for ExtraTruthLimits {
    fn default() -> Self {
        Self {
            max_relevant_extras: 12,
            max_truth_states: 4096,
            max_total_evaluation_nodes: 4_194_304,
            max_name_bytes: 16_384,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ExtraTruthError {
    #[error("extra truth limits must be positive")]
    InvalidLimits,
    #[error("extra truth compilation budget exceeded")]
    Limit,
    #[error("exact marker evaluation failed: {0}")]
    Marker(PythonMarkerError),
    #[error("marker semantics cannot be represented by monotone extras")]
    NonMonotone,
}
pub(crate) struct MarkerExtraTruth {
    semantics: MarkerSemantics,
    names: Vec<String>,
    enabled: Vec<usize>,
    warnings: Vec<PythonMarkerWarning>,
}
impl MarkerExtraTruth {
    pub(crate) fn semantics(&self) -> MarkerSemantics {
        self.semantics
    }
    pub(crate) fn extra_names(&self) -> &[String] {
        &self.names
    }
    pub(crate) fn enabled_masks(&self) -> &[usize] {
        &self.enabled
    }
    /// Possible diagnostics across the finite states. The final selected
    /// context is evaluated separately before user-visible diagnostics.
    pub(crate) fn possible_warnings(&self) -> &[PythonMarkerWarning] {
        &self.warnings
    }
}
impl std::fmt::Debug for MarkerExtraTruth {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("MarkerExtraTruth")
            .field("semantics", &self.semantics)
            .field("extra_count", &self.names.len())
            .field("enabled_count", &self.enabled.len())
            .finish_non_exhaustive()
    }
}
pub(crate) fn compile_extra_truth(
    expression: &PythonMarkerExpression,
    environment: PythonMarkerEnvironment<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    limits: ExtraTruthLimits,
) -> Result<MarkerExtraTruth, ExtraTruthError> {
    if [
        limits.max_relevant_extras,
        limits.max_truth_states,
        limits.max_total_evaluation_nodes,
        limits.max_name_bytes,
    ]
    .contains(&0)
    {
        return Err(ExtraTruthError::InvalidLimits);
    }
    if expression.node_count() > limits.max_total_evaluation_nodes {
        return Err(ExtraTruthError::Limit);
    }
    // The actual evaluator validates every comparison and explicit context.
    // Reuse this result for mask zero rather than evaluating it twice.
    let baseline = evaluate_python_marker(
        expression,
        environment,
        PythonMarkerScope::Dependency {
            requested_extras: &[],
        },
        semantics,
        marker_limits,
    )
    .map_err(ExtraTruthError::Marker)?;
    let mut names = BTreeSet::new();
    let mut name_bytes = 0usize;
    for (index, comparison) in expression.comparisons().enumerate() {
        if comparison.operator() != PythonMarkerOperator::Equal {
            continue;
        }
        let literal = match (comparison.left(), comparison.right()) {
            (
                PythonMarkerOperand::Field(PythonMarkerField::Extra),
                PythonMarkerOperand::Literal(value),
            )
            | (
                PythonMarkerOperand::Literal(value),
                PythonMarkerOperand::Field(PythonMarkerField::Extra),
            ) => value,
            _ => continue,
        };
        let name = NormalizedPythonName::new(literal).map_err(|_| {
            ExtraTruthError::Marker(PythonMarkerError::InvalidExtraName { comparison: index })
        })?;
        if !names.contains(name.as_str()) {
            if names.len() >= limits.max_relevant_extras {
                return Err(ExtraTruthError::Limit);
            }
            name_bytes = name_bytes
                .checked_add(name.as_str().len())
                .filter(|n| *n <= limits.max_name_bytes)
                .ok_or(ExtraTruthError::Limit)?;
            names.insert(name.as_str().to_owned());
        }
    }
    let names: Vec<String> = names.into_iter().collect();
    let shift = u32::try_from(names.len()).map_err(|_| ExtraTruthError::Limit)?;
    let states = 1usize
        .checked_shl(shift)
        .filter(|n| *n <= limits.max_truth_states)
        .ok_or(ExtraTruthError::Limit)?;
    states
        .checked_mul(expression.node_count())
        .filter(|n| *n <= limits.max_total_evaluation_nodes)
        .ok_or(ExtraTruthError::Limit)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(states)
        .map_err(|_| ExtraTruthError::Limit)?;
    let mut warnings = BTreeMap::new();
    for mask in 0..states {
        let selected: Vec<&str> = names
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1usize << bit) != 0)
            .map(|(_, name)| name.as_str())
            .collect();
        let evaluated = if mask == 0 {
            None
        } else {
            Some(
                evaluate_python_marker(
                    expression,
                    environment,
                    PythonMarkerScope::Dependency {
                        requested_extras: &selected,
                    },
                    semantics,
                    marker_limits,
                )
                .map_err(ExtraTruthError::Marker)?,
            )
        };
        let evaluated = evaluated.as_ref().unwrap_or(&baseline);
        values.push(evaluated.value());
        for warning in evaluated.warnings() {
            warnings.insert((warning.comparison_index, warning.kind as u8), *warning);
        }
    }
    // Feature constraints may add requests. An input table must never turn an
    // already enabled dependency off when the requested set grows.
    for mask in 0..states {
        if values[mask] {
            for bit in 0..names.len() {
                if !values[mask | (1usize << bit)] {
                    return Err(ExtraTruthError::NonMonotone);
                }
            }
        }
    }
    let enabled = values
        .into_iter()
        .enumerate()
        .filter_map(|(mask, value)| value.then_some(mask))
        .collect();
    Ok(MarkerExtraTruth {
        semantics,
        names,
        enabled,
        warnings: warnings.into_values().collect(),
    })
}

#[cfg(test)]
mod tests;
