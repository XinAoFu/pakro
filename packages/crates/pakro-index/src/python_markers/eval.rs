//! Explicit Current2026 comparison semantics, without a host or global BDD.

use std::collections::BTreeSet;
use std::str::FromStr;

use pep440_rs::{Version, VersionSpecifier, VersionSpecifiers};

use crate::python_source::NormalizedPythonName;

use super::*;

struct Work {
    used: usize,
    max: usize,
}
impl Work {
    fn charge(&mut self, count: usize) -> PythonMarkerResult<()> {
        self.used = self
            .used
            .checked_add(count)
            .filter(|value| *value <= self.max)
            .ok_or(PythonMarkerError::LimitExceeded(
                PythonMarkerLimitKind::EvaluationSteps,
            ))?;
        Ok(())
    }
}

struct Sets {
    extra: BTreeSet<NormalizedPythonName>,
    extras: BTreeSet<NormalizedPythonName>,
    groups: BTreeSet<NormalizedPythonName>,
}

fn charge_environment(
    used: &mut usize,
    value: &str,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<()> {
    if value.len() > limits.max_environment_field_bytes {
        return Err(PythonMarkerError::LimitExceeded(
            PythonMarkerLimitKind::EnvironmentFieldBytes,
        ));
    }
    *used = used
        .checked_add(value.len())
        .filter(|value| *value <= limits.max_environment_bytes)
        .ok_or(PythonMarkerError::LimitExceeded(
            PythonMarkerLimitKind::EnvironmentBytes,
        ))?;
    Ok(())
}

fn names(
    values: &[&str],
    field: PythonMarkerField,
) -> PythonMarkerResult<BTreeSet<NormalizedPythonName>> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            NormalizedPythonName::new(value)
                .map_err(|_| PythonMarkerError::InvalidContextName { field, index })
        })
        .collect()
}

fn context_sets(
    environment: PythonMarkerEnvironment<'_>,
    scope: PythonMarkerScope<'_>,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<Sets> {
    let mut bytes = 0usize;
    for value in [
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
    ] {
        charge_environment(&mut bytes, value, limits)?;
    }
    let (extra, extras, groups) = match scope {
        PythonMarkerScope::Environment => (&[][..], &[][..], &[][..]),
        PythonMarkerScope::Dependency { requested_extras } => (requested_extras, &[][..], &[][..]),
        PythonMarkerScope::Lock {
            selected_extras,
            dependency_groups,
        } => (&[][..], selected_extras, dependency_groups),
    };
    let count = extra
        .len()
        .checked_add(extras.len())
        .and_then(|value| value.checked_add(groups.len()))
        .filter(|value| *value <= limits.max_set_entries)
        .ok_or(PythonMarkerError::LimitExceeded(
            PythonMarkerLimitKind::SetEntries,
        ))?;
    // Charge raw entries, including duplicates, before normalization or any
    // collection: a tiny distinct set cannot hide an oversized public input.
    for value in extra.iter().chain(extras).chain(groups) {
        charge_environment(&mut bytes, value, limits)?;
    }
    debug_assert!(count <= limits.max_set_entries);
    Ok(Sets {
        extra: names(extra, PythonMarkerField::Extra)?,
        extras: names(extras, PythonMarkerField::Extras)?,
        groups: names(groups, PythonMarkerField::DependencyGroups)?,
    })
}

