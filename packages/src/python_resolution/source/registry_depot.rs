//! Resolve-mode coordination of genuine registry, lock and metadata owners.
//!
//! The prepared result retains the original declarations and exact observed
//! files. It is neither a closed graph nor permission to publish or install.
//! Strict locked-graph replay is a separate operation, not a one-candidate solve.

#[path = "registry_depot/active_policy.rs"]
mod active_policy;
#[path = "built.rs"]
mod built;
pub(in crate::packages::python_resolution::source) use built::BuiltRegistryWheel as ReplayedBuiltRegistryWheel;
use built::{BuiltRegistryWheel, RetainedBuiltRegistryWheel};

use super::*;
pub(crate) mod closed;
use std::collections::BTreeMap;
use std::time::Instant;
#[cfg(test)]
pub(crate) use tests::build_fixture::RegistryBuildFixture;
#[cfg(test)]
pub(crate) use tests::project_fixture::ProjectRegistryFixture;

use crate::packages::python_resolution::candidate_policy::{
    CachedCandidateUniverse, CandidatePolicy, CandidatePolicyLimits, CandidatePolicyUsage,
    PolicyNotice, Prereleases, Yanked, classify_candidate_universe,
    order_fresh_candidates_with_usage,
};
use crate::packages::python_resolution::lock_model::{ContentPin, LockModelLimits};
use crate::packages::python_resolution::lock_session::{
    BoundPythonLock, FilePinLookup, ResolutionIntent,
};
use crate::packages::python_resolution::marker_truth::ExtraTruthLimits;
use crate::packages::python_resolution::metadata_input::checked_metadata_context;
use crate::packages::python_resolution::metadata_input::{
    DependencyConstraint, MetadataInputError, MetadataInputLimits, PythonCompatibility,
    compile_metadata_input,
};
use crate::packages::python_wheel::{WheelLimits, WheelManifest};
use pakro_index::SupportedWheelTags;
use pakro_index::python_markers::{
    MarkerSemantics, PythonMarkerEnvironment, PythonMarkerLimits, PythonMarkerScope,
    evaluate_python_marker,
};
use pakro_index::python_metadata::{PythonDependencyMetadataFacts, PythonMetadataLimits};
use pakro_index::python_requirements::{
    PythonRequirementLimits, PythonRequirementSyntax, parse_python_requirement,
};
use pakro_index::python_source::{PythonSourcePolicy, ordered_python_source_indices};
use pakro_resolver::python_native::policy::{NativePolicyLimits, solve_step_with_policy};
use pakro_resolver::python_native::{
    CandidateKey, CandidateMetadata, CandidateRejection, CandidateVersion, Dependency, ExtraTruth,
    MetadataDepot, NativePlan, NativeSolveLimits, Requirement, RequirementConstraint, SolveStep,
    solve_step,
};

use super::caller_deadline::{
    acquire_fresh_registry_file_at_deadline, fetch_registry_project_at_deadline,
};
use super::fresh::wheel::sidecar::{
    CheckedFreshWheel, FreshSidecarError, SidecarLimits, verify_advertised_core_metadata,
};
use super::fresh::wheel::{FreshMetadataState, FreshWheelError, route_fresh_metadata};
use super::fresh::{FreshPinError, FreshPinnedArtifact, selected_file_ref};
use super::replay::metadata::{
    LockedMetadataError, LockedMetadataState, LockedWheelCandidate, route_locked_metadata,
};
use super::replay::{
    LockedRegistryArtifact, LockedReplayContext, LockedReplayError,
    acquire_selected_existing_registry_file,
};

