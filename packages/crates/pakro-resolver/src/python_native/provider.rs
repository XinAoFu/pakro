use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::rc::Rc;
use std::str::FromStr;

use crate::pyver::{RawVersionConstraint, RawVersionConstraintError, RawVersionConstraintLimits};

use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics,
    PubGrubError, Ranges,
};

#[path = "provider/active_policy.rs"]
mod active_policy;

type Constraints = DependencyConstraints<Package, Ranges<usize>>;
type ConstraintMap = BTreeMap<Package, Ranges<usize>>;
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Package {
    Root,
    Distribution(String),
    Feature(String, String),
    Guard(CandidateKey, usize),
    RejectedAssignment(usize),
}
impl fmt::Display for Package {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => out.write_str("Python requests"),
            Self::Distribution(name) => out.write_str(name),
            Self::Feature(name, extra) => write!(out, "{name}[{extra}]"),
            Self::Guard(_, _) => out.write_str("Python conditional dependency"),
            Self::RejectedAssignment(_) => out.write_str("Python candidate policy alternative"),
        }
    }
}
#[derive(Debug, Clone, Error)]
enum Stop {
    #[error("Python project input required")]
    Project(String),
    #[error("Python candidate metadata required")]
    Metadata(CandidateKey),
    #[error("invalid native Python solve input")]
    Invalid,
    #[error("native Python solve budget exceeded")]
    Limit,
    #[error("Python source adapter input required")]
    Source {
        parent: CandidateKey,
        declaration: usize,
    },
}
#[derive(Default)]
struct Usage {
    candidates: usize,
    dependencies: usize,
    utf8: usize,
    extra_references: usize,
    guard_states: usize,
    truth_work: usize,
}
struct TruthView {
    enabled: Vec<bool>,
    bit_ranges: Vec<Ranges<usize>>,
}
struct MetadataView<'a> {
    data: &'a CandidateMetadata,
    specs: Vec<Option<RawVersionConstraint>>,
    truths: Vec<Option<TruthView>>,
}
struct Provider<'a> {
    requests: &'a [Requirement],
    depot: &'a dyn MetadataDepot,
    limits: NativeSolveLimits,
    root_specs: RefCell<Vec<RawVersionConstraint>>,
    projects: RefCell<BTreeMap<String, &'a [CandidateVersion]>>,
    metadata: RefCell<BTreeMap<CandidateKey, Rc<MetadataView<'a>>>>,
    identities: RefCell<BTreeSet<CandidateKey>>,
    usage: RefCell<Usage>,
    calls: Cell<usize>,
    policy_mode: bool,
    nogoods: RefCell<Vec<Vec<(String, usize)>>>,
    retry_preferences: RefCell<BTreeMap<String, usize>>,
}
fn charge(value: &mut usize, count: usize, maximum: usize) -> Result<(), Stop> {
    *value = value
        .checked_add(count)
        .filter(|n| *n <= maximum)
        .ok_or(Stop::Limit)?;
    Ok(())
}
fn raw_constraint_error(error: RawVersionConstraintError) -> Stop {
    match error {
        RawVersionConstraintError::Limit(_) => Stop::Limit,
        _ => Stop::Invalid,
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.contains("--")
}
fn insert(map: &mut ConstraintMap, package: Package, range: Ranges<usize>) {
    map.entry(package)
        .and_modify(|current| *current = current.intersection(&range))
        .or_insert(range);
}
impl<'a> Provider<'a> {
    fn tick(&self) -> Result<(), Stop> {
        let n = self
            .calls
            .get()
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_solver_calls)
            .ok_or(Stop::Limit)?;
        self.calls.set(n);
        Ok(())
    }
    fn text(&self, text: &str) -> Result<(), Stop> {
        if text.len() > self.limits.max_field_bytes {
            return Err(Stop::Limit);
        }
        charge(
            &mut self.usage.borrow_mut().utf8,
            text.len(),
            self.limits.max_total_utf8_bytes,
        )
    }
    fn request(&self, request: &Requirement) -> Result<Option<RawVersionConstraint>, Stop> {
        self.text(&request.name)?;
        if !valid_name(&request.name) {
            return Err(Stop::Invalid);
        }
        if request.requested_extras.len() > self.limits.max_extras_per_requirement {
            return Err(Stop::Limit);
        }
        charge(
            &mut self.usage.borrow_mut().extra_references,
            request.requested_extras.len(),
            self.limits.max_total_extra_references,
        )?;
        for extra in &request.requested_extras {
            self.text(extra)?;
            if !valid_name(extra) {
                return Err(Stop::Invalid);
            }
        }
        match &request.constraint {
            RequirementConstraint::Version(specifier) => {
                self.text(specifier)?;
                let maximum = self.limits.max_field_bytes;
                RawVersionConstraint::parse(
                    specifier,
                    RawVersionConstraintLimits {
                        max_specifier_bytes: maximum,
                        max_clauses: maximum,
                        max_literal_bytes: maximum,
                        max_candidate_bytes: maximum,
                    },
                )
                .map(Some)
                .map_err(raw_constraint_error)
            }
            RequirementConstraint::SourceReference => Ok(None),
        }
    }
    fn truth(&self, truth: &ExtraTruth) -> Result<TruthView, Stop> {
        let bits = truth.extra_names.len();
        if bits > self.limits.max_truth_extras {
            return Err(Stop::Limit);
        }
        let shift = u32::try_from(bits).map_err(|_| Stop::Limit)?;
        let states = 1usize.checked_shl(shift).ok_or(Stop::Limit)?;
        let work = states.checked_mul(bits.max(1)).ok_or(Stop::Limit)?;
        {
            let mut usage = self.usage.borrow_mut();
            charge(
                &mut usage.guard_states,
                states,
                self.limits.max_total_guard_states,
            )?;
            charge(&mut usage.truth_work, work, self.limits.max_truth_work)?;
            charge(
                &mut usage.extra_references,
                bits,
                self.limits.max_total_extra_references,
            )?;
        }
        for name in &truth.extra_names {
            self.text(name)?;
            if !valid_name(name) {
                return Err(Stop::Invalid);
            }
        }
        if !truth.extra_names.windows(2).all(|pair| pair[0] < pair[1])
            || truth.enabled_masks.len() > states
            || !truth.enabled_masks.windows(2).all(|pair| pair[0] < pair[1])
            || truth.enabled_masks.iter().any(|mask| *mask >= states)
        {
            return Err(Stop::Invalid);
        }
        let mut enabled = Vec::new();
        enabled.try_reserve_exact(states).map_err(|_| Stop::Limit)?;
        enabled.resize(states, false);
        for mask in &truth.enabled_masks {
            enabled[*mask] = true;
        }
        // Current2026 can only turn false into true as extras are added.
        for mask in 0..states {
            if enabled[mask] {
                for bit in 0..bits {
                    if !enabled[mask | (1usize << bit)] {
                        return Err(Stop::Invalid);
                    }
                }
            }
        }
        let mut bit_ranges = Vec::new();
        bit_ranges
            .try_reserve_exact(bits)
            .map_err(|_| Stop::Limit)?;
        for bit in 0..bits {
            let width = 1usize << bit;
            let step = width.checked_mul(2).ok_or(Stop::Limit)?;
            let mut intervals = Vec::new();
            intervals
                .try_reserve_exact(states / step)
                .map_err(|_| Stop::Limit)?;
            for start in (width..states).step_by(step) {
                self.tick()?;
                intervals.push((Bound::Included(start), Bound::Excluded(start + width)));
            }
            bit_ranges.push(intervals.into_iter().collect());
        }
        Ok(TruthView {
            enabled,
            bit_ranges,
        })
    }
    fn project(&self, name: &str) -> Result<&'a [CandidateVersion], Stop> {
        if let Some(project) = self.projects.borrow().get(name).copied() {
            return Ok(project);
        }
        self.tick()?;
        if self.projects.borrow().len() >= self.limits.max_projects {
            return Err(Stop::Limit);
        }
        let project = self
            .depot
            .project(name)
            .ok_or_else(|| Stop::Project(name.into()))?;
        charge(
            &mut self.usage.borrow_mut().candidates,
            project.len(),
            self.limits.max_candidates,
        )?;
        for candidate in project {
            self.tick()?;
            self.text(&candidate.version)?;
            pep440_rs::Version::from_str(&candidate.version).map_err(|_| Stop::Invalid)?;
            if !self.identities.borrow_mut().insert(candidate.key) {
                return Err(Stop::Invalid);
            }
        }
        self.projects.borrow_mut().insert(name.into(), project);
        Ok(project)
    }
    fn metadata(&self, key: CandidateKey) -> Result<Rc<MetadataView<'a>>, Stop> {
        if let Some(view) = self.metadata.borrow().get(&key).cloned() {
            return Ok(view);
        }
        if !self.identities.borrow().contains(&key) {
            return Err(Stop::Invalid);
        }
        let data = self.depot.metadata(key).ok_or(Stop::Metadata(key))?;
        charge(
            &mut self.usage.borrow_mut().dependencies,
            data.dependencies.len(),
            self.limits.max_dependencies,
        )?;
        let mut specs = Vec::new();
        let mut truths = Vec::new();
        specs
            .try_reserve_exact(data.dependencies.len())
            .map_err(|_| Stop::Limit)?;
        truths
            .try_reserve_exact(data.dependencies.len())
            .map_err(|_| Stop::Limit)?;
        // Validate the complete declaration list before any child project is
        // consulted, including declarations whose condition will be false.
        for dependency in &data.dependencies {
            self.tick()?;
            specs.push(self.request(&dependency.requirement)?);
            truths.push(
                dependency
                    .extra_truth
                    .as_ref()
                    .map(|truth| self.truth(truth))
                    .transpose()?,
            );
        }
        let view = Rc::new(MetadataView {
            data,
            specs,
            truths,
        });
        self.metadata.borrow_mut().insert(key, Rc::clone(&view));
        Ok(view)
    }
    fn requirement(
        &self,
        request: &Requirement,
        spec: &RawVersionConstraint,
        constraints: &mut ConstraintMap,
    ) -> Result<(), Stop> {
        let project = self.project(&request.name)?;
        let mut intervals = Vec::new();
        intervals
            .try_reserve_exact(project.len())
            .map_err(|_| Stop::Limit)?;
        for (index, candidate) in project.iter().enumerate() {
            self.tick()?;
            if spec
                .matches(&candidate.version)
                .map_err(raw_constraint_error)?
            {
                intervals.push((Bound::Included(index), Bound::Included(index)));
            }
        }
        let range: Ranges<usize> = intervals.into_iter().collect();
        insert(
            constraints,
            Package::Distribution(request.name.clone()),
            range.clone(),
        );
        for extra in &request.requested_extras {
            self.tick()?;
            insert(
                constraints,
                Package::Feature(request.name.clone(), extra.clone()),
                range.clone(),
            );
        }
        Ok(())
    }
    fn dependency(
        &self,
        parent: CandidateKey,
        declaration: usize,
        request: &Requirement,
        spec: &Option<RawVersionConstraint>,
        constraints: &mut ConstraintMap,
    ) -> Result<(), Stop> {
        match (&request.constraint, spec) {
            (RequirementConstraint::Version(_), Some(spec)) => {
                self.requirement(request, spec, constraints)
            }
            (RequirementConstraint::SourceReference, None) => {
                if self.policy_mode {
                    // A joint-policy witness can temporarily introduce a
                    // ghost package. Only the original-root closure may emit
                    // an actual source-input request on this path.
                    Ok(())
                } else {
                    Err(Stop::Source {
                        parent,
                        declaration,
                    })
                }
            }
            _ => Err(Stop::Invalid),
        }
    }
    fn candidate(&self, name: &str, index: usize) -> Result<&'a CandidateVersion, Stop> {
        self.project(name)?.get(index).ok_or(Stop::Invalid)
    }
}
impl DependencyProvider for Provider<'_> {
    type P = Package;
    type V = usize;
    type VS = Ranges<usize>;
    type Priority = u32;
    type M = String;
    type Err = Stop;
    fn prioritize(
        &self,
        _package: &Package,
        _range: &Self::VS,
        stats: &PackageResolutionStatistics,
    ) -> u32 {
        stats.conflict_count()
    }
    fn choose_version(&self, package: &Package, range: &Self::VS) -> Result<Option<usize>, Stop> {
        self.tick()?;
        match package {
            Package::Root => Ok(range.contains(&0).then_some(0)),
            Package::Distribution(name) | Package::Feature(name, _) => {
                let project = self.project(name)?;
                if let Some(index) = self.retry_preferences.borrow().get(name) {
                    self.tick()?;
                    if *index >= project.len() {
                        return Err(Stop::Invalid);
                    }
                    if range.contains(index) {
                        return Ok(Some(*index));
                    }
                }
                for index in 0..project.len() {
                    self.tick()?;
                    if range.contains(&index) {
                        return Ok(Some(index));
                    }
                }
                Ok(None)
            }
            Package::Guard(key, index) => {
                let metadata = self.metadata(*key)?;
                let truth = metadata
                    .truths
                    .get(*index)
                    .and_then(Option::as_ref)
                    .ok_or(Stop::Invalid)?;
                for (lower, _) in range.iter() {
                    self.tick()?;
                    let first = match lower {
                        Bound::Unbounded => 0,
                        Bound::Included(value) => *value,
                        Bound::Excluded(value) => match value.checked_add(1) {
                            Some(value) => value,
                            None => continue,
                        },
                    };
                    if first < truth.enabled.len() && range.contains(&first) {
                        return Ok(Some(first));
                    }
                }
                Ok(None)
            }
            Package::RejectedAssignment(index) => {
                let nogoods = self.nogoods.borrow();
                let terms = nogoods.get(*index).ok_or(Stop::Invalid)?;
                for witness in 0..terms.len() {
                    self.tick()?;
                    if range.contains(&witness) {
                        return Ok(Some(witness));
                    }
                }
                Ok(None)
            }
        }
    }
    fn get_dependencies(
        &self,
        package: &Package,
        version: &usize,
    ) -> Result<Dependencies<Package, Self::VS, String>, Stop> {
        self.tick()?;
        let metadata = match package {
            Package::Root | Package::RejectedAssignment(_) => None,
            Package::Distribution(name) | Package::Feature(name, _) => {
                Some(self.metadata(self.candidate(name, *version)?.key)?)
            }
            Package::Guard(key, _) => Some(self.metadata(*key)?),
        };
        if let Some(metadata) = &metadata {
            match metadata.data.rejection {
                Some(CandidateRejection::RequiresPython) => {
                    return Ok(Dependencies::Unavailable(
                        "candidate Requires-Python is incompatible".into(),
                    ));
                }
                None => {}
            }
        }
        let mut constraints = ConstraintMap::new();
        match package {
            Package::Root => {
                for (request, spec) in self.requests.iter().zip(self.root_specs.borrow().iter()) {
                    self.requirement(request, spec, &mut constraints)?;
                }
                for (index, terms) in self.nogoods.borrow().iter().enumerate() {
                    self.tick()?;
                    insert(
                        &mut constraints,
                        Package::RejectedAssignment(index),
                        Ranges::between(0usize, terms.len()),
                    );
                }
            }
            Package::Distribution(name) => {
                let key = self.candidate(name, *version)?.key;
                let metadata = metadata.as_ref().ok_or(Stop::Invalid)?;
                for (index, dependency) in metadata.data.dependencies.iter().enumerate() {
                    self.tick()?;
                    if let Some(truth) = &metadata.truths[index] {
                        insert(
                            &mut constraints,
                            Package::Guard(key, index),
                            Ranges::between(0usize, truth.enabled.len()),
                        );
                    } else {
                        self.dependency(
                            key,
                            index,
                            &dependency.requirement,
                            &metadata.specs[index],
                            &mut constraints,
                        )?;
                    }
                }
            }
            Package::Feature(name, extra) => {
                let key = self.candidate(name, *version)?.key;
                let metadata = metadata.as_ref().ok_or(Stop::Invalid)?;
                insert(
                    &mut constraints,
                    Package::Distribution(name.clone()),
                    Ranges::singleton(*version),
                );
                for (index, dependency) in metadata.data.dependencies.iter().enumerate() {
                    self.tick()?;
                    if let Some(truth) = &dependency.extra_truth {
                        if let Ok(bit) = truth.extra_names.binary_search(extra) {
                            let view = metadata.truths[index].as_ref().ok_or(Stop::Invalid)?;
                            insert(
                                &mut constraints,
                                Package::Guard(key, index),
                                view.bit_ranges[bit].clone(),
                            );
                        }
                    }
                }
            }
            Package::Guard(key, index) => {
                let metadata = metadata.as_ref().ok_or(Stop::Invalid)?;
                let truth = metadata
                    .truths
                    .get(*index)
                    .and_then(Option::as_ref)
                    .ok_or(Stop::Invalid)?;
                let enabled = truth.enabled.get(*version).ok_or(Stop::Invalid)?;
                if *enabled {
                    self.dependency(
                        *key,
                        *index,
                        &metadata.data.dependencies[*index].requirement,
                        &metadata.specs[*index],
                        &mut constraints,
                    )?;
                }
            }
            Package::RejectedAssignment(index) => {
                let nogoods = self.nogoods.borrow();
                let (name, previous) = nogoods
                    .get(*index)
                    .and_then(|terms| terms.get(*version))
                    .ok_or(Stop::Invalid)?;
                insert(
                    &mut constraints,
                    Package::Distribution(name.clone()),
                    Ranges::singleton(*previous).complement(),
                );
            }
        }
        Ok(Dependencies::Available(
            constraints.into_iter().collect::<Constraints>(),
        ))
    }
    fn should_cancel(&self) -> Result<(), Stop> {
        self.tick()
    }
}
fn stopped(stop: Stop) -> Result<SolveStep, NativeSolveError> {
    match stop {
        Stop::Project(name) => Ok(SolveStep::NeedProject(name)),
        Stop::Metadata(key) => Ok(SolveStep::NeedMetadata(key)),
        Stop::Invalid => Err(NativeSolveError::InvalidInput),
        Stop::Limit => Err(NativeSolveError::Limit),
        Stop::Source {
            parent,
            declaration,
        } => Ok(SolveStep::NeedSource {
            parent,
            declaration,
        }),
    }
}
fn make_provider<'a>(
    requests: &'a [Requirement],
    depot: &'a dyn MetadataDepot,
    limits: NativeSolveLimits,
) -> Result<Provider<'a>, NativeSolveError> {
    if [
        limits.max_projects,
        limits.max_candidates,
        limits.max_dependencies,
        limits.max_field_bytes,
        limits.max_total_utf8_bytes,
        limits.max_solver_calls,
        limits.max_extras_per_requirement,
        limits.max_truth_extras,
        limits.max_total_extra_references,
        limits.max_total_guard_states,
        limits.max_truth_work,
    ]
    .contains(&0)
    {
        return Err(NativeSolveError::InvalidLimits);
    }
    if requests.len() > limits.max_dependencies {
        return Err(NativeSolveError::Limit);
    }
    let provider = Provider {
        requests,
        depot,
        limits,
        root_specs: RefCell::default(),
        projects: RefCell::default(),
        metadata: RefCell::default(),
        identities: RefCell::default(),
        usage: RefCell::new(Usage {
            dependencies: requests.len(),
            ..Default::default()
        }),
        calls: Cell::new(0),
        policy_mode: false,
        nogoods: RefCell::default(),
        retry_preferences: RefCell::default(),
    };
    let mut specs = Vec::new();
    specs
        .try_reserve_exact(requests.len())
        .map_err(|_| NativeSolveError::Limit)?;
    for request in requests {
        match provider.request(request) {
            Ok(Some(spec)) => specs.push(spec),
            Ok(None) => return Err(NativeSolveError::InvalidInput),
            Err(Stop::Limit) => return Err(NativeSolveError::Limit),
            Err(_) => return Err(NativeSolveError::InvalidInput),
        }
    }
    provider.root_specs.replace(specs);
    Ok(provider)
}
pub(super) fn run_with_policy(
    requests: &[Requirement],
    depot: &dyn MetadataDepot,
    policy: &dyn super::policy::ActiveCandidatePolicy,
    solver_limits: NativeSolveLimits,
    policy_limits: super::policy::NativePolicyLimits,
) -> Result<SolveStep, NativeSolveError> {
    active_policy::run(requests, depot, policy, solver_limits, policy_limits)
}
pub(super) fn run(
    requests: &[Requirement],
    depot: &dyn MetadataDepot,
    limits: NativeSolveLimits,
) -> Result<SolveStep, NativeSolveError> {
    let provider = make_provider(requests, depot, limits)?;
    if requests.is_empty() {
        return Ok(SolveStep::Complete(NativePlan { selected: vec![] }));
    }
    let selected = match pubgrub::resolve(&provider, Package::Root, 0usize) {
        Ok(selected) => selected,
        Err(PubGrubError::NoSolution(_)) => return Err(NativeSolveError::NoSolution),
        Err(
            PubGrubError::ErrorRetrievingDependencies { source, .. }
            | PubGrubError::ErrorChoosingVersion { source, .. }
            | PubGrubError::ErrorInShouldCancel(source),
        ) => return stopped(source),
    };
    let mut distributions = BTreeMap::new();
    let mut features = Vec::new();
    for (package, index) in selected {
        match package {
            Package::Distribution(name) => {
                distributions.insert(name, index);
            }
            Package::Feature(name, extra) => features.push((name, extra, index)),
            Package::Root | Package::Guard(_, _) | Package::RejectedAssignment(_) => {}
        }
    }
    let mut extras = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, extra, index) in features {
        if distributions.get(&name) != Some(&index) {
            return Err(NativeSolveError::InvalidInput);
        }
        extras.entry(name).or_default().insert(extra);
    }
    let mut nodes = Vec::new();
    for (name, index) in distributions {
        let candidate = provider
            .candidate(&name, index)
            .map_err(|_| NativeSolveError::InvalidInput)?;
        nodes.push(SelectedCandidate {
            requested_extras: extras
                .remove(&name)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            name,
            key: candidate.key,
            version: candidate.version.clone(),
        });
    }
    Ok(SolveStep::Complete(NativePlan { selected: nodes }))
}