fn field(operand: &Operand) -> Option<PythonMarkerField> {
    match operand {
        Operand::Field { field, .. } => Some(*field),
        Operand::Literal { .. } => None,
    }
}
fn availability(field: PythonMarkerField, scope: PythonMarkerScope<'_>) -> PythonMarkerResult<()> {
    let available = match field {
        PythonMarkerField::Extra => matches!(scope, PythonMarkerScope::Dependency { .. }),
        PythonMarkerField::Extras | PythonMarkerField::DependencyGroups => {
            matches!(scope, PythonMarkerScope::Lock { .. })
        }
        _ => true,
    };
    if available {
        Ok(())
    } else {
        Err(PythonMarkerError::ContextUnavailable(field))
    }
}
fn environment_value(
    field: PythonMarkerField,
    environment: PythonMarkerEnvironment<'_>,
) -> PythonMarkerResult<&str> {
    Ok(match field {
        PythonMarkerField::OsName => environment.os_name,
        PythonMarkerField::SysPlatform => environment.sys_platform,
        PythonMarkerField::PlatformRelease => environment.platform_release,
        PythonMarkerField::PlatformSystem => environment.platform_system,
        PythonMarkerField::PlatformVersion => environment.platform_version,
        PythonMarkerField::PlatformMachine => environment.platform_machine,
        PythonMarkerField::PlatformPythonImplementation => {
            environment.platform_python_implementation
        }
        PythonMarkerField::PythonVersion => environment.python_version,
        PythonMarkerField::PythonFullVersion => environment.python_full_version,
        PythonMarkerField::ImplementationName => environment.implementation_name,
        PythonMarkerField::ImplementationVersion => environment.implementation_version,
        special => return Err(PythonMarkerError::ContextUnavailable(special)),
    })
}
fn value<'a>(
    operand: &Operand,
    raw: &'a str,
    environment: PythonMarkerEnvironment<'a>,
) -> PythonMarkerResult<&'a str> {
    match operand {
        Operand::Field { field, .. } => environment_value(*field, environment),
        Operand::Literal { value_span, .. } => raw
            .get(value_span.clone())
            .ok_or(PythonMarkerError::Syntax { offset: 0 }),
    }
}

fn warning(
    warnings: &mut Vec<PythonMarkerWarning>,
    work: &mut Work,
    comparison_index: usize,
    kind: PythonMarkerWarningKind,
) -> PythonMarkerResult<()> {
    work.charge(1)?;
    warnings
        .try_reserve(1)
        .map_err(|_| PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::EvaluationSteps))?;
    warnings.push(PythonMarkerWarning {
        comparison_index,
        kind,
    });
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Category {
    String,
    VersionOrString,
    Version,
}
fn category(field: Option<PythonMarkerField>) -> Category {
    match field {
        Some(
            PythonMarkerField::PythonVersion
            | PythonMarkerField::PythonFullVersion
            | PythonMarkerField::ImplementationVersion,
        ) => Category::Version,
        Some(PythonMarkerField::PlatformRelease | PythonMarkerField::PlatformVersion) => {
            Category::VersionOrString
        }
        _ => Category::String,
    }
}