pub(crate) type RegistryDepotResult<T> = Result<T, RegistryDepotError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum RegistryDepotError {
    #[error("registry coordination is not implemented")]
    NotImplemented,
    #[error("registry coordination limits must be positive")]
    InvalidLimits,
    #[error("registry coordination budget exceeded")]
    Limit,
    #[error("registry coordination deadline expired")]
    Deadline,
    #[error("registry coordination configuration is inconsistent")]
    InvalidConfiguration,
    #[error("an original Python root declaration is invalid")]
    InvalidRoot,
    #[error("a root marker cannot be evaluated in its explicit scope")]
    RootMarker,
    #[error("the actual project lock is no longer current")]
    Revoked,
    #[error("registry project acquisition failed: {0}")]
    Project(#[source] SourceResolutionError),
    #[error("registry candidate policy failed")]
    CandidatePolicy,
    #[error("registry file acquisition failed: {0}")]
    FreshAcquisition(#[source] FreshPinError),
    #[error("registry wheel inspection failed: {0}")]
    FreshWheel(#[source] FreshWheelError),
    #[error("registry advertised metadata verification failed: {0}")]
    Sidecar(#[source] FreshSidecarError),
    #[error("registry locked file acquisition failed: {0}")]
    LockedAcquisition(#[source] LockedReplayError),
    #[error("registry locked wheel inspection failed: {0}")]
    LockedWheel(#[source] LockedMetadataError),
    #[error("registry dependency metadata compilation failed: {0}")]
    MetadataInput(#[source] MetadataInputError),
    #[error("registry source build preparation or output verification failed")]
    Build,
    #[error("native Python constraints have no solution")]
    NoSolution,
    #[error("registry candidate identity is inconsistent")]
    IdentityMismatch,
}

pub(crate) use crate::packages::python_resolution::root_scopes::OwnedRootMarkerScope as RegistryRootScope;

struct RegistryEnvironment {
    os_name: String,
    sys_platform: String,
    platform_release: String,
    platform_system: String,
    platform_version: String,
    platform_machine: String,
    platform_python_implementation: String,
    python_version: String,
    python_full_version: String,
    implementation_name: String,
    implementation_version: String,
}
impl RegistryEnvironment {
    fn from_checked(value: PythonMarkerEnvironment<'_>) -> Self {
        Self {
            os_name: value.os_name.to_owned(),
            sys_platform: value.sys_platform.to_owned(),
            platform_release: value.platform_release.to_owned(),
            platform_system: value.platform_system.to_owned(),
            platform_version: value.platform_version.to_owned(),
            platform_machine: value.platform_machine.to_owned(),
            platform_python_implementation: value.platform_python_implementation.to_owned(),
            python_version: value.python_version.to_owned(),
            python_full_version: value.python_full_version.to_owned(),
            implementation_name: value.implementation_name.to_owned(),
            implementation_version: value.implementation_version.to_owned(),
        }
    }
    fn view(&self) -> PythonMarkerEnvironment<'_> {
        PythonMarkerEnvironment {
            os_name: &self.os_name,
            sys_platform: &self.sys_platform,
            platform_release: &self.platform_release,
            platform_system: &self.platform_system,
            platform_version: &self.platform_version,
            platform_machine: &self.platform_machine,
            platform_python_implementation: &self.platform_python_implementation,
            python_version: &self.python_version,
            python_full_version: &self.python_full_version,
            implementation_name: &self.implementation_name,
            implementation_version: &self.implementation_version,
        }
    }
}

/// Sessions are fixed for the attempt; callers cannot replace authentication
/// or routing after an immutable project observation has issued local keys.
pub(crate) struct RegistryEndpoint<'a> {
    pub(crate) project: PythonSourceSession<'a>,
    pub(crate) file: SourceFileSession<'a>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegistryDepotLimits {
    pub(crate) max_roots: usize,
    pub(crate) max_projects: usize,
    pub(crate) max_candidates: usize,
    pub(crate) max_metadata: usize,
    pub(crate) max_steps: usize,
    pub(crate) max_retained_bytes: u64,
    pub(crate) max_utf8_bytes: usize,
    pub(crate) requirements: PythonRequirementLimits,
    pub(crate) markers: PythonMarkerLimits,
    pub(crate) project: SimpleClientLimits,
    pub(crate) source: SourceObservationLimits,
    pub(crate) lock: LockModelLimits,
    pub(crate) wheel: WheelLimits,
    pub(crate) metadata: PythonMetadataLimits,
    pub(crate) input: MetadataInputLimits,
    pub(crate) truth: ExtraTruthLimits,
    pub(crate) solver: NativeSolveLimits,
    pub(crate) candidates: CandidatePolicyLimits,
    pub(crate) policy: NativePolicyLimits,
}
impl Default for RegistryDepotLimits {
    fn default() -> Self {
        Self {
            max_roots: 4096,
            max_projects: 4096,
            max_candidates: 65536,
            max_metadata: 8192,
            max_steps: 131072,
            max_retained_bytes: 4 * 1024 * 1024 * 1024,
            max_utf8_bytes: 64 * 1024 * 1024,
            requirements: Default::default(),
            markers: Default::default(),
            project: Default::default(),
            source: Default::default(),
            lock: Default::default(),
            wheel: Default::default(),
            metadata: Default::default(),
            input: Default::default(),
            truth: Default::default(),
            solver: Default::default(),
            candidates: Default::default(),
            policy: Default::default(),
        }
    }
}
impl RegistryDepotLimits {
    fn validate(self) -> RegistryDepotResult<()> {
        if [
            self.max_roots,
            self.max_projects,
            self.max_candidates,
            self.max_metadata,
            self.max_steps,
            self.max_utf8_bytes,
            self.requirements.max_input_bytes,
            self.requirements.max_requested_extras,
            self.requirements.max_syntax_tokens,
            self.requirements.max_marker_comparisons,
            self.candidates.max_files,
            self.candidates.max_constraints,
            self.candidates.max_supported_tags,
            self.candidates.max_field_bytes,
            self.candidates.max_total_utf8_bytes,
            self.candidates.max_work_units,
            self.input.max_dependencies,
            self.input.max_retained_utf8_bytes,
            self.input.max_total_truth_states,
            self.input.max_total_evaluation_nodes,
            self.truth.max_relevant_extras,
            self.truth.max_truth_states,
            self.truth.max_total_evaluation_nodes,
            self.truth.max_name_bytes,
            self.solver.max_projects,
            self.solver.max_candidates,
            self.solver.max_dependencies,
            self.solver.max_field_bytes,
            self.solver.max_total_utf8_bytes,
            self.solver.max_solver_calls,
            self.solver.max_extras_per_requirement,
            self.solver.max_truth_extras,
            self.solver.max_total_extra_references,
            self.solver.max_total_guard_states,
            self.solver.max_truth_work,
        ]
        .contains(&0)
            || self.max_retained_bytes == 0
        {
            return Err(RegistryDepotError::InvalidLimits);
        }
        self.markers
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        self.requirements
            .marker
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        self.lock
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        self.policy
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        self.project
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        self.source
            .validate()
            .map_err(|_| RegistryDepotError::InvalidLimits)?;
        super::wheel_metadata::validate_limits(self.wheel, self.metadata)
            .map_err(|_| RegistryDepotError::InvalidLimits)
    }
}

#[derive(Clone)]
pub(crate) struct RegistryDepotContext<'a> {
    pub(crate) client: &'a SimpleIndexClient,
    pub(crate) endpoints: &'a [RegistryEndpoint<'a>],
    pub(crate) source_policy: &'a PythonSourcePolicy,
    pub(crate) candidate_policy: CandidatePolicy<'a>,
    pub(crate) supported: &'a SupportedWheelTags,
    pub(crate) environment: PythonMarkerEnvironment<'a>,
    pub(crate) root_scope: RegistryRootScope,
    pub(crate) semantics: MarkerSemantics,
    pub(crate) fetch: SimpleFetchPolicy,
    pub(crate) limits: RegistryDepotLimits,
    pub(crate) cache_root: &'a Path,
    pub(crate) scratch_root: &'a Path,
    pub(crate) deadline: Instant,
}

/// The local key is issued only while inserting a genuine immutable project.
struct ProjectEntry {
    owner: Option<BoundRegistryProject>,
    endpoint: usize,
    candidates: Vec<CandidateVersion>,
    universe: Option<CachedCandidateUniverse>,
}
struct FileEntry {
    project: String,
    file_index: usize,
}
struct MetadataEntry<'root> {
    owner: RegistryMetadataOwner<'root>,
    input: CandidateMetadata,
}

pub(crate) enum RegistryMetadataOwner<'root> {
    Fresh(CheckedFreshWheel<'root>),
    Locked(LockedWheelCandidate<'root>),
    Built(BuiltRegistryWheel<'root>),
}
impl RegistryMetadataOwner<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        match self {
            Self::Fresh(value) => value.wheel().pin(),
            Self::Locked(value) => value.pin(),
            Self::Built(value) => value.pin(),
        }
    }
    pub(crate) fn manifest(&self) -> &WheelManifest {
        match self {
            Self::Fresh(value) => value.wheel().manifest(),
            Self::Locked(value) => value.manifest(),
            Self::Built(value) => value.manifest(),
        }
    }
    pub(crate) fn facts(&self) -> &PythonDependencyMetadataFacts {
        match self {
            Self::Fresh(value) => value.wheel().facts(),
            Self::Locked(value) => value.facts(),
            Self::Built(value) => value.facts(),
        }
    }
    pub(crate) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        match self {
            Self::Fresh(value) => value
                .recheck_for(root)
                .map_err(|_| RegistryDepotError::Revoked),
            Self::Locked(value) => value
                .recheck_for(root)
                .map_err(|_| RegistryDepotError::Revoked),
            Self::Built(value) => value.recheck_for(root),
        }
    }
    pub(crate) fn output_filename(&self) -> &str {
        self.build_record()
            .map_or(self.pin().file().filename(), |record| {
                &record.output_filename
            })
    }
    pub(crate) fn output_digest(&self) -> Digest {
        match self {
            Self::Built(value) => value.output_digest(),
            _ => self.pin().digest(),
        }
    }
    pub(crate) fn output_size(&self) -> u64 {
        self.build_record()
            .map_or(self.pin().size(), |record| record.output_size)
    }
    pub(crate) fn build_record(
        &self,
    ) -> Option<&crate::packages::python_environment::model::SourceBuildRecord> {
        match self {
            Self::Built(value) => Some(value.record()),
            _ => None,
        }
    }
}
impl fmt::Debug for RegistryMetadataOwner<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("RegistryMetadataOwner")
            .field("pin", self.pin())
            .finish_non_exhaustive()
    }
}

