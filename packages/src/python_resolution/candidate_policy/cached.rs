//! Static candidate facts owned by one immutable registry project entry.
//! This cache is descriptive and grants no source or installation authority.
use super::*;

struct CachedCandidate {
    file_index: usize,
    raw_version: String,
    version: Version,
    yanked: bool,
}
pub(crate) struct CachedCandidateUniverse {
    candidates: Vec<CachedCandidate>,
    visible: Vec<usize>,
    prereleases: Prereleases,
    yanked: Yanked,
    installed_version: Option<Version>,
    input_files: usize,
    supported_tags: usize,
    max_static_field: usize,
}
impl CachedCandidateUniverse {
    pub(crate) fn len(&self) -> usize {
        self.visible.len()
    }
    pub(crate) fn file_index(&self, position: usize) -> Option<usize> {
        self.visible
            .get(position)
            .and_then(|index| self.candidates.get(*index))
            .map(|candidate| candidate.file_index)
    }
}
pub(crate) fn classify_candidate_universe<'a>(
    project: &'a SimpleProject,
    supported: &SupportedWheelTags,
    python: &PythonMarkerContext,
    policy: CandidatePolicy<'_>,
    limits: CandidatePolicyLimits,
    usage: &mut CandidatePolicyUsage,
) -> Result<(OrderedCandidates<'a>, CachedCandidateUniverse), CandidatePolicyError> {
    // Preserve all statically compatible facts before unconditional policy
    // filters. An otherwise hidden prerelease can still affect the old
    // yanked-before-prerelease gate when metadata is unknown.
    let mut ordered = order_fresh_candidate_universe(
        project,
        supported,
        python,
        CandidatePolicy {
            prereleases: Prereleases::Allow,
            yanked: Yanked::ExactPinIfNoUnyanked,
            ..policy
        },
        limits,
        usage,
    )?;
    let mut budget = Budget {
        usage,
        limits,
        max_field: 0,
    };
    budget.work(ordered.candidates().len())?;
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(ordered.candidates().len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    for choice in ordered.candidates() {
        let raw = choice
            .file()
            .version()
            .ok_or(CandidatePolicyError::InvalidInput)?;
        budget.text(raw)?;
        candidates.push(CachedCandidate {
            file_index: choice.file_index(),
            raw_version: raw.into(),
            version: choice.version.clone(),
            yanked: choice.file().yanked().is_yanked(),
        });
    }
    let installed_version = policy
        .installed_version
        .map(Version::from_str)
        .transpose()
        .map_err(|_| CandidatePolicyError::InvalidInput)?;
    let max_static_field = ordered.max_field;
    let visible: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            ((!candidate.yanked || policy.yanked == Yanked::ExactPinIfNoUnyanked)
                && (!candidate.version.any_prerelease()
                    || policy.prereleases != Prereleases::Disallow))
                .then_some(index)
        })
        .collect();
    ordered.candidates.retain(|choice| {
        (!choice.file().yanked().is_yanked() || policy.yanked == Yanked::ExactPinIfNoUnyanked)
            && (!choice.version.any_prerelease() || policy.prereleases != Prereleases::Disallow)
    });
    Ok((
        ordered,
        CachedCandidateUniverse {
            candidates,
            visible,
            prereleases: policy.prereleases,
            yanked: policy.yanked,
            installed_version,
            input_files: project.files().len(),
            supported_tags: supported.ordered().len(),
            max_static_field,
        },
    ))
}

