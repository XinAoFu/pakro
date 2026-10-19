//! Actual file-bound declaration input to the existing registry coordinator.

#[path = "resolve/build.rs"]
pub(crate) mod build;

use super::*;
use crate::packages::python_resolution::graph_check::GraphCheckLimits;
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::source::registry_depot::closed::{
    ClosedRegistryGraph, RegistryClosureError, close_registry_plan,
};
use crate::packages::python_resolution::source::registry_depot::{
    RegistryDepotContext, RegistryDepotError, RegistryPreparation, RegistryRootScope,
    ResolveRegistryDepot,
};

pub(crate) type ProjectRegistryResult<T> = Result<T, ProjectRegistryError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ProjectRegistryError {
    #[error("project declaration file is no longer bound: {0}")]
    File(PythonProjectFileError),
    #[error("project selection and target marker context differ")]
    SelectionContext,
    #[error("project Python compatibility context is invalid")]
    PythonContext,
    #[error("project requires another Python language version")]
    RequiresPython,
    #[error("project registry deadline expired")]
    Deadline,
    #[error("project registry lock observation was revoked")]
    Revoked,
    #[error("project root marker is invalid in its declaration context")]
    RootMarker,
    #[error("project registry coordination failed: {0}")]
    Registry(RegistryDepotError),
    #[error("project registry closure failed: {0}")]
    Closure(RegistryClosureError),
}

struct ProjectBinding<'file, 'root> {
    selection: BoundPythonDeclarationSelection<'file>,
    root: &'root BoundPythonLock,
    deadline: Instant,
}
impl ProjectBinding<'_, '_> {
    fn recheck_for(
        &self,
        file: &BoundPythonProjectFile,
        root: &BoundPythonLock,
    ) -> ProjectRegistryResult<()> {
        project_deadline(self.deadline)?;
        self.selection
            .recheck_for(file)
            .map_err(ProjectRegistryError::File)?;
        if !std::ptr::eq(self.root, root) {
            return Err(ProjectRegistryError::Revoked);
        }
        root.recheck().map_err(|_| ProjectRegistryError::Revoked)?;
        project_deadline(self.deadline)
    }
}

pub(crate) struct ClosedPythonProjectRegistry<'file, 'root> {
    binding: ProjectBinding<'file, 'root>,
    graph: ClosedRegistryGraph<'root>,
}
impl ClosedPythonProjectRegistry<'_, '_> {
    pub(crate) fn graph(&self) -> &ClosedRegistryGraph<'_> {
        &self.graph
    }
    pub(crate) fn recheck_for(
        &self,
        file: &BoundPythonProjectFile,
        root: &BoundPythonLock,
    ) -> ProjectRegistryResult<()> {
        self.binding.recheck_for(file, root)?;
        self.graph
            .recheck_for(root)
            .map_err(|_| ProjectRegistryError::Revoked)
    }
}
pub(crate) struct ProjectMetadataContinuation<'file, 'root, 'config> {
    binding: ProjectBinding<'file, 'root>,
    context: RegistryDepotContext<'config>,
    closure_limits: GraphCheckLimits,
}
pub(crate) struct ProjectLocalContinuation<'file, 'root, 'config> {
    binding: ProjectBinding<'file, 'root>,
    depot: ResolveRegistryDepot<'root, 'config>,
    closure_limits: GraphCheckLimits,
    declaration: usize,
}
impl ProjectLocalContinuation<'_, '_, '_> {
    pub(crate) fn declaration(&self) -> usize {
        self.declaration
    }
}
pub(crate) struct ProjectRegistryContinuation<'file, 'root, 'config> {
    binding: ProjectBinding<'file, 'root>,
    registry: RegistryPreparation<'root, 'config>,
    closure_limits: GraphCheckLimits,
}
impl ProjectRegistryContinuation<'_, '_, '_> {
    pub(crate) fn registry(&self) -> &RegistryPreparation<'_, '_> {
        &self.registry
    }
}

