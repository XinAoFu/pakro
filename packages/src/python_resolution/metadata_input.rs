//! Pure solving inputs projected from exact Core Metadata declarations.
//! Source references remain unresolved; this never authorizes an artifact.

use super::marker_truth::{
    ExtraTruthError, ExtraTruthLimits, MarkerExtraTruth, compile_extra_truth,
};
use pakro_index::python_markers::{MarkerSemantics, PythonMarkerEnvironment, PythonMarkerLimits};
use pakro_index::python_metadata::{PythonDependencyMetadataFacts, SdistDynamicPolicy};
use pakro_index::{
    PythonMarkerContext, PythonMarkerContextBuilder, python_requires_matches_for_context,
};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub(crate) struct MetadataInputLimits {
    pub max_dependencies: usize,
    pub max_retained_utf8_bytes: usize,
    pub max_total_truth_states: usize,
    pub max_total_evaluation_nodes: usize,
}
impl Default for MetadataInputLimits {
    fn default() -> Self {
        Self {
            max_dependencies: 1024,
            max_retained_utf8_bytes: 4 * 1024 * 1024,
            max_total_truth_states: 65_536,
            max_total_evaluation_nodes: 4_194_304,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum MetadataInputError {
    #[error("metadata input limits must be positive")]
    InvalidLimits,
    #[error("metadata input budget exceeded")]
    Limit,
    #[error("explicit Python metadata context is inconsistent")]
    InvalidContext,
    #[error("actual Requires-Python condition is invalid")]
    InvalidRequiresPython,
    #[error("exact dependency conditions could not be compiled: {0}")]
    Truth(ExtraTruthError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonCompatibility {
    Compatible,
    RequiresPythonMismatch,
}
pub(crate) enum DependencyConstraint {
    Version(String),
    /// Still needs the actual URL/Git/path adapter; never an empty range.
    SourceReference(String),
}
impl std::fmt::Debug for DependencyConstraint {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::Version(_) => "Version(<validated specifiers>)",
            Self::SourceReference(_) => "SourceReference(<private unresolved token>)",
        })
    }
}
pub(crate) struct DependencyInput {
    pub name: String,
    pub constraint: DependencyConstraint,
    pub requested_extras: Vec<String>,
    pub condition: Option<MarkerExtraTruth>,
    pub original_declaration: String,
}
impl std::fmt::Debug for DependencyInput {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("DependencyInput")
            .field("name", &self.name)
            .field("constraint", &self.constraint)
            .field("extra_count", &self.requested_extras.len())
            .field("has_condition", &self.condition.is_some())
            .finish_non_exhaustive()
    }
}
pub(crate) struct MetadataSolveInput {
    pub dependencies: Vec<DependencyInput>,
    pub requires_python: Option<String>,
    pub dynamic_fields: Vec<String>,
    pub dynamic_policy: SdistDynamicPolicy,
    /// Data-only eligibility; no source, build or publication authority.
    pub python_compatibility: PythonCompatibility,
}
impl std::fmt::Debug for MetadataSolveInput {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("MetadataSolveInput")
            .field("dependencies", &self.dependencies)
            .field("has_requires_python", &self.requires_python.is_some())
            .field("dynamic_field_count", &self.dynamic_fields.len())
            .field("dynamic_policy", &self.dynamic_policy)
            .field("python_compatibility", &self.python_compatibility)
            .finish_non_exhaustive()
    }
}
struct Budget {
    remaining: usize,
}
impl Budget {
    fn charge(&mut self, count: usize) -> Result<(), MetadataInputError> {
        self.remaining = self
            .remaining
            .checked_sub(count)
            .ok_or(MetadataInputError::Limit)?;
        Ok(())
    }
    fn copy(&mut self, text: &str) -> Result<String, MetadataInputError> {
        self.charge(text.len())?;
        Ok(text.to_owned())
    }
}
pub(crate) fn checked_metadata_context(
    environment: PythonMarkerEnvironment<'_>,
    marker_limits: PythonMarkerLimits,
) -> Result<PythonMarkerContext, MetadataInputError> {
    marker_limits
        .validate()
        .map_err(|_| MetadataInputError::InvalidLimits)?;
    let fields = [
        environment.os_name,
        environment.sys_platform,
        environment.platform_release,
        environment.platform_system,
        environment.platform_version,
        environment.platform_machine,
        environment.platform_python_implementation,
        environment.python_version,
        environment.python_full_version,
        environment.implementation_name,
        environment.implementation_version,
    ];
    let mut environment_bytes = 0usize;
    for field in fields {
        if field.len() > marker_limits.max_environment_field_bytes {
            return Err(MetadataInputError::Limit);
        }
        environment_bytes = environment_bytes
            .checked_add(field.len())
            .filter(|n| *n <= marker_limits.max_environment_bytes)
            .ok_or(MetadataInputError::Limit)?;
    }
    let context = PythonMarkerContext::new(PythonMarkerContextBuilder {
        os_name: environment.os_name,
        sys_platform: environment.sys_platform,
        platform_release: environment.platform_release,
        platform_system: environment.platform_system,
        platform_version: environment.platform_version,
        platform_machine: environment.platform_machine,
        platform_python_implementation: environment.platform_python_implementation,
        python_version: environment.python_version,
        python_full_version: environment.python_full_version,
        implementation_name: environment.implementation_name,
        implementation_version: environment.implementation_version,
    })
    .map_err(|_| MetadataInputError::InvalidContext)?;
    Ok(context)
}
pub(crate) fn compile_metadata_input(
    facts: &PythonDependencyMetadataFacts,
    environment: PythonMarkerEnvironment<'_>,
    semantics: MarkerSemantics,
    marker_limits: PythonMarkerLimits,
    truth_limits: ExtraTruthLimits,
    limits: MetadataInputLimits,
) -> Result<MetadataSolveInput, MetadataInputError> {
    if [
        limits.max_dependencies,
        limits.max_retained_utf8_bytes,
        limits.max_total_truth_states,
        limits.max_total_evaluation_nodes,
    ]
    .contains(&0)
    {
        return Err(MetadataInputError::InvalidLimits);
    }
    if facts.requirements().len() > limits.max_dependencies {
        return Err(MetadataInputError::Limit);
    }
    let context = checked_metadata_context(environment, marker_limits)?;
    let python_compatibility = match facts.requires_python() {
        Some(spec)
            if !python_requires_matches_for_context(spec, &context)
                .map_err(|_| MetadataInputError::InvalidRequiresPython)? =>
        {
            PythonCompatibility::RequiresPythonMismatch
        }
        _ => PythonCompatibility::Compatible,
    };
    let mut bytes = Budget {
        remaining: limits.max_retained_utf8_bytes,
    };
    let requires_python = facts
        .requires_python()
        .map(|text| bytes.copy(text))
        .transpose()?;
    let dynamic_fields = facts
        .dynamic_fields()
        .iter()
        .map(|field| bytes.copy(field))
        .collect::<Result<Vec<_>, _>>()?;
    let mut states = Budget {
        remaining: limits.max_total_truth_states,
    };
    let mut nodes = Budget {
        remaining: limits.max_total_evaluation_nodes,
    };
    let mut dependencies = Vec::new();
    dependencies
        .try_reserve_exact(facts.requirements().len())
        .map_err(|_| MetadataInputError::Limit)?;
    for declaration in facts.requirements() {
        let name = bytes.copy(declaration.name())?;
        let original_declaration = bytes.copy(declaration.raw_for_semantic_resolution())?;
        let constraint = match declaration.unresolved_reference() {
            Some(reference) => DependencyConstraint::SourceReference(
                bytes.copy(reference.raw_for_source_binding())?,
            ),
            None => {
                let value = declaration.raw_version_specifiers().unwrap_or_default();
                DependencyConstraint::Version(bytes.copy(value)?)
            }
        };
        let requested_extras = declaration
            .requested_extras()
            .map(|extra| bytes.copy(extra))
            .collect::<Result<Vec<_>, _>>()?;
        let condition = match declaration.exact_marker() {
            None => None,
            Some(expression) => {
                if states.remaining == 0 || nodes.remaining == 0 {
                    return Err(MetadataInputError::Limit);
                }
                let per_truth = ExtraTruthLimits {
                    max_truth_states: truth_limits.max_truth_states.min(states.remaining),
                    max_total_evaluation_nodes: truth_limits
                        .max_total_evaluation_nodes
                        .min(nodes.remaining),
                    max_name_bytes: truth_limits.max_name_bytes.min(bytes.remaining.max(1)),
                    ..truth_limits
                };
                let truth = compile_extra_truth(
                    expression,
                    environment,
                    semantics,
                    marker_limits,
                    per_truth,
                )
                .map_err(|error| match error {
                    ExtraTruthError::Limit => MetadataInputError::Limit,
                    other => MetadataInputError::Truth(other),
                })?;
                let shift = u32::try_from(truth.extra_names().len())
                    .map_err(|_| MetadataInputError::Limit)?;
                let count = 1usize.checked_shl(shift).ok_or(MetadataInputError::Limit)?;
                states.charge(count)?;
                nodes.charge(
                    count
                        .checked_mul(expression.node_count())
                        .ok_or(MetadataInputError::Limit)?,
                )?;
                for extra in truth.extra_names() {
                    bytes.charge(extra.len())?;
                }
                Some(truth)
            }
        };
        dependencies.push(DependencyInput {
            name,
            constraint,
            requested_extras,
            condition,
            original_declaration,
        });
    }
    Ok(MetadataSolveInput {
        dependencies,
        requires_python,
        dynamic_fields,
        dynamic_policy: facts.sdist_dynamic_policy(),
        python_compatibility,
    })
}

#[cfg(test)]
mod tests;
