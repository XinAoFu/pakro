//! Raw metadata and shared declaration budgets before owned facts or parsing.

use super::*;

fn exceeded(kind: PythonMetadataLimitKind) -> PythonMetadataError {
    PythonMetadataError::LimitExceeded(kind)
}
fn invalid(index: usize) -> PythonMetadataError {
    PythonMetadataError::InvalidField {
        field: PythonMetadataField::RequiresDist,
        index: Some(index),
    }
}
pub(super) fn inputs(
    fields: PythonMetadataFields<'_>,
    expected: &PythonMetadataExpectation<'_>,
    limits: PythonMetadataLimits,
) -> PythonMetadataResult<()> {
    if [
        limits.max_total_bytes,
        limits.max_field_bytes,
        limits.max_requirements,
        limits.max_provided_extras,
        limits.max_requested_extras,
        limits.max_dynamic_fields,
        limits.max_marker_depth,
        limits.max_marker_terms,
        limits.max_syntax_tokens,
    ]
    .contains(&0)
    {
        return Err(PythonMetadataError::InvalidLimits);
    }
    for (actual, maximum, kind) in [
        (
            fields.requires_dist.len(),
            limits.max_requirements,
            PythonMetadataLimitKind::Requirements,
        ),
        (
            fields.provides_extra.len(),
            limits.max_provided_extras,
            PythonMetadataLimitKind::ProvidedExtras,
        ),
        (
            fields.dynamic.len(),
            limits.max_dynamic_fields,
            PythonMetadataLimitKind::DynamicFields,
        ),
    ] {
        if actual > maximum {
            return Err(exceeded(kind));
        }
    }
    if expected.name.as_str().len() > limits.max_field_bytes
        || expected.version.len() > limits.max_field_bytes
    {
        return Err(exceeded(PythonMetadataLimitKind::FieldBytes));
    }
    let mut total = 0usize;
    let scalars = [fields.name, fields.version];
    for value in scalars
        .into_iter()
        .chain(fields.requires_python)
        .chain(fields.requires_dist.iter().map(String::as_str))
        .chain(fields.provides_extra.iter().map(String::as_str))
        .chain(fields.dynamic.iter().map(String::as_str))
    {
        if value.len() > limits.max_field_bytes {
            return Err(exceeded(PythonMetadataLimitKind::FieldBytes));
        }
        total = total
            .checked_add(value.len())
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or_else(|| exceeded(PythonMetadataLimitKind::TotalBytes))?;
    }
    Ok(())
}
pub(super) fn declaration_limits(
    limits: PythonMetadataLimits,
) -> PythonMetadataResult<crate::python_requirements::PythonRequirementLimits> {
    let max_nodes = limits
        .max_marker_terms
        .checked_mul(2)
        .and_then(|value| value.checked_add(limits.max_marker_depth))
        .ok_or_else(|| exceeded(PythonMetadataLimitKind::MarkerTerms))?;
    Ok(crate::python_requirements::PythonRequirementLimits {
        max_input_bytes: limits.max_field_bytes,
        max_requested_extras: limits.max_requested_extras,
        max_syntax_tokens: limits.max_syntax_tokens,
        max_marker_comparisons: limits.max_marker_terms,
        marker: crate::python_markers::PythonMarkerLimits {
            max_input_bytes: limits.max_field_bytes,
            max_literal_bytes: limits.max_field_bytes,
            max_tokens: limits.max_syntax_tokens,
            max_nodes,
            max_group_depth: limits.max_marker_depth,
            ..Default::default()
        },
    })
}

pub(super) fn declaration_error(
    error: crate::python_requirements::PythonRequirementError,
    index: usize,
) -> PythonMetadataError {
    use crate::python_markers::{
        PythonMarkerError as MarkerError, PythonMarkerLimitKind as MarkerLimit,
    };
    use crate::python_requirements::{
        PythonRequirementError as Error, PythonRequirementLimitKind as Limit,
    };
    match error {
        Error::InvalidLimits | Error::Marker(MarkerError::InvalidLimits) => {
            PythonMetadataError::InvalidLimits
        }
        Error::LimitExceeded(Limit::InputBytes) => exceeded(PythonMetadataLimitKind::FieldBytes),
        Error::LimitExceeded(Limit::RequestedExtras) => {
            exceeded(PythonMetadataLimitKind::RequestedExtras)
        }
        Error::LimitExceeded(Limit::SyntaxTokens) => {
            exceeded(PythonMetadataLimitKind::SyntaxTokens)
        }
        Error::LimitExceeded(Limit::MarkerComparisons) => {
            exceeded(PythonMetadataLimitKind::MarkerTerms)
        }
        Error::Marker(MarkerError::LimitExceeded(MarkerLimit::GroupDepth)) => {
            exceeded(PythonMetadataLimitKind::MarkerDepth)
        }
        Error::Marker(MarkerError::LimitExceeded(
            MarkerLimit::InputBytes | MarkerLimit::LiteralBytes,
        )) => exceeded(PythonMetadataLimitKind::FieldBytes),
        Error::Marker(MarkerError::LimitExceeded(MarkerLimit::Tokens)) => {
            exceeded(PythonMetadataLimitKind::SyntaxTokens)
        }
        Error::Marker(MarkerError::LimitExceeded(
            MarkerLimit::Nodes | MarkerLimit::Comparisons,
        )) => exceeded(PythonMetadataLimitKind::MarkerTerms),
        _ => invalid(index),
    }
}

/// Validate all declaration budgets before collecting any metadata facts. The
/// shared span scanner and marker lexer construct no expression or BDD here.
pub(super) fn requirement(
    raw: &str,
    index: usize,
    limits: PythonMetadataLimits,
) -> PythonMetadataResult<bool> {
    crate::python_requirements::preflight_python_requirement(raw, declaration_limits(limits)?)
        .map(|spans| spans.marker().is_some())
        .map_err(|error| declaration_error(error, index))
}
