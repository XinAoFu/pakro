//! Explicit policy for one complete constraint snapshot. These ordered file
//! facts confer no source, installed-state, lock, graph or artifact authority.

use std::cmp::Ordering;
use std::str::FromStr;

use pakro_index::simple::{PythonDistributionKind, SimpleFile, SimpleProject};
use pakro_index::{
    PythonMarkerContext, SupportedWheelTags, WheelBuildTag, WheelTags,
    python_requires_matches_for_context,
};
use pakro_resolver::pyver::{
    RawVersionConstraint, RawVersionConstraintError, RawVersionConstraintLimits,
};
use pep440_rs::{Operator, Version};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prereleases {
    Allow,
    Disallow,
    IfNoMatchingFinal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Yanked {
    Disallow,
    ExactPinIfNoUnyanked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sdists {
    Include,
    Exclude,
}
/// No implicit default: the coordinator supplies the session's policy.
#[derive(Clone, Copy)]
pub(crate) struct CandidatePolicy<'a> {
    pub prereleases: Prereleases,
    pub yanked: Yanked,
    pub sdists: Sdists,
    /// Pure data. The coordinator establishes actual installed provenance;
    /// copying a version from a lock file does not establish that provenance.
    pub installed_version: Option<&'a str>,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct CandidatePolicyLimits {
    pub max_files: usize,
    pub max_constraints: usize,
    pub max_supported_tags: usize,
    pub max_field_bytes: usize,
    pub max_total_utf8_bytes: usize,
    pub max_work_units: usize,
}
impl Default for CandidatePolicyLimits {
    fn default() -> Self {
        Self {
            max_files: 65_536,
            max_constraints: 4_096,
            max_supported_tags: 65_536,
            max_field_bytes: 16_384,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            max_work_units: 16_777_216,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyNotice {
    PrereleaseFallback,
    YankedExactPin,
}
#[derive(Debug)]
pub(crate) struct CandidateChoice<'a> {
    file_index: usize,
    file: &'a SimpleFile,
    notices: Vec<PolicyNotice>,
    version: Version,
}
impl CandidateChoice<'_> {
    pub(crate) fn file_index(&self) -> usize {
        self.file_index
    }
    pub(crate) fn file(&self) -> &SimpleFile {
        self.file
    }
    pub(crate) fn notices(&self) -> &[PolicyNotice] {
        &self.notices
    }
}
#[derive(Debug)]
pub(crate) struct OrderedCandidates<'a> {
    project: &'a SimpleProject,
    candidates: Vec<CandidateChoice<'a>>,
    max_field: usize,
    // This one assessment's selected file passed static/constraint gates and
    // was rejected specifically by the conditional prerelease gate.
    selected_prerelease_rejected: bool,
}
impl OrderedCandidates<'_> {
    pub(crate) fn project(&self) -> &SimpleProject {
        self.project
    }
    pub(crate) fn candidates(&self) -> &[CandidateChoice<'_>] {
        &self.candidates
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum CandidatePolicyError {
    #[error("candidate policy limits must be positive")]
    InvalidLimits,
    #[error("candidate policy budget exceeded")]
    Limit,
    #[error("invalid candidate policy input")]
    InvalidInput,
}

#[derive(Default)]
pub(crate) struct CandidatePolicyUsage {
    utf8: usize,
    work: usize,
}
impl CandidatePolicyUsage {
    pub(crate) fn usage(&self) -> (usize, usize) {
        (self.work, self.utf8)
    }
}
struct Budget<'a> {
    usage: &'a mut CandidatePolicyUsage,
    limits: CandidatePolicyLimits,
    max_field: usize,
}
impl Budget<'_> {
    fn text(&mut self, value: &str) -> Result<(), CandidatePolicyError> {
        if value.len() > self.limits.max_field_bytes {
            return Err(CandidatePolicyError::Limit);
        }
        self.max_field = self.max_field.max(value.len());
        self.usage.utf8 = self
            .usage
            .utf8
            .checked_add(value.len())
            .filter(|size| *size <= self.limits.max_total_utf8_bytes)
            .ok_or(CandidatePolicyError::Limit)?;
        Ok(())
    }
    fn work(&mut self, count: usize) -> Result<(), CandidatePolicyError> {
        self.usage.work = self
            .usage
            .work
            .checked_add(count)
            .filter(|size| *size <= self.limits.max_work_units)
            .ok_or(CandidatePolicyError::Limit)?;
        Ok(())
    }
}
struct Candidate<'a> {
    file_index: usize,
    file: &'a SimpleFile,
    version: Version,
    rank: Option<usize>,
    tags: Option<&'a WheelTags>,
    build: Option<&'a WheelBuildTag>,
}
fn candidate_order(left: &Candidate<'_>, right: &Candidate<'_>) -> Ordering {
    right
        .version
        .cmp(&left.version)
        .then_with(|| left.tags.is_none().cmp(&right.tags.is_none()))
        .then_with(|| left.rank.cmp(&right.rank))
        .then_with(|| match (left.tags, right.tags) {
            (Some(left), Some(right)) => (&left.python, &left.abi, &left.platform).cmp(&(
                &right.python,
                &right.abi,
                &right.platform,
            )),
            _ => Ordering::Equal,
        })
        .then_with(|| right.build.cmp(&left.build))
        .then_with(|| left.file.filename().cmp(right.file.filename()))
}