pub(crate) struct ResolveRegistryDepot<'root, 'config> {
    root: &'root BoundPythonLock,
    context: RegistryDepotContext<'config>,
    roots: Vec<PythonRequirementSyntax>,
    root_scopes: Option<Vec<RegistryRootScope>>,
    active_roots: Vec<usize>,
    environment: RegistryEnvironment,
    requirements: Vec<Requirement>,
    projects: BTreeMap<String, ProjectEntry>,
    files: BTreeMap<CandidateKey, FileEntry>,
    metadata: BTreeMap<CandidateKey, MetadataEntry<'root>>,
    source_order: Vec<usize>,
    steps: usize,
    retained_bytes: u64,
    retained_utf8: usize,
    policy_usage: CandidatePolicyUsage,
}
impl<'root, 'config> ResolveRegistryDepot<'root, 'config> {
    pub(crate) fn new_scoped(
        root: &'root BoundPythonLock,
        raw_roots: &[&str],
        root_scopes: &[RegistryRootScope],
        context: RegistryDepotContext<'config>,
    ) -> RegistryDepotResult<Self> {
        Self::new_inner(root, raw_roots, Some(root_scopes), context)
    }
    pub(crate) fn new(
        root: &'root BoundPythonLock,
        raw_roots: &[&str],
        context: RegistryDepotContext<'config>,
    ) -> RegistryDepotResult<Self> {
        Self::new_inner(root, raw_roots, None, context)
    }
    fn new_inner(
        root: &'root BoundPythonLock,
        raw_roots: &[&str],
        root_scopes: Option<&[RegistryRootScope]>,
        context: RegistryDepotContext<'config>,
    ) -> RegistryDepotResult<Self> {
        let limits = context.limits;
        limits.validate()?;
        if Instant::now() >= context.deadline {
            return Err(RegistryDepotError::Deadline);
        }
        root.recheck().map_err(|_| RegistryDepotError::Revoked)?;
        if raw_roots.len() > limits.max_roots {
            return Err(RegistryDepotError::Limit);
        }
        if root_scopes.is_some_and(|scopes| scopes.len() != raw_roots.len()) {
            return Err(RegistryDepotError::InvalidConfiguration);
        }
        if context.endpoints.len() > limits.max_projects {
            return Err(RegistryDepotError::Limit);
        }
        let mut bytes = 0usize;
        for raw in raw_roots {
            bytes = bytes
                .checked_add(raw.len())
                .filter(|n| *n <= limits.max_utf8_bytes)
                .ok_or(RegistryDepotError::Limit)?;
        }
        // Charge the original context input before cloning any scope. Include
        // duplicates and inactive roots; the environment cannot hide a budget.
        let mut scope_values = 0usize;
        for scope in std::iter::once(&context.root_scope).chain(root_scopes.into_iter().flatten()) {
            let mut entries = 0usize;
            for value in scope.values() {
                entries = entries
                    .checked_add(1)
                    .filter(|n| *n <= limits.markers.max_set_entries)
                    .ok_or(RegistryDepotError::Limit)?;
                scope_values = scope_values
                    .checked_add(1)
                    .filter(|n| *n <= limits.solver.max_total_extra_references)
                    .ok_or(RegistryDepotError::Limit)?;
                bytes = bytes
                    .checked_add(value.len())
                    .filter(|n| *n <= limits.max_utf8_bytes)
                    .ok_or(RegistryDepotError::Limit)?;
                if value.len() > limits.markers.max_environment_field_bytes {
                    return Err(RegistryDepotError::Limit);
                }
                pakro_index::python_source::NormalizedPythonName::new(value)
                    .map_err(|_| RegistryDepotError::InvalidConfiguration)?;
            }
        }
        for value in [
            context.environment.os_name,
            context.environment.sys_platform,
            context.environment.platform_release,
            context.environment.platform_system,
            context.environment.platform_version,
            context.environment.platform_machine,
            context.environment.platform_python_implementation,
            context.environment.python_version,
            context.environment.python_full_version,
            context.environment.implementation_name,
            context.environment.implementation_version,
        ] {
            bytes = bytes
                .checked_add(value.len())
                .filter(|n| *n <= limits.max_utf8_bytes)
                .ok_or(RegistryDepotError::Limit)?;
        }
        checked_metadata_context(context.environment, limits.markers)
            .map_err(|_| RegistryDepotError::InvalidConfiguration)?;
        // Source configuration is checked before observing any availability.
        let registries: Vec<_> = context
            .endpoints
            .iter()
            .map(|endpoint| endpoint.project.registry)
            .collect();
        let source_order = ordered_python_source_indices(&registries, context.source_policy)
            .map_err(|_| RegistryDepotError::InvalidConfiguration)?;
        let mut roots = Vec::new();
        let mut active_roots = Vec::new();
        roots
            .try_reserve_exact(raw_roots.len())
            .map_err(|_| RegistryDepotError::Limit)?;
        active_roots
            .try_reserve_exact(raw_roots.len())
            .map_err(|_| RegistryDepotError::Limit)?;
        for (index, raw) in raw_roots.iter().enumerate() {
            if Instant::now() >= context.deadline {
                return Err(RegistryDepotError::Deadline);
            }
            let declaration = parse_python_requirement(raw, limits.requirements)
                .map_err(|_| RegistryDepotError::InvalidRoot)?;
            let active = match declaration.marker() {
                None => true,
                Some(expression) => root_scopes
                    .map_or(&context.root_scope, |scopes| &scopes[index])
                    .with_scope(|scope| {
                        evaluate_python_marker(
                            expression,
                            context.environment,
                            scope,
                            context.semantics,
                            limits.markers,
                        )
                    })
                    .map_err(|_| RegistryDepotError::RootMarker)?
                    .value(),
            };
            if active {
                active_roots.push(index);
            }
            roots.push(declaration);
        }
        if Instant::now() >= context.deadline {
            return Err(RegistryDepotError::Deadline);
        }
        Ok(Self {
            root,
            environment: RegistryEnvironment::from_checked(context.environment),
            context,
            roots,
            root_scopes: root_scopes.map(<[RegistryRootScope]>::to_vec),
            active_roots,
            requirements: Vec::new(),
            projects: BTreeMap::new(),
            files: BTreeMap::new(),
            metadata: BTreeMap::new(),
            source_order,
            steps: 0,
            retained_bytes: 0,
            retained_utf8: bytes,
            policy_usage: Default::default(),
        })
    }

