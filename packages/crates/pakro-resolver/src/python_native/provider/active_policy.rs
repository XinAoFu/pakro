//! Original-root closure and bounded policy retries around the same PubGrub.

use super::*;
use crate::python_native::policy::*;
use std::collections::VecDeque;

#[derive(Default)]
struct PolicyUsage {
    rounds: usize,
    terms: usize,
    references: usize,
    propagation: usize,
}
struct ActiveNode {
    index: usize,
    extras: BTreeSet<String>,
}
struct ActiveGraph<'a> {
    nodes: BTreeMap<String, ActiveNode>,
    constraints: BTreeMap<String, Vec<ActiveVersionConstraint<'a>>>,
    origins: BTreeSet<ActiveRequirementOrigin>,
    queue: VecDeque<String>,
    queued: BTreeSet<String>,
}
impl<'a> ActiveGraph<'a> {
    fn activate(
        &mut self,
        provider: &Provider<'a>,
        selected: &BTreeMap<String, usize>,
        request: &'a Requirement,
        specifier: Option<&RawVersionConstraint>,
        origin: ActiveRequirementOrigin,
        usage: &mut PolicyUsage,
        limits: NativePolicyLimits,
    ) -> Result<(), Stop> {
        charge(&mut usage.propagation, 1, limits.max_propagation_steps)?;
        let raw = match (&request.constraint, specifier) {
            (RequirementConstraint::Version(raw), Some(spec)) => {
                let index = selected.get(&request.name).ok_or(Stop::Invalid)?;
                let candidate = provider.candidate(&request.name, *index)?;
                if !spec
                    .matches(&candidate.version)
                    .map_err(raw_constraint_error)?
                {
                    return Err(Stop::Invalid);
                }
                raw.as_str()
            }
            (RequirementConstraint::SourceReference, None) => {
                let ActiveRequirementOrigin::Dependency {
                    parent,
                    declaration,
                } = origin
                else {
                    return Err(Stop::Invalid);
                };
                return Err(Stop::Source {
                    parent,
                    declaration,
                });
            }
            _ => return Err(Stop::Invalid),
        };
        if !self.origins.contains(&origin) {
            charge(&mut usage.references, 1, limits.max_active_references)?;
            self.origins.insert(origin);
            let constraints = self.constraints.entry(request.name.clone()).or_default();
            constraints.try_reserve(1).map_err(|_| Stop::Limit)?;
            constraints.push(ActiveVersionConstraint { origin, raw });
        }
        let index = *selected.get(&request.name).ok_or(Stop::Invalid)?;
        let mut changed = !self.nodes.contains_key(&request.name);
        let node = self
            .nodes
            .entry(request.name.clone())
            .or_insert_with(|| ActiveNode {
                index,
                extras: BTreeSet::new(),
            });
        if node.index != index {
            return Err(Stop::Invalid);
        }
        for extra in &request.requested_extras {
            charge(&mut usage.propagation, 1, limits.max_propagation_steps)?;
            changed |= node.extras.insert(extra.clone());
        }
        if changed && self.queued.insert(request.name.clone()) {
            self.queue.push_back(request.name.clone());
        }
        Ok(())
    }
}