fn raw_constraint_error(error: RawVersionConstraintError) -> CandidatePolicyError {
    match error {
        RawVersionConstraintError::Limit(_) => CandidatePolicyError::Limit,
        _ => CandidatePolicyError::InvalidInput,
    }
}

fn validate_limits(limits: CandidatePolicyLimits) -> Result<(), CandidatePolicyError> {
    if [
        limits.max_files,
        limits.max_constraints,
        limits.max_supported_tags,
        limits.max_field_bytes,
        limits.max_total_utf8_bytes,
        limits.max_work_units,
    ]
    .contains(&0)
    {
        return Err(CandidatePolicyError::InvalidLimits);
    }
    Ok(())
}

struct ConstraintSnapshot {
    parsed: Vec<RawVersionConstraint>,
    effort: usize,
    explicit_pre: bool,
    exact_pin: bool,
}
fn parse_constraints(
    constraints: &[&str],
    budget: &mut Budget<'_>,
) -> Result<ConstraintSnapshot, CandidatePolicyError> {
    let limits = budget.limits;
    if constraints.len() > limits.max_constraints {
        return Err(CandidatePolicyError::Limit);
    }
    let mut parsed = Vec::new();
    parsed
        .try_reserve_exact(constraints.len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    let mut clauses = 0usize;
    let mut effort = 0usize;
    let mut explicit_pre = false;
    let mut exact_pin = false;
    for constraint in constraints {
        budget.text(constraint)?;
        let spec = RawVersionConstraint::parse(
            constraint,
            RawVersionConstraintLimits {
                max_specifier_bytes: limits.max_field_bytes,
                max_clauses: limits.max_constraints,
                max_literal_bytes: limits.max_field_bytes,
                max_candidate_bytes: limits.max_field_bytes,
            },
        )
        .map_err(raw_constraint_error)?;
        clauses = clauses
            .checked_add(spec.clause_count())
            .filter(|count| *count <= limits.max_constraints)
            .ok_or(CandidatePolicyError::Limit)?;
        // Even an empty AST invokes matches for every candidate. Keep the
        // actual clause limit separate from this finite work envelope.
        effort = effort
            .checked_add(spec.clause_count().max(1))
            .ok_or(CandidatePolicyError::Limit)?;
        exact_pin |= spec.has_arbitrary_equality();
        explicit_pre |= spec.arbitrary_literals().any(|literal| {
            Version::from_str(literal).is_ok_and(|version| version.any_prerelease())
        });
        for clause in spec.ordinary().iter() {
            exact_pin |= matches!(clause.operator(), Operator::Equal | Operator::ExactEqual);
            explicit_pre |= !matches!(
                clause.operator(),
                Operator::NotEqual | Operator::NotEqualStar
            ) && clause.any_prerelease();
        }
        parsed.push(spec);
    }
    Ok(ConstraintSnapshot {
        parsed,
        effort,
        explicit_pre,
        exact_pin,
    })
}

/// This result is valid only for these explicit constraints. A resolver's
/// learned conflict range is not an interchangeable constraint snapshot.
pub(crate) fn order_fresh_candidates<'a>(
    project: &'a SimpleProject,
    constraints: &[&str],
    supported: &SupportedWheelTags,
    python: &PythonMarkerContext,
    policy: CandidatePolicy<'_>,
    limits: CandidatePolicyLimits,
) -> Result<OrderedCandidates<'a>, CandidatePolicyError> {
    order_fresh_candidates_with_usage(
        project,
        constraints,
        supported,
        python,
        policy,
        limits,
        &mut CandidatePolicyUsage::default(),
    )
}
pub(crate) fn order_fresh_candidates_with_usage<'a>(
    project: &'a SimpleProject,
    constraints: &[&str],
    supported: &SupportedWheelTags,
    python: &PythonMarkerContext,
    policy: CandidatePolicy<'_>,
    limits: CandidatePolicyLimits,
    usage: &mut CandidatePolicyUsage,
) -> Result<OrderedCandidates<'a>, CandidatePolicyError> {
    order_kernel(
        project,
        constraints,
        supported,
        python,
        policy,
        PolicyRun {
            limits,
            disabled: &[],
            defer_conditional: false,
            assessed_selected: None,
            usage,
        },
    )
}
struct PolicyRun<'a> {
    limits: CandidatePolicyLimits,
    disabled: &'a [bool],
    defer_conditional: bool,
    assessed_selected: Option<usize>,
    usage: &'a mut CandidatePolicyUsage,
}
fn order_kernel<'a>(
    project: &'a SimpleProject,
    constraints: &[&str],
    supported: &SupportedWheelTags,
    python: &PythonMarkerContext,
    policy: CandidatePolicy<'_>,
    run: PolicyRun<'_>,
) -> Result<OrderedCandidates<'a>, CandidatePolicyError> {
    let PolicyRun {
        limits,
        disabled,
        defer_conditional,
        assessed_selected,
        usage,
    } = run;
    if !disabled.is_empty() && disabled.len() != project.files().len() {
        return Err(CandidatePolicyError::InvalidInput);
    }
    validate_limits(limits)?;
    if project.files().len() > limits.max_files
        || constraints.len() > limits.max_constraints
        || supported.ordered().len() > limits.max_supported_tags
    {
        return Err(CandidatePolicyError::Limit);
    }
    let mut budget = Budget {
        usage,
        limits,
        max_field: 0,
    };
    budget.text(python.python_full_version())?;
    let installed = policy
        .installed_version
        .map(|version| {
            budget.text(version)?;
            Version::from_str(version).map_err(|_| CandidatePolicyError::InvalidInput)
        })
        .transpose()?;
    let ConstraintSnapshot {
        parsed,
        effort,
        explicit_pre,
        exact_pin,
    } = parse_constraints(constraints, &mut budget)?;
    budget.work(supported.ordered().len())?;
    for tag in supported.ordered() {
        for field in [tag.interpreter(), tag.abi(), tag.platform()] {
            budget.text(field)?;
        }
    }
    let count = project.files().len();
    let sort_height = if count <= 1 {
        0
    } else {
        (usize::BITS - (count - 1).leading_zeros()) as usize
    };
    let per_file = effort
        .checked_add(sort_height)
        .and_then(|cost| cost.checked_add(1))
        .ok_or(CandidatePolicyError::Limit)?;
    budget.work(
        count
            .checked_mul(per_file)
            .ok_or(CandidatePolicyError::Limit)?,
    )?;

    // Finish validating all file facts before selecting any of them. The
    // actual Simple parser owns structural validation; no locator is opened.
    let mut facts = Vec::new();
    facts
        .try_reserve_exact(count)
        .map_err(|_| CandidatePolicyError::Limit)?;
    for file in project.files() {
        budget.text(file.filename())?;
        let version = file
            .version()
            .map(|version| {
                budget.text(version)?;
                Version::from_str(version).map_err(|_| CandidatePolicyError::InvalidInput)
            })
            .transpose()?;
        let compatible = match file.requires_python() {
            Some(spec) => {
                budget.text(spec)?;
                python_requires_matches_for_context(spec, python)
                    .map_err(|_| CandidatePolicyError::InvalidInput)?
            }
            None => true,
        };
        facts.push((version, compatible));
    }
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(count)
        .map_err(|_| CandidatePolicyError::Limit)?;
    for (file_index, (file, (version, compatible))) in project.files().iter().zip(facts).enumerate()
    {
        let (rank, tags, build) = match file.kind() {
            PythonDistributionKind::Wheel(wheel) => {
                let rank = supported
                    .plan_rank(&wheel.tags)
                    .map_err(|_| CandidatePolicyError::InvalidInput)?;
                budget.work(rank.work_bound())?;
                let Some(rank) = rank.best_rank() else {
                    continue;
                };
                (Some(rank), Some(&wheel.tags), wheel.build.as_ref())
            }
            PythonDistributionKind::Sdist if policy.sdists == Sdists::Include => (None, None, None),
            _ => continue,
        };
        let version = version.ok_or(CandidatePolicyError::InvalidInput)?;
        let candidate_text = file.version().ok_or(CandidatePolicyError::InvalidInput)?;
        let mut matches = true;
        for spec in &parsed {
            matches &= spec.matches(candidate_text).map_err(raw_constraint_error)?;
        }
        if !compatible || !matches || disabled.get(file_index).copied().unwrap_or(false) {
            continue;
        }
        candidates.push(Candidate {
            file_index,
            file,
            version,
            rank,
            tags,
            build,
        });
    }
    let has_unyanked = candidates.iter().any(|c| !c.file.yanked().is_yanked());
    candidates.retain(|candidate| {
        !candidate.file.yanked().is_yanked()
            || ((defer_conditional || (!has_unyanked && exact_pin))
                && policy.yanked == Yanked::ExactPinIfNoUnyanked)
    });
    let has_final = candidates
        .iter()
        .any(|candidate| !candidate.version.any_prerelease());
    let mut selected_prerelease_rejected = false;
    candidates.retain(|candidate| {
        let allowed = !candidate.version.any_prerelease()
            || match policy.prereleases {
                Prereleases::Allow => true,
                Prereleases::Disallow => false,
                Prereleases::IfNoMatchingFinal => {
                    defer_conditional
                        || explicit_pre
                        || !has_final
                        || installed.as_ref() == Some(&candidate.version)
                }
            };
        selected_prerelease_rejected |= !allowed
            && policy.prereleases == Prereleases::IfNoMatchingFinal
            && !candidate.file.yanked().is_yanked()
            && assessed_selected == Some(candidate.file_index);
        allowed
    });
    // Grouping by canonical tags makes build comparison a total order.
    // Comparing build only for equal tags and filenames for other pairs can
    // otherwise create comparison cycles.
    candidates.sort_unstable_by(candidate_order);
    let mut choices = Vec::new();
    choices
        .try_reserve_exact(candidates.len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    for candidate in candidates {
        let mut notices = Vec::new();
        notices
            .try_reserve_exact(2)
            .map_err(|_| CandidatePolicyError::Limit)?;
        if !defer_conditional && candidate.file.yanked().is_yanked() {
            notices.push(PolicyNotice::YankedExactPin);
        }
        if !defer_conditional
            && candidate.version.any_prerelease()
            && policy.prereleases == Prereleases::IfNoMatchingFinal
            && !explicit_pre
            && !has_final
            && installed.as_ref() != Some(&candidate.version)
        {
            notices.push(PolicyNotice::PrereleaseFallback);
        }
        choices.push(CandidateChoice {
            file_index: candidate.file_index,
            file: candidate.file,
            notices,
            version: candidate.version,
        });
    }
    Ok(OrderedCandidates {
        project,
        candidates: choices,
        max_field: budget.max_field,
        selected_prerelease_rejected,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateKnowledge {
    Unknown,
    Compatible,
    Incompatible,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ActiveCandidateAssessment {
    Eligible(Vec<PolicyNotice>),
    Reject,
    /// An observed eligible final in this snapshot, identified by the
    /// original project file index. This is retry advice, never a pin.
    RejectWithAlternative(usize),
    NeedMetadata(usize),
}

#[path = "candidate_policy/cached.rs"]
mod cached;
pub(crate) use cached::{
    CachedCandidateUniverse, assess_cached_candidate, classify_candidate_universe,
    prefer_candidate_priorities,
};
pub(crate) struct ActiveCandidateContext<'a> {
    pub project: &'a SimpleProject,
    pub supported: &'a SupportedWheelTags,
    pub python: &'a PythonMarkerContext,
    pub policy: CandidatePolicy<'a>,
    pub limits: CandidatePolicyLimits,
}
pub(crate) fn order_fresh_candidate_universe<'a>(
    project: &'a SimpleProject,
    supported: &SupportedWheelTags,
    python: &PythonMarkerContext,
    policy: CandidatePolicy<'_>,
    limits: CandidatePolicyLimits,
    usage: &mut CandidatePolicyUsage,
) -> Result<OrderedCandidates<'a>, CandidatePolicyError> {
    order_kernel(
        project,
        &[],
        supported,
        python,
        policy,
        PolicyRun {
            limits,
            disabled: &[],
            defer_conditional: true,
            assessed_selected: None,
            usage,
        },
    )
}
pub(crate) fn assess_active_candidate(
    context: ActiveCandidateContext<'_>,
    selected: usize,
    constraints: &[&str],
    knowledge: &[CandidateKnowledge],
    usage: &mut CandidatePolicyUsage,
) -> Result<ActiveCandidateAssessment, CandidatePolicyError> {
    let ActiveCandidateContext {
        project,
        supported,
        python,
        policy,
        limits,
    } = context;
    if project.files().len() > limits.max_files {
        return Err(CandidatePolicyError::Limit);
    }
    if knowledge.len() != project.files().len()
        || knowledge.get(selected) != Some(&CandidateKnowledge::Compatible)
    {
        return Err(CandidatePolicyError::InvalidInput);
    }
    let mut disabled = Vec::new();
    disabled
        .try_reserve_exact(knowledge.len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    disabled.extend(
        knowledge
            .iter()
            .map(|value| *value != CandidateKnowledge::Compatible),
    );
    let optimistic = order_kernel(
        project,
        constraints,
        supported,
        python,
        policy,
        PolicyRun {
            limits,
            disabled: &disabled,
            defer_conditional: false,
            assessed_selected: Some(selected),
            usage,
        },
    )?;
    if optimistic.selected_prerelease_rejected {
        // The original selected-membership scan would visit every choice on
        // this rejection path. Use it to advise an already observed eligible
        // final, without re-running policy or treating the advice as a pin.
        return Ok(optimistic
            .candidates
            .into_iter()
            .find(|choice| !choice.version.any_prerelease())
            .map_or(ActiveCandidateAssessment::Reject, |choice| {
                ActiveCandidateAssessment::RejectWithAlternative(choice.file_index)
            }));
    }
    let Some(allowed) = optimistic
        .candidates
        .into_iter()
        .find(|choice| choice.file_index == selected)
    else {
        return Ok(ActiveCandidateAssessment::Reject);
    };
    if !knowledge.contains(&CandidateKnowledge::Unknown) {
        return Ok(ActiveCandidateAssessment::Eligible(allowed.notices));
    }
    for (disabled, value) in disabled.iter_mut().zip(knowledge) {
        *disabled = *value == CandidateKnowledge::Incompatible;
    }
    let pessimistic = order_kernel(
        project,
        constraints,
        supported,
        python,
        policy,
        PolicyRun {
            limits,
            disabled: &disabled,
            defer_conditional: false,
            assessed_selected: None,
            usage,
        },
    )?;
    if let Some(allowed) = pessimistic
        .candidates
        .iter()
        .find(|choice| choice.file_index == selected)
    {
        return Ok(ActiveCandidateAssessment::Eligible(allowed.notices.clone()));
    }
    let blocker = pessimistic
        .candidates
        .iter()
        .find(|choice| knowledge[choice.file_index] == CandidateKnowledge::Unknown)
        .ok_or(CandidatePolicyError::InvalidInput)?;
    Ok(ActiveCandidateAssessment::NeedMetadata(blocker.file_index))
}

#[cfg(test)]
#[path = "candidate_policy/tests.rs"]
mod tests;