    pub(crate) async fn prepare(
        mut self,
    ) -> RegistryDepotResult<RegistryPreparation<'root, 'config>> {
        self.check_live()?;
        for position in 0..self.active_roots.len() {
            let index = self.active_roots[position];
            let root = &self.roots[index];
            if root.unresolved_reference().is_some() {
                let declaration = root.raw_for_semantic_resolution().to_owned();
                return Ok(RegistryPreparation::NeedsSourceAdapter(
                    RegistrySourceContinuation {
                        depot: self,
                        declaration,
                        request: RegistrySourceRequest::Root { declaration: index },
                    },
                ));
            }
            let requirement = Requirement {
                name: root.name().to_owned(),
                constraint: RequirementConstraint::Version(
                    root.raw_version_specifiers().unwrap_or_default().to_owned(),
                ),
                requested_extras: root.requested_extras().map(str::to_owned).collect(),
            };
            self.charge(requirement.name.len())?;
            if let RequirementConstraint::Version(value) = &requirement.constraint {
                self.charge(value.len())?;
            }
            for extra in &requirement.requested_extras {
                self.charge(extra.len())?;
            }
            self.requirements.push(requirement);
        }
        // Conditional fallback needs the actual backtracking constraints.
        if self.context.candidate_policy.installed_version.is_some() {
            return Ok(RegistryPreparation::NeedsConstraintPolicy(
                RegistryPolicyContinuation { depot: self },
            ));
        }
        self.solve().await
    }

    async fn solve(mut self) -> RegistryDepotResult<RegistryPreparation<'root, 'config>> {
        loop {
            self.tick()?;
            let mut policy_notices = BTreeMap::new();
            let result = if self.uses_active_policy() {
                let usage = std::cell::RefCell::new(std::mem::take(&mut self.policy_usage));
                let notices = std::cell::RefCell::new(BTreeMap::new());
                let adapter = active_policy::RegistryActivePolicy {
                    depot: &self,
                    usage: &usage,
                    notices: &notices,
                };
                let result = solve_step_with_policy(
                    &self.requirements,
                    &self,
                    &adapter,
                    self.context.limits.solver,
                    self.context.limits.policy,
                );
                self.policy_usage = usage.into_inner();
                policy_notices = notices.into_inner();
                result
            } else {
                solve_step(&self.requirements, &self, self.context.limits.solver)
            };
            self.check_live()?;
            let step = result.map_err(|error| match error {
                pakro_resolver::python_native::NativeSolveError::NoSolution => {
                    RegistryDepotError::NoSolution
                }
                pakro_resolver::python_native::NativeSolveError::Limit => RegistryDepotError::Limit,
                _ => RegistryDepotError::IdentityMismatch,
            })?;
            match step {
                SolveStep::NeedProject(name) => self.observe_project(&name).await?,
                SolveStep::NeedMetadata(key) => match self.observe_metadata(key).await? {
                    MetadataObservation::Wheel(owner) => {
                        self.insert_metadata(key, owner)?;
                    }
                    MetadataObservation::Build(source) => {
                        return Ok(RegistryPreparation::NeedsBuild(RegistryBuildContinuation {
                            depot: self,
                            candidate: key,
                            source,
                        }));
                    }
                },
                SolveStep::NeedSource {
                    parent,
                    declaration,
                } => {
                    let owner = &self
                        .metadata
                        .get(&parent)
                        .ok_or(RegistryDepotError::IdentityMismatch)?
                        .owner;
                    let raw = owner
                        .facts()
                        .requirements()
                        .get(declaration)
                        .ok_or(RegistryDepotError::IdentityMismatch)?;
                    if raw.unresolved_reference().is_none() {
                        return Err(RegistryDepotError::IdentityMismatch);
                    }
                    let request = RegistrySourceRequest::Dependency {
                        parent,
                        declaration,
                    };
                    let declaration = raw.raw_for_semantic_resolution().to_owned();
                    return Ok(RegistryPreparation::NeedsSourceAdapter(
                        RegistrySourceContinuation {
                            depot: self,
                            declaration,
                            request,
                        },
                    ));
                }
                SolveStep::Complete(plan) => {
                    policy_notices
                        .retain(|key, _| plan.selected.iter().any(|node| node.key == *key));
                    return self.finish(plan, policy_notices);
                }
            }
        }
    }

    fn insert_metadata(
        &mut self,
        key: CandidateKey,
        owner: RegistryMetadataOwner<'root>,
    ) -> RegistryDepotResult<()> {
        let input = compile_metadata_input(
            owner.facts(),
            self.environment.view(),
            self.context.semantics,
            self.context.limits.markers,
            self.context.limits.truth,
            self.context.limits.input,
        )
        .map_err(RegistryDepotError::MetadataInput)?;
        let dependencies: Vec<Dependency> = input
            .dependencies
            .into_iter()
            .map(|dependency| Dependency {
                requirement: Requirement {
                    name: dependency.name,
                    constraint: match dependency.constraint {
                        DependencyConstraint::Version(value) => {
                            RequirementConstraint::Version(value)
                        }
                        DependencyConstraint::SourceReference(_) => {
                            RequirementConstraint::SourceReference
                        }
                    },
                    requested_extras: dependency.requested_extras,
                },
                extra_truth: dependency.condition.map(|truth| ExtraTruth {
                    extra_names: truth.extra_names().to_vec(),
                    enabled_masks: truth.enabled_masks().to_vec(),
                }),
            })
            .collect();
        for dependency in &dependencies {
            self.charge(dependency.requirement.name.len())?;
            if let RequirementConstraint::Version(value) = &dependency.requirement.constraint {
                self.charge(value.len())?;
            }
            for extra in &dependency.requirement.requested_extras {
                self.charge(extra.len())?;
            }
            if let Some(truth) = &dependency.extra_truth {
                for extra in &truth.extra_names {
                    self.charge(extra.len())?;
                }
            }
        }
        let rejection = (input.python_compatibility == PythonCompatibility::RequiresPythonMismatch)
            .then_some(CandidateRejection::RequiresPython);
        self.metadata.insert(
            key,
            MetadataEntry {
                owner,
                input: CandidateMetadata {
                    dependencies,
                    rejection,
                },
            },
        );
        Ok(())
    }