pub(crate) enum ProjectRegistryPreparation<'file, 'root, 'config> {
    Closed(ClosedPythonProjectRegistry<'file, 'root>),
    NeedsMetadataWorker(ProjectMetadataContinuation<'file, 'root, 'config>),
    NeedsLocalProject(ProjectLocalContinuation<'file, 'root, 'config>),
    NeedsRegistry(ProjectRegistryContinuation<'file, 'root, 'config>),
}
impl ProjectRegistryPreparation<'_, '_, '_> {
    pub(crate) fn selection(&self) -> &BoundPythonDeclarationSelection<'_> {
        match self {
            Self::Closed(value) => &value.binding.selection,
            Self::NeedsMetadataWorker(value) => &value.binding.selection,
            Self::NeedsLocalProject(value) => &value.binding.selection,
            Self::NeedsRegistry(value) => &value.binding.selection,
        }
    }
}
impl fmt::Debug for ProjectRegistryPreparation<'_, '_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Closed(_) => "ProjectRegistryPreparation::Closed",
            Self::NeedsMetadataWorker(_) => "ProjectRegistryPreparation::NeedsMetadataWorker",
            Self::NeedsLocalProject(_) => "ProjectRegistryPreparation::NeedsLocalProject",
            Self::NeedsRegistry(_) => "ProjectRegistryPreparation::NeedsRegistry",
        })
    }
}

use crate::packages::python_declarations::PythonDeclarationOrigin;
use crate::packages::python_resolution::metadata_input::checked_metadata_context;
use pakro_index::python_markers::evaluate_python_marker;
use pakro_index::python_requires_matches_for_context;