pub(crate) fn assess_cached_candidate(
    universe: &CachedCandidateUniverse,
    selected_position: usize,
    constraints: &[&str],
    knowledge: &[CandidateKnowledge],
    limits: CandidatePolicyLimits,
    usage: &mut CandidatePolicyUsage,
) -> Result<ActiveCandidateAssessment, CandidatePolicyError> {
    validate_limits(limits)?;
    if universe.input_files > limits.max_files
        || universe.supported_tags > limits.max_supported_tags
        || universe.max_static_field > limits.max_field_bytes
    {
        return Err(CandidatePolicyError::Limit);
    }
    if knowledge.len() != universe.len()
        || knowledge.get(selected_position) != Some(&CandidateKnowledge::Compatible)
    {
        return Err(CandidatePolicyError::InvalidInput);
    }
    let mut budget = Budget {
        usage,
        limits,
        max_field: 0,
    };
    let snapshot = parse_constraints(constraints, &mut budget)?;
    // One constraint pass, one knowledge pass and two three-pass conditional
    // reductions. Sorting and source/target classification were paid once.
    let work = universe
        .candidates
        .len()
        .checked_mul(
            snapshot
                .effort
                .checked_add(8)
                .ok_or(CandidatePolicyError::Limit)?,
        )
        .and_then(|n| n.checked_add(snapshot.effort))
        .ok_or(CandidatePolicyError::Limit)?;
    budget.work(work)?;
    let mut static_knowledge = Vec::new();
    static_knowledge
        .try_reserve_exact(universe.candidates.len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    static_knowledge.resize(universe.candidates.len(), CandidateKnowledge::Unknown);
    for (position, value) in knowledge.iter().enumerate() {
        static_knowledge[universe.visible[position]] = *value;
    }
    let knowledge = static_knowledge.as_slice();
    let selected_position = universe.visible[selected_position];
    let mut matches = Vec::new();
    matches
        .try_reserve_exact(universe.candidates.len())
        .map_err(|_| CandidatePolicyError::Limit)?;
    for candidate in &universe.candidates {
        let mut matched = true;
        for spec in &snapshot.parsed {
            matched &= spec
                .matches(&candidate.raw_version)
                .map_err(raw_constraint_error)?;
        }
        matches.push(matched);
    }
    let (allowed, notices, _, alternative) = reduce(
        universe,
        selected_position,
        &snapshot,
        knowledge,
        &matches,
        false,
    );
    if !allowed {
        return Ok(alternative.map_or(ActiveCandidateAssessment::Reject, |index| {
            ActiveCandidateAssessment::RejectWithAlternative(index)
        }));
    }
    if !knowledge
        .iter()
        .zip(&matches)
        .any(|(value, matched)| *matched && *value == CandidateKnowledge::Unknown)
    {
        return Ok(ActiveCandidateAssessment::Eligible(notices));
    }
    let (allowed, notices, blocker, _) = reduce(
        universe,
        selected_position,
        &snapshot,
        knowledge,
        &matches,
        true,
    );
    if allowed {
        return Ok(ActiveCandidateAssessment::Eligible(notices));
    }
    blocker
        .map(ActiveCandidateAssessment::NeedMetadata)
        .ok_or(CandidatePolicyError::InvalidInput)
}

fn reduce(
    universe: &CachedCandidateUniverse,
    selected: usize,
    snapshot: &ConstraintSnapshot,
    knowledge: &[CandidateKnowledge],
    matches: &[bool],
    include_unknown: bool,
) -> (bool, Vec<PolicyNotice>, Option<usize>, Option<usize>) {
    let eligible = |index: usize| {
        matches[index]
            && (knowledge[index] == CandidateKnowledge::Compatible
                || (include_unknown && knowledge[index] == CandidateKnowledge::Unknown))
    };
    let has_unyanked = universe
        .candidates
        .iter()
        .enumerate()
        .any(|(index, candidate)| eligible(index) && !candidate.yanked);
    let after_yanked = |index: usize, candidate: &CachedCandidate| {
        eligible(index)
            && (!candidate.yanked
                || (!has_unyanked
                    && snapshot.exact_pin
                    && universe.yanked == Yanked::ExactPinIfNoUnyanked))
    };
    let first_final = universe
        .candidates
        .iter()
        .enumerate()
        .find_map(|(index, candidate)| {
            (after_yanked(index, candidate) && !candidate.version.any_prerelease())
                .then_some(candidate.file_index)
        });
    let has_final = first_final.is_some();
    let allowed = |index: usize, candidate: &CachedCandidate| {
        after_yanked(index, candidate)
            && (!candidate.version.any_prerelease()
                || match universe.prereleases {
                    Prereleases::Allow => true,
                    Prereleases::Disallow => false,
                    Prereleases::IfNoMatchingFinal => {
                        snapshot.explicit_pre
                            || !has_final
                            || universe.installed_version.as_ref() == Some(&candidate.version)
                    }
                })
    };
    let candidate = &universe.candidates[selected];
    let selected_allowed = allowed(selected, candidate);
    // Reuse the same already-budgeted final scan. Advice is limited to a
    // known-only conditional prerelease rejection; unknown files and other
    // rejection categories never become compatibility or selection evidence.
    let alternative = if !include_unknown
        && !selected_allowed
        && matches[selected]
        && !candidate.yanked
        && candidate.version.any_prerelease()
        && universe.prereleases == Prereleases::IfNoMatchingFinal
    {
        first_final
    } else {
        None
    };
    let mut notices = Vec::new();
    if selected_allowed {
        if candidate.yanked {
            notices.push(PolicyNotice::YankedExactPin);
        }
        if candidate.version.any_prerelease()
            && universe.prereleases == Prereleases::IfNoMatchingFinal
            && !snapshot.explicit_pre
            && !has_final
            && universe.installed_version.as_ref() != Some(&candidate.version)
        {
            notices.push(PolicyNotice::PrereleaseFallback);
        }
    }
    let blocker = universe
        .candidates
        .iter()
        .enumerate()
        .find_map(|(index, candidate)| {
            (knowledge[index] == CandidateKnowledge::Unknown && allowed(index, candidate))
                .then_some(candidate.file_index)
        });
    (selected_allowed, notices, blocker, alternative)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{file, policy, project, python, target, wheel};
    use super::*;

    #[test]
    fn s4_cached_active_matches_uncached_policy_for_constraints_and_knowledge() {
        let mut yanked = wheel("2.0rc1");
        yanked["yanked"] = serde_json::json!(true);
        let mut yanked_final = wheel("2.0");
        yanked_final["yanked"] = serde_json::json!(true);
        let project = project(vec![wheel("3.0rc1"), yanked, yanked_final, wheel("1.0")]);
        let supported = target();
        let python = python();
        let constraints: &[&[&str]] = &[
            &[">=1"],
            &["==2.0rc1"],
            &[">=2.0rc1", "<3"],
            &["===1.0"],
            &["!=2.0"],
        ];
        let mut policy = policy();
        policy.yanked = Yanked::ExactPinIfNoUnyanked;
        for prereleases in [
            Prereleases::Allow,
            Prereleases::Disallow,
            Prereleases::IfNoMatchingFinal,
        ] {
            policy.prereleases = prereleases;
            let (ordered, cache) = classify_candidate_universe(
                &project,
                &supported,
                &python,
                policy,
                Default::default(),
                &mut CandidatePolicyUsage::default(),
            )
            .unwrap();
            assert_eq!(ordered.candidates().len(), cache.len());
            let count = cache.len();
            for selected in 0..count {
                for state in 0..3usize.pow(count as u32) {
                    let mut raw = state;
                    let mut knowledge = Vec::new();
                    for _ in 0..count {
                        knowledge.push(match raw % 3 {
                            0 => CandidateKnowledge::Unknown,
                            1 => CandidateKnowledge::Compatible,
                            _ => CandidateKnowledge::Incompatible,
                        });
                        raw /= 3;
                    }
                    knowledge[selected] = CandidateKnowledge::Compatible;
                    let mut all = vec![CandidateKnowledge::Unknown; project.files().len()];
                    for (position, value) in knowledge.iter().enumerate() {
                        all[cache.file_index(position).unwrap()] = *value;
                    }
                    for constraints in constraints {
                        let expected = assess_active_candidate(
                            ActiveCandidateContext {
                                project: &project,
                                supported: &supported,
                                python: &python,
                                policy,
                                limits: Default::default(),
                            },
                            cache.file_index(selected).unwrap(),
                            constraints,
                            &all,
                            &mut CandidatePolicyUsage::default(),
                        );
                        let actual = assess_cached_candidate(
                            &cache,
                            selected,
                            constraints,
                            &knowledge,
                            Default::default(),
                            &mut CandidatePolicyUsage::default(),
                        );
                        assert_eq!(
                            actual, expected,
                            "constraints={constraints:?}, selected={selected}, state={state}, policy={prereleases:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn s4_cached_active_charges_only_constraints_and_retains_cumulative_boundary() {
        let mut files = vec![wheel("2.0"), wheel("1.0")];
        files.extend(
            (30..62).map(|minor| file(&format!("demo_pkg-9.0-cp3{minor}-cp3{minor}-win32.whl"))),
        );
        let project = project(files);
        let supported = target();
        let python = python();
        let mut usage = CandidatePolicyUsage::default();
        let (_, cache) = classify_candidate_universe(
            &project,
            &supported,
            &python,
            policy(),
            Default::default(),
            &mut usage,
        )
        .unwrap();
        let initial = usage.work;
        let initial_text = usage.utf8;
        let knowledge = vec![CandidateKnowledge::Compatible; cache.len()];
        assess_cached_candidate(
            &cache,
            0,
            &[">=1"],
            &knowledge,
            Default::default(),
            &mut usage,
        )
        .unwrap();
        let delta = usage.work - initial;
        assert!(delta > 0);
        assert_eq!(cache.len(), 2);
        assert!(
            delta < project.files().len(),
            "off-target Unknown files do not enter active scans"
        );
        assert_eq!(
            usage.utf8 - initial_text,
            ">=1".len(),
            "static target/file facts must not be recharged on active reentry"
        );
        let limits = CandidatePolicyLimits {
            max_work_units: initial + 2 * delta,
            ..Default::default()
        };
        assess_cached_candidate(&cache, 0, &[">=1"], &knowledge, limits, &mut usage).unwrap();
        assert_eq!(
            assess_cached_candidate(&cache, 0, &[">=1"], &knowledge, limits, &mut usage)
                .unwrap_err(),
            CandidatePolicyError::Limit
        );
    }

    #[test]
    fn s4_cached_active_preserves_raw_equality_and_installed_prerelease_fallback() {
        let project = project(vec![wheel("02.0.0"), wheel("2.0rc1"), wheel("3.0")]);
        let supported = target();
        let python = python();
        let mut policy = policy();
        policy.installed_version = Some("2.0rc1");
        let (_, cache) = classify_candidate_universe(
            &project,
            &supported,
            &python,
            policy,
            Default::default(),
            &mut CandidatePolicyUsage::default(),
        )
        .unwrap();
        let knowledge = vec![CandidateKnowledge::Compatible; cache.len()];
        let mut all = vec![CandidateKnowledge::Unknown; project.files().len()];
        for position in 0..cache.len() {
            all[cache.file_index(position).unwrap()] = CandidateKnowledge::Compatible;
        }
        for constraints in [
            &["===02.0.0"][..],
            &["===2.0.0"][..],
            &["(>=2.0rc1,<4)"][..],
        ] {
            for position in 0..cache.len() {
                let old = assess_active_candidate(
                    ActiveCandidateContext {
                        project: &project,
                        supported: &supported,
                        python: &python,
                        policy,
                        limits: Default::default(),
                    },
                    cache.file_index(position).unwrap(),
                    constraints,
                    &all,
                    &mut CandidatePolicyUsage::default(),
                );
                let new = assess_cached_candidate(
                    &cache,
                    position,
                    constraints,
                    &knowledge,
                    Default::default(),
                    &mut CandidatePolicyUsage::default(),
                );
                assert_eq!(new, old, "constraints={constraints:?}, selected={position}");
            }
        }
    }

    #[test]
    fn s4_empty_constraints_cached_work_is_bounded() {
        let project = project(vec![wheel("2.0"), wheel("1.0")]);
        let supported = target();
        let python = python();
        for raw in ["", "  "] {
            let mut usage = CandidatePolicyUsage::default();
            let (_, cache) = classify_candidate_universe(
                &project,
                &supported,
                &python,
                policy(),
                Default::default(),
                &mut usage,
            )
            .unwrap();
            let constraints = vec![raw; 4096];
            let knowledge = vec![CandidateKnowledge::Compatible; cache.len()];
            assert_eq!(
                assess_cached_candidate(
                    &cache,
                    0,
                    &constraints,
                    &knowledge,
                    CandidatePolicyLimits {
                        max_work_units: 256,
                        ..Default::default()
                    },
                    &mut usage
                )
                .unwrap_err(),
                CandidatePolicyError::Limit
            );
        }
    }

    #[test]
    fn s4_empty_constraints_fresh_work_is_bounded() {
        let project = project(vec![wheel("2.0"), wheel("1.0")]);
        let supported = target();
        let python = python();
        for raw in ["", "  "] {
            let constraints = vec![raw; 4096];
            assert_eq!(
                order_fresh_candidates(
                    &project,
                    &constraints,
                    &supported,
                    &python,
                    policy(),
                    CandidatePolicyLimits {
                        max_work_units: 256,
                        ..Default::default()
                    }
                )
                .unwrap_err(),
                CandidatePolicyError::Limit
            );
        }
    }
}