    fn uses_active_policy(&self) -> bool {
        self.context.candidate_policy.prereleases == Prereleases::IfNoMatchingFinal
            || self.context.candidate_policy.yanked == Yanked::ExactPinIfNoUnyanked
    }
    fn check_live(&self) -> RegistryDepotResult<()> {
        if Instant::now() >= self.context.deadline {
            return Err(RegistryDepotError::Deadline);
        }
        self.root
            .recheck()
            .map_err(|_| RegistryDepotError::Revoked)?;
        if Instant::now() >= self.context.deadline {
            return Err(RegistryDepotError::Deadline);
        }
        Ok(())
    }
    fn tick(&mut self) -> RegistryDepotResult<()> {
        self.check_live()?;
        self.steps = self
            .steps
            .checked_add(1)
            .filter(|n| *n <= self.context.limits.max_steps)
            .ok_or(RegistryDepotError::Limit)?;
        Ok(())
    }
    async fn observe_project(&mut self, name: &str) -> RegistryDepotResult<()> {
        if self.projects.contains_key(name) {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        if self.projects.len() >= self.context.limits.max_projects {
            return Err(RegistryDepotError::Limit);
        }
        let name =
            NormalizedPythonName::new(name).map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let python = checked_metadata_context(self.environment.view(), self.context.limits.markers)
            .map_err(|_| RegistryDepotError::InvalidConfiguration)?;
        for endpoint_index in self.source_order.clone() {
            self.tick()?;
            let endpoint = &self.context.endpoints[endpoint_index];
            let lookup = fetch_registry_project_at_deadline(
                self.context.client,
                project_session(&endpoint.project),
                &name,
                self.context.fetch,
                self.context.limits.project,
                self.context.deadline,
            )
            .await
            .map_err(|error| {
                stage_error(self.context.deadline, RegistryDepotError::Project(error))
            })?;
            self.check_live()?;
            let RegistryProjectLookup::Present(owner) = lookup else {
                continue;
            };
            self.charge_project(&owner)?;
            let (ordered, universe) = if self.uses_active_policy() {
                classify_candidate_universe(
                    &owner.data.project,
                    self.context.supported,
                    &python,
                    self.context.candidate_policy,
                    self.context.limits.candidates,
                    &mut self.policy_usage,
                ).map(|(ordered, universe)| (ordered, Some(universe)))
            } else {
                order_fresh_candidates_with_usage(
                    &owner.data.project,
                    &[],
                    self.context.supported,
                    &python,
                    self.context.candidate_policy,
                    self.context.limits.candidates,
                    &mut self.policy_usage,
                ).map(|ordered| (ordered, None))
            }
            .map_err(|error| match error {
                crate::packages::python_resolution::candidate_policy::CandidatePolicyError::Limit => {
                    RegistryDepotError::Limit
                }
                _ => RegistryDepotError::CandidatePolicy,
            })?;
            if self
                .files
                .len()
                .checked_add(ordered.candidates().len())
                .is_none_or(|n| n > self.context.limits.max_candidates)
            {
                return Err(RegistryDepotError::Limit);
            }
            let mut candidates = Vec::new();
            candidates
                .try_reserve_exact(ordered.candidates().len())
                .map_err(|_| RegistryDepotError::Limit)?;
            for candidate in ordered.candidates() {
                let version = candidate
                    .file()
                    .version()
                    .ok_or(RegistryDepotError::IdentityMismatch)?
                    .to_owned();
                self.charge(version.len())?;
                self.charge(name.as_str().len())?;
                let mut hash = Sha256::new();
                hash_field(&mut hash, b"pakro.registry-depot.local-candidate/v1");
                hash_field(&mut hash, owner.source_key().as_bytes());
                hash_field(&mut hash, &(candidate.file_index() as u64).to_be_bytes());
                hash_field(&mut hash, &(self.files.len() as u64).to_be_bytes());
                let key = CandidateKey(hash.finalize().into());
                if self
                    .files
                    .insert(
                        key,
                        FileEntry {
                            project: name.as_str().to_owned(),
                            file_index: candidate.file_index(),
                        },
                    )
                    .is_some()
                {
                    return Err(RegistryDepotError::IdentityMismatch);
                }
                candidates.push(CandidateVersion { key, version });
            }
            self.projects.insert(
                name.as_str().to_owned(),
                ProjectEntry {
                    owner: Some(owner),
                    endpoint: endpoint_index,
                    candidates,
                    universe,
                },
            );
            return Ok(());
        }
        self.projects.insert(
            name.as_str().to_owned(),
            ProjectEntry {
                owner: None,
                endpoint: 0,
                candidates: Vec::new(),
                universe: None,
            },
        );
        Ok(())
    }

    async fn observe_metadata(
        &mut self,
        key: CandidateKey,
    ) -> RegistryDepotResult<MetadataObservation<'root>> {
        self.tick()?;
        if self.metadata.len() >= self.context.limits.max_metadata
            || self.metadata.contains_key(&key)
        {
            return Err(RegistryDepotError::Limit);
        }
        let limits = self.context.limits;
        let temporary = limits
            .source
            .transfer
            .max_wire_bytes
            .checked_mul(2)
            .ok_or(RegistryDepotError::Limit)?;
        if self
            .retained_bytes
            .checked_add(temporary)
            .is_none_or(|n| n > limits.max_retained_bytes)
        {
            return Err(RegistryDepotError::Limit);
        }
        let file = self
            .files
            .get(&key)
            .ok_or(RegistryDepotError::IdentityMismatch)?;
        let entry = self
            .projects
            .get(&file.project)
            .ok_or(RegistryDepotError::IdentityMismatch)?;
        let project = entry
            .owner
            .as_ref()
            .ok_or(RegistryDepotError::IdentityMismatch)?;
        let selected = SelectedRegistryFile {
            project,
            file_index: file.file_index,
        };
        let reference = selected_file_ref(&selected, limits.lock)
            .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let endpoint = &self.context.endpoints[entry.endpoint];
        let observation = match self
            .root
            .lookup(&reference, ResolutionIntent::Resolve)
            .map_err(|_| RegistryDepotError::Revoked)?
        {
            FilePinLookup::Fresh(slot) => {
                let source = acquire_fresh_registry_file_at_deadline(
                    slot,
                    selected,
                    self.context.client,
                    file_session(&endpoint.file),
                    self.context.fetch.network,
                    limits.source,
                    limits.lock,
                    self.context.scratch_root,
                    self.context.deadline,
                )
                .await
                .map_err(|error| {
                    stage_error(
                        self.context.deadline,
                        RegistryDepotError::FreshAcquisition(error),
                    )
                })?;
                match route_fresh_metadata(
                    source,
                    self.context.supported,
                    limits.wheel,
                    limits.metadata,
                )
                .map_err(RegistryDepotError::FreshWheel)?
                {
                    FreshMetadataState::NeedsBuild(source) => {
                        MetadataObservation::Build(BuildOwner::Fresh(source))
                    }
                    FreshMetadataState::Wheel(wheel) => {
                        let raw = u64::try_from(wheel.manifest().core_metadata().raw().len())
                            .map_err(|_| RegistryDepotError::Limit)?;
                        let wire = raw.min(limits.source.transfer.max_wire_bytes);
                        let spool = wire.checked_mul(2).ok_or(RegistryDepotError::Limit)?;
                        let additional =
                            match wheel.file_facts().core_metadata() {
                                pakro_index::simple::CoreMetadataAvailability::Available {
                                    hashes,
                                } if hashes.preferred_digest().is_some() => spool,
                                _ => 0,
                            };
                        if self
                            .retained_bytes
                            .checked_add(wheel.pin().size())
                            .and_then(|n| n.checked_add(additional))
                            .is_none_or(|n| n > limits.max_retained_bytes)
                        {
                            return Err(RegistryDepotError::Limit);
                        }
                        let transfer = SourceFileLimits {
                            max_wire_bytes: wire,
                            ..limits.source.transfer
                        };
                        let wheel = verify_advertised_core_metadata(
                            wheel,
                            self.context.client,
                            file_session(&endpoint.file),
                            self.context.fetch.network,
                            SidecarLimits {
                                transfer,
                                max_spool_bytes: limits.source.max_spool_bytes.min(spool),
                                max_identity_bytes: limits.source.max_identity_bytes,
                                deadline: self.context.deadline,
                            },
                            self.context.scratch_root,
                        )
                        .await
                        .map_err(|error| {
                            stage_error(self.context.deadline, RegistryDepotError::Sidecar(error))
                        })?;
                        MetadataObservation::Wheel(RegistryMetadataOwner::Fresh(wheel))
                    }
                }
            }
            FilePinLookup::Existing(slot) => {
                let source = acquire_selected_existing_registry_file(
                    slot,
                    &selected,
                    LockedReplayContext {
                        client: self.context.client,
                        project_session: project_session(&endpoint.project),
                        file_session: file_session(&endpoint.file),
                        policy: self.context.fetch,
                        project_limits: limits.project,
                        source_limits: limits.source,
                        lock_limits: limits.lock,
                        cache_root: self.context.cache_root,
                        scratch_root: self.context.scratch_root,
                        deadline: self.context.deadline,
                    },
                )
                .await
                .map_err(|error| {
                    stage_error(
                        self.context.deadline,
                        RegistryDepotError::LockedAcquisition(error),
                    )
                })?;
                match route_locked_metadata(
                    source,
                    self.context.supported,
                    limits.wheel,
                    limits.metadata,
                )
                .map_err(RegistryDepotError::LockedWheel)?
                {
                    LockedMetadataState::NeedsBuild(source) => {
                        MetadataObservation::Build(BuildOwner::Locked(source))
                    }
                    LockedMetadataState::Wheel(wheel) => {
                        MetadataObservation::Wheel(RegistryMetadataOwner::Locked(wheel))
                    }
                }
            }
            FilePinLookup::Missing => return Err(RegistryDepotError::IdentityMismatch),
        };
        self.check_live()?;
        let size = match &observation {
            MetadataObservation::Wheel(owner) => owner.pin().size(),
            MetadataObservation::Build(BuildOwner::Fresh(owner)) => owner.size(),
            MetadataObservation::Build(BuildOwner::Locked(owner)) => owner.size(),
        };
        self.retained_bytes = self
            .retained_bytes
            .checked_add(size)
            .filter(|n| *n <= limits.max_retained_bytes)
            .ok_or(RegistryDepotError::Limit)?;
        if let MetadataObservation::Wheel(owner) = &observation {
            self.charge_metadata(owner)?;
        }
        Ok(observation)
    }