fn project_deadline(deadline: Instant) -> ProjectRegistryResult<()> {
    if Instant::now() >= deadline {
        return Err(ProjectRegistryError::Deadline);
    }
    Ok(())
}
fn canonical_choices(
    values: &[String],
    context: &RegistryDepotContext<'_>,
) -> ProjectRegistryResult<Vec<String>> {
    if values.len() > context.limits.markers.max_set_entries {
        return Err(ProjectRegistryError::Registry(RegistryDepotError::Limit));
    }
    let mut bytes = 0usize;
    for value in values {
        if value.len() > context.limits.markers.max_environment_field_bytes {
            return Err(ProjectRegistryError::Registry(RegistryDepotError::Limit));
        }
        bytes = bytes
            .checked_add(value.len())
            .filter(|value| *value <= context.limits.max_utf8_bytes)
            .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(values.len())
        .map_err(|_| ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    result.extend(values.iter().cloned());
    result.sort_unstable();
    result.dedup();
    Ok(result)
}
fn scoped_roots(
    selection: &BoundPythonDeclarationSelection<'_>,
    extras: &[String],
    groups: &[String],
    context: &RegistryDepotContext<'_>,
) -> ProjectRegistryResult<Vec<RegistryRootScope>> {
    let requirements = selection.declarations().known().requirements();
    if requirements.len() > context.limits.max_roots {
        return Err(ProjectRegistryError::Registry(RegistryDepotError::Limit));
    }
    // Charge the scopes this adapter will own before cloning them. The depot
    // remains the sole validator of its full environment/source/input budgets.
    let extra_bytes = extras
        .iter()
        .try_fold(0usize, |used, name| used.checked_add(name.len()))
        .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    let mut retained = groups
        .iter()
        .try_fold(extra_bytes, |used, name| used.checked_add(name.len()))
        .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    let mut references = extras
        .len()
        .checked_add(groups.len())
        .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    for requirement in requirements {
        retained = retained
            .checked_add(requirement.syntax().raw_for_semantic_resolution().len())
            .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
        if !matches!(requirement.origin(), PythonDeclarationOrigin::Group { .. }) {
            retained = retained
                .checked_add(extra_bytes)
                .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
            references = references
                .checked_add(extras.len())
                .ok_or(ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
        }
        if retained > context.limits.max_utf8_bytes
            || references > context.limits.solver.max_total_extra_references
        {
            return Err(ProjectRegistryError::Registry(RegistryDepotError::Limit));
        }
    }
    let mut scopes = Vec::new();
    scopes
        .try_reserve_exact(requirements.len())
        .map_err(|_| ProjectRegistryError::Registry(RegistryDepotError::Limit))?;
    for requirement in requirements {
        scopes.push(match requirement.origin() {
            PythonDeclarationOrigin::Project { .. } | PythonDeclarationOrigin::Extra { .. } => {
                RegistryRootScope::ProjectExtras(extras.to_vec())
            }
            PythonDeclarationOrigin::Group { .. } => RegistryRootScope::Environment,
        });
    }
    Ok(scopes)
}

pub(crate) async fn prepare_project_registry<'file, 'root, 'config>(
    selection: BoundPythonDeclarationSelection<'file>,
    actual_file: &'file BoundPythonProjectFile,
    root: &'root BoundPythonLock,
    context: RegistryDepotContext<'config>,
    closure_limits: GraphCheckLimits,
) -> ProjectRegistryResult<ProjectRegistryPreparation<'file, 'root, 'config>> {
    let binding = ProjectBinding {
        selection,
        root,
        deadline: context.deadline,
    };
    binding.recheck_for(actual_file, root)?;
    closure_limits
        .validate()
        .map_err(|error| ProjectRegistryError::Closure(RegistryClosureError::Graph(error)))?;
    let marker_context = checked_metadata_context(context.environment, context.limits.markers)
        .map_err(|_| ProjectRegistryError::PythonContext)?;
    let selected = binding.selection.declarations().known();
    let extras = canonical_choices(selected.extras(), &context)?;
    let groups = canonical_choices(selected.groups(), &context)?;
    if !matches!(&context.root_scope, RegistryRootScope::Lock { extras: expected_extras, groups: expected_groups }
        if expected_extras == &extras && expected_groups == &groups)
    {
        return Err(ProjectRegistryError::SelectionContext);
    }
    if let Some(specification) = actual_file
        .parsed()
        .project()
        .and_then(|project| project.requires_python())
    {
        if !python_requires_matches_for_context(specification, &marker_context)
            .map_err(|_| ProjectRegistryError::PythonContext)?
        {
            return Err(ProjectRegistryError::RequiresPython);
        }
    }
    binding.recheck_for(actual_file, root)?;
    if binding.selection.declarations().metadata_need().is_some() {
        return Ok(ProjectRegistryPreparation::NeedsMetadataWorker(
            ProjectMetadataContinuation {
                binding,
                context,
                closure_limits,
            },
        ));
    }
    let scopes = scoped_roots(&binding.selection, &extras, &groups, &context)?;
    let raw_roots = selected
        .requirements()
        .iter()
        .map(|requirement| requirement.syntax().raw_for_semantic_resolution())
        .collect::<Vec<_>>();
    let environment = context.environment;
    let semantics = context.semantics;
    let marker_limits = context.limits.markers;
    let depot = ResolveRegistryDepot::new_scoped(root, &raw_roots, &scopes, context)
        .map_err(ProjectRegistryError::Registry)?;
    let mut local_declaration = None;
    if let Some(project) = actual_file.parsed().project() {
        for (index, requirement) in selected.requirements().iter().enumerate() {
            if requirement.syntax().unresolved_reference().is_some()
                || requirement.syntax().name() != project.name().as_str()
            {
                continue;
            }
            let active = match requirement.syntax().marker() {
                None => true,
                Some(expression) => scopes[index]
                    .with_scope(|scope| {
                        evaluate_python_marker(
                            expression,
                            environment,
                            scope,
                            semantics,
                            marker_limits,
                        )
                    })
                    .map_err(|_| ProjectRegistryError::RootMarker)?
                    .value(),
            };
            if active {
                local_declaration = Some(index);
                break;
            }
        }
    }
    binding.recheck_for(actual_file, root)?;
    if let Some(declaration) = local_declaration {
        return Ok(ProjectRegistryPreparation::NeedsLocalProject(
            ProjectLocalContinuation {
                binding,
                depot,
                closure_limits,
                declaration,
            },
        ));
    }
    let preparation = depot
        .prepare()
        .await
        .map_err(ProjectRegistryError::Registry)?;
    binding.recheck_for(actual_file, root)?;
    match preparation {
        RegistryPreparation::Ready(prepared) => {
            let graph = close_registry_plan(prepared, closure_limits)
                .map_err(ProjectRegistryError::Closure)?;
            binding.recheck_for(actual_file, root)?;
            Ok(ProjectRegistryPreparation::Closed(
                ClosedPythonProjectRegistry { binding, graph },
            ))
        }
        registry => Ok(ProjectRegistryPreparation::NeedsRegistry(
            ProjectRegistryContinuation {
                binding,
                registry,
                closure_limits,
            },
        )),
    }
}

#[cfg(test)]
#[path = "resolve/tests.rs"]
mod tests;