struct ComparisonEvaluator<'a> {
    raw: &'a str,
    environment: PythonMarkerEnvironment<'a>,
    sets: Sets,
    work: Work,
    warnings: Vec<PythonMarkerWarning>,
}
impl ComparisonEvaluator<'_> {
    fn warn(&mut self, index: usize, kind: PythonMarkerWarningKind) -> PythonMarkerResult<()> {
        warning(&mut self.warnings, &mut self.work, index, kind)
    }
    fn string(
        &mut self,
        left: &str,
        operator: PythonMarkerOperator,
        right: &str,
        index: usize,
    ) -> PythonMarkerResult<bool> {
        use PythonMarkerOperator as Op;
        Ok(match operator {
            Op::Equal => left == right,
            Op::NotEqual => left != right,
            Op::In => right.contains(left),
            Op::NotIn => !right.contains(left),
            Op::Less | Op::Greater => {
                self.warn(index, PythonMarkerWarningKind::StringOrderedComparison)?;
                false
            }
            Op::LessEqual | Op::GreaterEqual => {
                self.warn(index, PythonMarkerWarningKind::StringOrderedComparison)?;
                left == right
            }
            Op::Compatible | Op::ArbitraryEqual => {
                self.warn(index, PythonMarkerWarningKind::StringCompatibleEquality)?;
                left == right
            }
        })
    }
    fn compare(&mut self, comparison: &Comparison, index: usize) -> PythonMarkerResult<bool> {
        self.work.charge(2)?;
        let left_field = field(&comparison.left);
        let right_field = field(&comparison.right);
        let fields = [left_field, right_field];
        let operator = comparison.operator;
        if fields.iter().flatten().any(|field| {
            matches!(
                field,
                PythonMarkerField::Extras | PythonMarkerField::DependencyGroups
            )
        }) {
            let set = match right_field {
                Some(PythonMarkerField::Extras) => Some(&self.sets.extras),
                Some(PythonMarkerField::DependencyGroups) => Some(&self.sets.groups),
                _ => None,
            };
            if let (Some(set), Operand::Literal { value_span, .. }) = (set, &comparison.left)
                && matches!(
                    operator,
                    PythonMarkerOperator::In | PythonMarkerOperator::NotIn
                )
            {
                let literal = self
                    .raw
                    .get(value_span.clone())
                    .ok_or(PythonMarkerError::Syntax { offset: 0 })?;
                let name = NormalizedPythonName::new(literal)
                    .map_err(|_| PythonMarkerError::InvalidExtraName { comparison: index })?;
                let contains = set.contains(&name);
                return Ok(if operator == PythonMarkerOperator::In {
                    contains
                } else {
                    !contains
                });
            }
            self.warn(index, PythonMarkerWarningKind::UnsupportedSetComparison)?;
            return Ok(false);
        }
        if fields.contains(&Some(PythonMarkerField::Extra)) {
            let literal = match (&comparison.left, &comparison.right) {
                (
                    Operand::Field {
                        field: PythonMarkerField::Extra,
                        ..
                    },
                    Operand::Literal { value_span, .. },
                )
                | (
                    Operand::Literal { value_span, .. },
                    Operand::Field {
                        field: PythonMarkerField::Extra,
                        ..
                    },
                ) => Some(value_span),
                _ => None,
            };
            if operator == PythonMarkerOperator::Equal
                && let Some(span) = literal
            {
                let literal = self
                    .raw
                    .get(span.clone())
                    .ok_or(PythonMarkerError::Syntax { offset: 0 })?;
                let name = NormalizedPythonName::new(literal)
                    .map_err(|_| PythonMarkerError::InvalidExtraName { comparison: index })?;
                return Ok(self.sets.extra.contains(&name));
            }
            self.warn(index, PythonMarkerWarningKind::UnsupportedExtraComparison)?;
            return Ok(false);
        }
        if left_field.is_none() && right_field.is_none() {
            self.warn(index, PythonMarkerWarningKind::LiteralOnlyComparison)?;
        }
        if left_field.is_some() && right_field.is_some() {
            self.warn(index, PythonMarkerWarningKind::VariableVariableComparison)?;
        }
        let left = value(&comparison.left, self.raw, self.environment)?;
        let right = value(&comparison.right, self.raw, self.environment)?;
        let category = category(left_field).max(category(right_field));
        let containment = matches!(
            operator,
            PythonMarkerOperator::In | PythonMarkerOperator::NotIn
        );
        if category == Category::Version && containment {
            self.warn(index, PythonMarkerWarningKind::VersionContainment)?;
            return Ok(false);
        }
        if category != Category::String && !containment {
            if operator == PythonMarkerOperator::ArbitraryEqual {
                return Ok(left.eq_ignore_ascii_case(right));
            }
            let text = match operator {
                PythonMarkerOperator::Less => "<",
                PythonMarkerOperator::LessEqual => "<=",
                PythonMarkerOperator::NotEqual => "!=",
                PythonMarkerOperator::Equal => "==",
                PythonMarkerOperator::GreaterEqual => ">=",
                PythonMarkerOperator::Greater => ">",
                PythonMarkerOperator::Compatible => "~=",
                _ => {
                    return Err(PythonMarkerError::Syntax {
                        offset: comparison.operator_span.start,
                    });
                }
            };
            // Keep the original operand direction. PEP440's ~= and exclusive
            // clauses are not ordinary symmetric mathematical comparisons.
            let mut specifier = String::new();
            let size =
                text.len()
                    .checked_add(right.len())
                    .ok_or(PythonMarkerError::LimitExceeded(
                        PythonMarkerLimitKind::EnvironmentBytes,
                    ))?;
            specifier.try_reserve_exact(size).map_err(|_| {
                PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::EvaluationSteps)
            })?;
            specifier.push_str(text);
            specifier.push_str(right);
            if let (Ok(candidate), Ok(specifier)) = (
                Version::from_str(left),
                VersionSpecifier::from_str(&specifier),
            ) {
                let specifiers = std::iter::once(specifier).collect::<VersionSpecifiers>();
                return Ok(pakro_resolver::pyver::specifiers_contain(
                    &specifiers,
                    &candidate,
                ));
            }
            self.warn(index, PythonMarkerWarningKind::VersionStringFallback)?;
        }
        self.string(left, operator, right, index)
    }
}