    fn charge_metadata(&mut self, owner: &RegistryMetadataOwner<'_>) -> RegistryDepotResult<()> {
        // Count the complete raw observation, including Description. The
        // existing A limits bound the single-step parse peak; this is a
        // logical retained-payload budget, not an allocator/RSS claim.
        self.charge(owner.manifest().core_metadata().raw().len())?;
        // A retains parsed headers alongside their raw bytes. Reserving
        // one additional raw extent conservatively covers their text,
        // including unknown header names and values, without reopening A.
        self.charge(owner.manifest().core_metadata().raw().len())?;
        self.charge(owner.manifest().core_metadata().description().len())?;
        for value in [
            owner.facts().name().as_str(),
            owner.facts().version(),
            owner.facts().requires_python().unwrap_or_default(),
            owner.pin().file().registry_id(),
            owner.pin().file().project(),
            owner.pin().file().version(),
            owner.pin().file().filename(),
            owner.manifest().name(),
            owner.manifest().version(),
            owner.manifest().dist_info(),
        ] {
            self.charge(value.len())?;
        }
        for declaration in owner.facts().requirements() {
            self.charge(declaration.raw_for_semantic_resolution().len())?;
            self.charge(declaration.name().len())?;
            for extra in declaration.requested_extras() {
                self.charge(extra.len())?;
            }
            if let Some(reference) = declaration.unresolved_reference() {
                self.charge(reference.raw_for_source_binding().len())?;
            }
            if let Some(marker) = declaration.exact_marker() {
                self.charge(marker.raw_for_semantic_resolution().len())?;
            }
        }
        for extra in owner.facts().provided_extras() {
            self.charge(extra.len())?;
        }
        for field in owner.facts().dynamic_fields() {
            self.charge(field.len())?;
        }
        for member in owner.manifest().members() {
            self.charge(member.archive_path().len())?;
            self.charge(member.relative_path().len())?;
        }
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> RegistryDepotResult<()> {
        self.retained_utf8 = self
            .retained_utf8
            .checked_add(bytes)
            .filter(|n| *n <= self.context.limits.max_utf8_bytes)
            .ok_or(RegistryDepotError::Limit)?;
        Ok(())
    }
    fn charge_project(&mut self, owner: &BoundRegistryProject) -> RegistryDepotResult<()> {
        let project = &owner.data.project;
        for value in [
            owner.data.registry.id().as_str(),
            owner.data.registry.canonical_base().as_url().as_str(),
            owner
                .data
                .registry
                .credential_ref()
                .map_or("", |value| value.as_str()),
        ] {
            self.charge(value.len())?;
        }
        self.charge(project.name().as_str().len())?;
        self.charge(project.source().as_str().len())?;
        for version in project.versions() {
            self.charge(version.len())?;
        }
        for file in project.files() {
            self.charge(file.filename().len())?;
            self.charge(file.locator().url().as_str().len())?;
            for value in [
                file.version(),
                file.requires_python(),
                file.yanked().reason(),
                file.upload_time(),
            ]
            .into_iter()
            .flatten()
            {
                self.charge(value.len())?;
            }
            if let Some(value) = file.provenance() {
                self.charge(value.url().as_str().len())?;
            }
            for (algorithm, digest) in file.hashes().entries() {
                self.charge(algorithm.len())?;
                self.charge(digest.len())?;
            }
            if let pakro_index::simple::CoreMetadataAvailability::Available { hashes } =
                file.core_metadata()
            {
                for (algorithm, digest) in hashes.entries() {
                    self.charge(algorithm.len())?;
                    self.charge(digest.len())?;
                }
            }
            if let PythonDistributionKind::Wheel(wheel) = file.kind() {
                self.charge(wheel.name.len())?;
                self.charge(wheel.version.len())?;
                for value in wheel
                    .tags
                    .python
                    .iter()
                    .chain(&wheel.tags.abi)
                    .chain(&wheel.tags.platform)
                {
                    self.charge(value.len())?;
                }
                if let Some(build) = &wheel.build {
                    for value in [build.raw(), build.number(), build.suffix()] {
                        self.charge(value.len())?;
                    }
                }
            }
        }
        for locator in project
            .relations()
            .tracks()
            .iter()
            .chain(project.relations().alternate_locations())
        {
            self.charge(locator.url().as_str().len())?;
        }
        if let Some(status) = project.status() {
            self.charge(status.raw().len())?;
            if let Some(reason) = status.reason() {
                self.charge(reason.len())?;
            }
        }
        Ok(())
    }

    fn finish(
        mut self,
        plan: NativePlan,
        policy_notices: BTreeMap<CandidateKey, Vec<PolicyNotice>>,
    ) -> RegistryDepotResult<RegistryPreparation<'root, 'config>> {
        self.check_live()?;
        let usage = RegistryDepotUsage {
            retained_bytes: self.retained_bytes,
            retained_utf8: self.retained_utf8,
            steps: self.steps,
            projects: self.projects.len(),
            metadata: self.metadata.len(),
            policy_work: self.policy_usage.usage().0,
            policy_utf8: self.policy_usage.usage().1,
        };
        let mut owners = BTreeMap::new();
        for selected in &plan.selected {
            let entry = self
                .metadata
                .remove(&selected.key)
                .ok_or(RegistryDepotError::IdentityMismatch)?;
            if entry.owner.pin().file().project() != selected.name {
                return Err(RegistryDepotError::IdentityMismatch);
            }
            entry.owner.recheck_for(self.root)?;
            if owners.insert(selected.key, entry.owner).is_some() {
                return Err(RegistryDepotError::IdentityMismatch);
            }
        }
        self.check_live()?;
        Ok(RegistryPreparation::Ready(RegistryPreparedPlan {
            root: self.root,
            roots: self.roots,
            root_scopes: self.root_scopes,
            environment: self.environment,
            root_scope: self.context.root_scope,
            semantics: self.context.semantics,
            supported: self.context.supported.clone(),
            deadline: self.context.deadline,
            marker_limits: self.context.limits.markers,
            plan,
            owners,
            policy_notices,
            usage,
        }))
    }
}

fn project_session<'a>(session: &PythonSourceSession<'a>) -> PythonSourceSession<'a> {
    PythonSourceSession {
        registry: session.registry,
        auth_scope: session.auth_scope,
        credentials: session.credentials,
        auth_origins: session.auth_origins,
        transport: session.transport,
    }
}
fn file_session<'a>(session: &SourceFileSession<'a>) -> SourceFileSession<'a> {
    SourceFileSession {
        credential_ref: session.credential_ref,
        auth_scope: session.auth_scope,
        credentials: session.credentials,
        auth_origins: session.auth_origins,
        transport: session.transport,
    }
}
fn stage_error(deadline: Instant, fallback: RegistryDepotError) -> RegistryDepotError {
    if Instant::now() >= deadline {
        RegistryDepotError::Deadline
    } else {
        fallback
    }
}
enum MetadataObservation<'root> {
    Wheel(RegistryMetadataOwner<'root>),
    Build(BuildOwner<'root>),
}
impl fmt::Debug for ResolveRegistryDepot<'_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ResolveRegistryDepot")
            .field("roots", &self.roots.len())
            .field("projects", &self.projects.len())
            .field("metadata", &self.metadata.len())
            .finish_non_exhaustive()
    }
}
impl MetadataDepot for ResolveRegistryDepot<'_, '_> {
    fn project(&self, name: &str) -> Option<&[CandidateVersion]> {
        self.projects
            .get(name)
            .map(|entry| entry.candidates.as_slice())
    }
    fn metadata(&self, key: CandidateKey) -> Option<&CandidateMetadata> {
        self.metadata.get(&key).map(|entry| &entry.input)
    }
}