fn derive_active<'a>(
    provider: &Provider<'a>,
    selected: &BTreeMap<String, usize>,
    usage: &mut PolicyUsage,
    limits: NativePolicyLimits,
) -> Result<ActiveGraph<'a>, Stop> {
    let mut graph = ActiveGraph {
        nodes: BTreeMap::new(),
        constraints: BTreeMap::new(),
        origins: BTreeSet::new(),
        queue: VecDeque::new(),
        queued: BTreeSet::new(),
    };
    for (index, (root, spec)) in provider
        .requests
        .iter()
        .zip(provider.root_specs.borrow().iter())
        .enumerate()
    {
        graph.activate(
            provider,
            selected,
            root,
            Some(spec),
            ActiveRequirementOrigin::Root { declaration: index },
            usage,
            limits,
        )?;
    }
    while let Some(name) = graph.queue.pop_front() {
        provider.tick()?;
        graph.queued.remove(&name);
        charge(&mut usage.propagation, 1, limits.max_propagation_steps)?;
        let node = graph.nodes.get(&name).ok_or(Stop::Invalid)?;
        let key = provider.candidate(&name, node.index)?.key;
        let metadata = provider.metadata(key)?;
        let data: &'a CandidateMetadata = metadata.data;
        let extras = node.extras.clone();
        for (index, dependency) in data.dependencies.iter().enumerate() {
            charge(&mut usage.propagation, 1, limits.max_propagation_steps)?;
            let active = match (&dependency.extra_truth, &metadata.truths[index]) {
                (None, None) => true,
                (Some(truth), Some(view)) => {
                    let mut mask = 0usize;
                    for (bit, extra) in truth.extra_names.iter().enumerate() {
                        charge(&mut usage.propagation, 1, limits.max_propagation_steps)?;
                        if extras.contains(extra) {
                            mask |= 1usize << bit;
                        }
                    }
                    *view.enabled.get(mask).ok_or(Stop::Invalid)?
                }
                _ => return Err(Stop::Invalid),
            };
            if active {
                graph.activate(
                    provider,
                    selected,
                    &dependency.requirement,
                    metadata.specs[index].as_ref(),
                    ActiveRequirementOrigin::Dependency {
                        parent: key,
                        declaration: index,
                    },
                    usage,
                    limits,
                )?;
            }
        }
    }
    // The LFP may encounter a parent before another path supplies a tighter
    // constraint. Expose a deterministic original declaration order to policy.
    for constraints in graph.constraints.values_mut() {
        constraints.sort_by_key(|value| value.origin);
    }
    Ok(graph)
}

