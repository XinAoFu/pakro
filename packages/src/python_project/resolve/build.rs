//! A fixed local-build consumer which keeps the actual declaration binding.

use super::*;
use crate::packages::python_environment::prefix::UnpublishedPrefix;
use crate::packages::python_environment::resolved_build::{
    PreparedResolvedEnvironment, ResolvedBuildError, ResolvedBuildLimits,
    prepare_resolved_environment,
};
use crate::packages::python_environment::runtime_startup::StartupAdapterBinding;
use crate::packages::python_environment::runtime_target::ObservedRegistryRuntimeTarget;
use pakro_resolver::python_native::NativePlan;
use tokio::time::Instant as TokioInstant;

pub(crate) type ProjectEnvironmentResult<T> = Result<T, ProjectEnvironmentError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ProjectEnvironmentError {
    #[error("project environment declaration binding failed: {0}")]
    Project(ProjectRegistryError),
    #[error("project environment build failed: {0}")]
    Build(ResolvedBuildError),
}

pub(crate) struct PreparedPythonProjectEnvironment<'file, 'root> {
    // Environment cleanup precedes release of the actual file/source evidence.
    environment: PreparedResolvedEnvironment<'root>,
    binding: ProjectBinding<'file, 'root>,
    deadline: TokioInstant,
}
impl PreparedPythonProjectEnvironment<'_, '_> {
    pub(crate) fn final_prefix(&self) -> &Path {
        self.environment.final_prefix()
    }
    pub(crate) fn executable(&self) -> &Path {
        self.environment.executable()
    }
    pub(crate) fn plan(&self) -> &NativePlan {
        self.environment.plan()
    }
    pub(crate) fn selection(&self) -> &BoundPythonDeclarationSelection<'_> {
        &self.binding.selection
    }
    pub(crate) fn recheck(&self) -> ProjectEnvironmentResult<()> {
        environment_deadline(self.deadline)?;
        current_project(&self.binding)?;
        self.environment
            .recheck_current()
            .map_err(ProjectEnvironmentError::Build)?;
        current_project(&self.binding)?;
        environment_deadline(self.deadline)
    }
}
impl fmt::Debug for PreparedPythonProjectEnvironment<'_, '_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("PreparedPythonProjectEnvironment")
            .field("selected", &self.plan().selected.len())
            .finish_non_exhaustive()
    }
}
impl<'file, 'root> ClosedPythonProjectRegistry<'file, 'root> {
    pub(crate) async fn prepare_environment(
        self,
        runtime: ObservedRegistryRuntimeTarget,
        destination: UnpublishedPrefix,
        startup: StartupAdapterBinding,
        limits: ResolvedBuildLimits,
        deadline: TokioInstant,
    ) -> ProjectEnvironmentResult<PreparedPythonProjectEnvironment<'file, 'root>> {
        self.prepare_with_finish(runtime, destination, startup, limits, deadline, |_| {})
            .await
    }
    async fn prepare_with_finish(
        self,
        runtime: ObservedRegistryRuntimeTarget,
        destination: UnpublishedPrefix,
        startup: StartupAdapterBinding,
        limits: ResolvedBuildLimits,
        deadline: TokioInstant,
        finish: impl FnOnce(&Path),
    ) -> ProjectEnvironmentResult<PreparedPythonProjectEnvironment<'file, 'root>> {
        let ClosedPythonProjectRegistry { binding, graph } = self;
        let deadline = deadline.min(TokioInstant::from_std(binding.deadline));
        environment_deadline(deadline)?;
        current_project(&binding)?;
        let environment =
            prepare_resolved_environment(graph, runtime, destination, startup, limits, deadline)
                .await
                .map_err(ProjectEnvironmentError::Build)?;
        finish(environment.final_prefix());
        let prepared = PreparedPythonProjectEnvironment {
            environment,
            binding,
            deadline,
        };
        prepared.recheck()?;
        Ok(prepared)
    }
}
fn current_project(binding: &ProjectBinding<'_, '_>) -> ProjectEnvironmentResult<()> {
    binding
        .recheck_for(binding.selection.owner, binding.root)
        .map_err(ProjectEnvironmentError::Project)
}
fn environment_deadline(deadline: TokioInstant) -> ProjectEnvironmentResult<()> {
    if TokioInstant::now() >= deadline {
        return Err(ProjectEnvironmentError::Project(
            ProjectRegistryError::Deadline,
        ));
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
#[path = "build/tests.rs"]
mod tests;