pub(crate) struct RegistryPreparedPlan<'root> {
    root: &'root BoundPythonLock,
    roots: Vec<PythonRequirementSyntax>,
    root_scopes: Option<Vec<RegistryRootScope>>,
    environment: RegistryEnvironment,
    root_scope: RegistryRootScope,
    semantics: MarkerSemantics,
    supported: SupportedWheelTags,
    deadline: Instant,
    marker_limits: PythonMarkerLimits,
    plan: NativePlan,
    owners: BTreeMap<CandidateKey, RegistryMetadataOwner<'root>>,
    policy_notices: BTreeMap<CandidateKey, Vec<PolicyNotice>>,
    usage: RegistryDepotUsage,
}
impl<'root> RegistryPreparedPlan<'root> {
    pub(in crate::packages::python_resolution::source) fn from_replayed(
        root: &'root BoundPythonLock,
        roots: Vec<PythonRequirementSyntax>,
        root_scopes: Option<Vec<RegistryRootScope>>,
        target: super::locked_graph::FrozenRegistryTarget,
        owners: BTreeMap<CandidateKey, RegistryMetadataOwner<'root>>,
        derived: crate::packages::python_resolution::graph_check::DerivedGraphData,
        deadline: Instant,
        retained_utf8: usize,
    ) -> Result<Self, super::locked_graph::LockedGraphError> {
        use super::locked_graph::LockedGraphError;
        use pakro_resolver::python_native::SelectedCandidate;
        root.recheck().map_err(|_| LockedGraphError::Revoked)?;
        let mut selected = owners
            .iter()
            .map(|(key, owner)| SelectedCandidate {
                name: owner.facts().name().as_str().to_owned(),
                key: *key,
                version: owner.facts().version().to_owned(),
                requested_extras: derived.extras.get(key).cloned().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        selected.sort_by(|a, b| a.name.cmp(&b.name));
        let retained_bytes = owners.values().try_fold(0u64, |total, owner| {
            total
                .checked_add(owner.pin().size())
                .and_then(|n| {
                    if owner.build_record().is_some() {
                        n.checked_add(owner.output_size())
                    } else {
                        Some(n)
                    }
                })
                .ok_or(LockedGraphError::Limit)
        })?;
        let metadata_count = owners.len();
        Ok(Self {
            root,
            roots,
            root_scopes,
            environment: RegistryEnvironment::from_checked(target.environment()),
            root_scope: target.with_root_scope(|scope| match scope {
                PythonMarkerScope::Environment => RegistryRootScope::Environment,
                PythonMarkerScope::Dependency { requested_extras } => {
                    RegistryRootScope::ProjectExtras(
                        requested_extras
                            .iter()
                            .map(|value| (*value).to_owned())
                            .collect(),
                    )
                }
                PythonMarkerScope::Lock {
                    selected_extras,
                    dependency_groups,
                } => RegistryRootScope::Lock {
                    extras: selected_extras
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                    groups: dependency_groups
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                },
            }),
            semantics: target.semantics(),
            supported: target.supported().clone(),
            deadline,
            marker_limits: Default::default(),
            plan: NativePlan { selected },
            owners,
            policy_notices: BTreeMap::new(),
            usage: RegistryDepotUsage {
                retained_bytes,
                retained_utf8,
                steps: 0,
                projects: 0,
                metadata: metadata_count,
                policy_work: 0,
                policy_utf8: 0,
            },
        })
    }
}
impl RegistryPreparedPlan<'_> {
    pub(crate) fn policy_notices(&self, key: CandidateKey) -> &[PolicyNotice] {
        self.policy_notices
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn roots(&self) -> &[PythonRequirementSyntax] {
        &self.roots
    }
    pub(crate) fn environment(&self) -> PythonMarkerEnvironment<'_> {
        self.environment.view()
    }
    pub(crate) fn root_scope(&self) -> &RegistryRootScope {
        &self.root_scope
    }
    pub(crate) fn with_root_scopes<T>(
        &self,
        use_scopes: impl FnOnce(
            crate::packages::python_resolution::graph_check::RootMarkerScopes<'_>,
        ) -> T,
    ) -> T {
        use crate::packages::python_resolution::graph_check::RootMarkerScopes;
        match &self.root_scopes {
            Some(scopes) => use_scopes(RootMarkerScopes::OwnedDeclarations(scopes)),
            None => self
                .root_scope
                .with_scope(|scope| use_scopes(RootMarkerScopes::Uniform(scope))),
        }
    }
    pub(crate) fn semantics(&self) -> MarkerSemantics {
        self.semantics
    }
    pub(crate) fn supported(&self) -> &SupportedWheelTags {
        &self.supported
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn marker_limits(&self) -> PythonMarkerLimits {
        self.marker_limits
    }
    pub(crate) fn plan(&self) -> &NativePlan {
        &self.plan
    }
    pub(crate) fn usage(&self) -> RegistryDepotUsage {
        self.usage
    }
    pub(crate) fn owner(&self, key: CandidateKey) -> Option<&RegistryMetadataOwner<'_>> {
        self.owners.get(&key)
    }
    pub(crate) fn root(&self) -> &BoundPythonLock {
        self.root
    }
    pub(crate) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        if !std::ptr::eq(self.root, root) {
            return Err(RegistryDepotError::Revoked);
        }
        self.root
            .recheck()
            .map_err(|_| RegistryDepotError::Revoked)?;
        for owner in self.owners.values() {
            owner.recheck_for(root)?;
        }
        Ok(())
    }
}
impl fmt::Debug for RegistryPreparedPlan<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("RegistryPreparedPlan")
            .field("plan", &self.plan)
            .field("original_roots", &self.roots.len())
            .finish_non_exhaustive()
    }
}