pub(super) fn run(
    requests: &[Requirement],
    depot: &dyn MetadataDepot,
    policy: &dyn ActiveCandidatePolicy,
    solver_limits: NativeSolveLimits,
    limits: NativePolicyLimits,
) -> Result<SolveStep, NativeSolveError> {
    limits.validate()?;
    let mut provider = make_provider(requests, depot, solver_limits)?;
    provider.policy_mode = true;
    if requests.is_empty() {
        return Ok(SolveStep::Complete(NativePlan { selected: vec![] }));
    }
    let mut usage = PolicyUsage::default();
    loop {
        charge(&mut usage.rounds, 1, limits.max_rounds).map_err(|_| NativeSolveError::Limit)?;
        let resolved = pubgrub::resolve(&provider, Package::Root, 0usize);
        let assignment = match resolved {
            Ok(assignment) => assignment,
            Err(PubGrubError::NoSolution(_)) => return Err(NativeSolveError::NoSolution),
            Err(
                PubGrubError::ErrorRetrievingDependencies { source, .. }
                | PubGrubError::ErrorChoosingVersion { source, .. }
                | PubGrubError::ErrorInShouldCancel(source),
            ) => return stopped(source),
        };
        let selected = assignment
            .into_iter()
            .filter_map(|(package, index)| match package {
                Package::Distribution(name) => Some((name, index)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let graph = match derive_active(&provider, &selected, &mut usage, limits) {
            Ok(graph) => graph,
            Err(stop) => return stopped(stop),
        };
        let mut rejected = None;
        let mut retry_preference = None;
        for (position, (name, node)) in graph.nodes.iter().enumerate() {
            if let Err(stop) = provider.tick() {
                return stopped(stop);
            }
            let project = match provider.project(name) {
                Ok(project) => project,
                Err(stop) => return stopped(stop),
            };
            let mut candidates = Vec::new();
            candidates
                .try_reserve_exact(project.len())
                .map_err(|_| NativeSolveError::Limit)?;
            for candidate in project {
                if let Err(stop) = provider.tick() {
                    return stopped(stop);
                }
                let metadata = if depot.metadata(candidate.key).is_some() {
                    match provider.metadata(candidate.key) {
                        Ok(view) => Some(view.data),
                        Err(stop) => return stopped(stop),
                    }
                } else {
                    None
                };
                candidates.push(PolicyCandidate {
                    candidate,
                    metadata,
                });
            }
            let constraints = graph
                .constraints
                .get(name)
                .ok_or(NativeSolveError::InvalidInput)?;
            let selected = provider
                .candidate(name, node.index)
                .map_err(|_| NativeSolveError::InvalidInput)?
                .key;
            match policy.evaluate(ActivePolicyQuery {
                name,
                selected,
                constraints,
                candidates: &candidates,
            }) {
                Ok(ActivePolicyDecision::Allow) => {}
                Ok(ActivePolicyDecision::Reject) => {
                    rejected = Some(position);
                    break;
                }
                Ok(ActivePolicyDecision::RejectWithAlternative(key)) => {
                    if key == selected {
                        return Err(NativeSolveError::InvalidInput);
                    }
                    let mut alternative = None;
                    for (index, candidate) in candidates.iter().enumerate() {
                        provider.tick().map_err(|_| NativeSolveError::Limit)?;
                        if candidate.candidate.key == key {
                            if candidate.metadata.is_none_or(|data| data.rejection.is_some()) {
                                return Err(NativeSolveError::InvalidInput);
                            }
                            alternative = Some(index);
                            break;
                        }
                    }
                    let alternative = alternative.ok_or(NativeSolveError::InvalidInput)?;
                    provider.text(name).map_err(|_| NativeSolveError::Limit)?;
                    retry_preference = Some((name.clone(), alternative));
                    rejected = Some(position);
                    break;
                }
                Ok(ActivePolicyDecision::NeedMetadata(key)) => {
                    if !candidates.iter().any(|candidate| {
                        candidate.candidate.key == key && candidate.metadata.is_none()
                    }) {
                        return Err(NativeSolveError::InvalidInput);
                    }
                    return Ok(SolveStep::NeedMetadata(key));
                }
                Err(ActivePolicyError::InvalidInput) => return Err(NativeSolveError::InvalidInput),
                Err(ActivePolicyError::Limit) => return Err(NativeSolveError::Limit),
            }
        }
        if let Some(rejected_position) = rejected {
            charge(&mut usage.terms, graph.nodes.len(), limits.max_nogood_terms)
                .map_err(|_| NativeSolveError::Limit)?;
            let mut terms = Vec::new();
            terms
                .try_reserve_exact(graph.nodes.len())
                .map_err(|_| NativeSolveError::Limit)?;
            for (name, node) in graph.nodes {
                provider.text(&name).map_err(|_| NativeSolveError::Limit)?;
                terms.push((name, node.index));
            }
            // A witness may change any node: a different parent can make the
            // rejected candidate legal. Try the actual rejected node first so
            // transitive, unrelated file alternatives do not trigger builds
            // before the policy offender is repaired. The complete disjunction
            // and every fallback witness remain unchanged.
            terms[..=rejected_position].rotate_right(1);
            if terms.is_empty() || provider.nogoods.borrow().contains(&terms) {
                return Err(NativeSolveError::InvalidInput);
            }
            provider
                .nogoods
                .borrow_mut()
                .try_reserve(1)
                .map_err(|_| NativeSolveError::Limit)?;
            // Advice for other projects remains useful while this immutable
            // input snapshot retries another policy rejection. It never
            // changes a range, and every assignment is evaluated again.
            if let Some((name, index)) = retry_preference {
                let _ = provider.retry_preferences.borrow_mut().insert(name, index);
            } else {
                let _ = provider.retry_preferences.borrow_mut().remove(&terms[0].0);
            }
            provider.nogoods.borrow_mut().push(terms);
            continue;
        }
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(graph.nodes.len())
            .map_err(|_| NativeSolveError::Limit)?;
        for (name, node) in graph.nodes {
            let candidate = provider
                .candidate(&name, node.index)
                .map_err(|_| NativeSolveError::InvalidInput)?;
            nodes.push(SelectedCandidate {
                name,
                key: candidate.key,
                version: candidate.version.clone(),
                requested_extras: node.extras.into_iter().collect(),
            });
        }
        return Ok(SolveStep::Complete(NativePlan { selected: nodes }));
    }
}