fn check_usage(
    expression: &PythonMarkerExpression,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<()> {
    for (actual, max, kind) in [
        (
            expression.raw.len(),
            limits.max_input_bytes,
            PythonMarkerLimitKind::InputBytes,
        ),
        (
            expression.token_count,
            limits.max_tokens,
            PythonMarkerLimitKind::Tokens,
        ),
        (
            expression.nodes.len(),
            limits.max_nodes,
            PythonMarkerLimitKind::Nodes,
        ),
        (
            expression.group_depth,
            limits.max_group_depth,
            PythonMarkerLimitKind::GroupDepth,
        ),
    ] {
        if actual > max {
            return Err(PythonMarkerError::LimitExceeded(kind));
        }
    }
    for node in &expression.nodes {
        if let MarkerNode::Comparison(comparison) = node {
            for operand in [&comparison.left, &comparison.right] {
                if let Operand::Literal { value_span, .. } = operand
                    && value_span.len() > limits.max_literal_bytes
                {
                    return Err(PythonMarkerError::LimitExceeded(
                        PythonMarkerLimitKind::LiteralBytes,
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn evaluate(
    expression: &PythonMarkerExpression,
    environment: PythonMarkerEnvironment<'_>,
    scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    limits: PythonMarkerLimits,
) -> PythonMarkerResult<PythonMarkerEvaluation> {
    parse::validate_limits(limits)?;
    check_usage(expression, limits)?;
    let sets = context_sets(environment, scope, limits)?;
    // Check the complete expression before folding any boolean. An earlier
    // true/false operand cannot make an unavailable context become valid.
    for node in &expression.nodes {
        if let MarkerNode::Comparison(comparison) = node {
            for operand in [&comparison.left, &comparison.right] {
                if let Some(field) = field(operand) {
                    availability(field, scope)?;
                }
            }
        }
    }
    match semantics {
        MarkerSemantics::Current2026 => {}
    }
    let mut evaluation = ComparisonEvaluator {
        raw: &expression.raw,
        environment,
        sets,
        work: Work {
            used: 0,
            max: limits.max_evaluation_steps,
        },
        warnings: Vec::new(),
    };
    let mut values: Vec<bool> = Vec::new();
    values
        .try_reserve_exact(expression.nodes.len())
        .map_err(|_| PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::Nodes))?;
    let mut comparison_index = 0usize;
    for node in &expression.nodes {
        evaluation.work.charge(1)?;
        let value = match node {
            MarkerNode::Comparison(comparison) => {
                let value = evaluation.compare(comparison, comparison_index)?;
                comparison_index += 1;
                value
            }
            MarkerNode::And(left, right) | MarkerNode::Or(left, right) => {
                let left = *values
                    .get(*left)
                    .ok_or(PythonMarkerError::Syntax { offset: 0 })?;
                let right = *values
                    .get(*right)
                    .ok_or(PythonMarkerError::Syntax { offset: 0 })?;
                if matches!(node, MarkerNode::And(_, _)) {
                    left && right
                } else {
                    left || right
                }
            }
            MarkerNode::Group(child) => *values
                .get(*child)
                .ok_or(PythonMarkerError::Syntax { offset: 0 })?,
        };
        values.push(value);
    }
    let value = *values
        .get(expression.root)
        .ok_or(PythonMarkerError::Syntax { offset: 0 })?;
    Ok(PythonMarkerEvaluation {
        value,
        semantics,
        warnings: evaluation.warnings,
    })
}