enum BuildOwner<'root> {
    Fresh(FreshPinnedArtifact<'root>),
    Locked(LockedRegistryArtifact<'root>),
}
pub(crate) struct RegistryBuildContinuation<'root, 'config> {
    depot: ResolveRegistryDepot<'root, 'config>,
    candidate: CandidateKey,
    source: BuildOwner<'root>,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct RegistryDepotUsage {
    pub retained_bytes: u64,
    pub retained_utf8: usize,
    pub steps: usize,
    pub projects: usize,
    pub metadata: usize,
    pub policy_work: usize,
    pub policy_utf8: usize,
}
impl<'root, 'config> RegistryBuildContinuation<'root, 'config> {
    pub(crate) fn pin(&self) -> &ContentPin {
        match &self.source {
            BuildOwner::Fresh(value) => value.pin(),
            BuildOwner::Locked(value) => value.pin(),
        }
    }
    pub(crate) fn candidate(&self) -> CandidateKey {
        self.candidate
    }
    pub(crate) fn registry_context(&self) -> RegistryDepotContext<'config> {
        self.depot.context.clone()
    }
    pub(crate) fn usage(&self) -> RegistryDepotUsage {
        RegistryDepotUsage {
            retained_bytes: self.depot.retained_bytes,
            retained_utf8: self.depot.retained_utf8,
            steps: self.depot.steps,
            projects: self.depot.projects.len(),
            metadata: self.depot.metadata.len(),
            policy_work: self.depot.policy_usage.usage().0,
            policy_utf8: self.depot.policy_usage.usage().1,
        }
    }
    pub(crate) fn source_verified_copy(
        &mut self,
        scratch: &Path,
    ) -> RegistryDepotResult<crate::artifact_snapshot::VerifiedArtifact> {
        self.depot.check_live()?;
        self.source.recheck_for(self.depot.root)?;
        let pin = self.pin().clone();
        let retained = self
            .depot
            .retained_bytes
            .checked_add(pin.size())
            .filter(|n| *n <= self.depot.context.limits.max_retained_bytes)
            .ok_or(RegistryDepotError::Limit)?;
        self.source
            .seek(SeekFrom::Start(0))
            .map_err(|_| RegistryDepotError::Build)?;
        let copy = crate::artifact_snapshot::spool_verified_reader(
            super::caller_deadline::Reader::new(
                &mut self.source,
                Some(self.depot.context.deadline),
            ),
            pin.file().filename(),
            pin.digest(),
            self.depot.context.limits.source.transfer.max_wire_bytes,
            scratch,
        )
        .map_err(|_| stage_error(self.depot.context.deadline, RegistryDepotError::Build))?;
        self.depot.check_live()?;
        self.source.recheck_for(self.depot.root)?;
        if copy.size() != pin.size() {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        self.depot.retained_bytes = retained;
        Ok(copy)
    }
}
pub(crate) struct RegistrySourceContinuation<'root, 'config> {
    depot: ResolveRegistryDepot<'root, 'config>,
    declaration: String,
    request: RegistrySourceRequest,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistrySourceRequest {
    Root {
        declaration: usize,
    },
    Dependency {
        parent: CandidateKey,
        declaration: usize,
    },
}
impl RegistrySourceContinuation<'_, '_> {
    pub(crate) fn declaration_for_source_binding(&self) -> &str {
        &self.declaration
    }
    pub(crate) fn request(&self) -> RegistrySourceRequest {
        self.request
    }
}
pub(crate) struct RegistryPolicyContinuation<'root, 'config> {
    depot: ResolveRegistryDepot<'root, 'config>,
}
pub(crate) enum RegistryPreparation<'root, 'config> {
    Ready(RegistryPreparedPlan<'root>),
    NeedsBuild(RegistryBuildContinuation<'root, 'config>),
    NeedsSourceAdapter(RegistrySourceContinuation<'root, 'config>),
    NeedsConstraintPolicy(RegistryPolicyContinuation<'root, 'config>),
}
impl fmt::Debug for RegistryPreparation<'_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Ready(_) => "RegistryPreparation::Ready",
            Self::NeedsBuild(_) => "RegistryPreparation::NeedsBuild",
            Self::NeedsSourceAdapter(_) => "RegistryPreparation::NeedsSourceAdapter",
            Self::NeedsConstraintPolicy(_) => "RegistryPreparation::NeedsConstraintPolicy",
        })
    }
}

#[cfg(test)]
#[path = "registry_depot/tests.rs"]
mod tests;
