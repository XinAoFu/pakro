//! Explicit installation orchestration for one exact Python request (F2).
//!
//! Consumes an R01-normalized request that passed the F1 exact-version gate
//! and one frozen OperationContext; the caller's context is never rebuilt.
//! An already-published exact instance is reused without acquisition; a
//! missing one is fetched through the caller-provided acquire closure (the
//! CLI provides the real native fetch; tests inject counters and fixtures)
//! and published through the R06 core. A project observation links the
//! success through F1's selection commit; a global install publishes
//! content without fabricating a project.

use std::future::Future;

use eyre::{ContextCompat, Result, ensure};

use crate::artifact_snapshot::VerifiedArtifact;
use crate::libraries::language::identity::LanguageKey;
use crate::libraries::language::repository::{
    LanguageEnumerateError, LanguageInstallSpec, LanguageRepository,
};
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::programs::install::runtime::prepare_language;
use crate::programs::install::selection::apply_directory_selection;
use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
use crate::programs::shared::context::OperationContext;
use crate::programs::shared::exact_version::ExactPythonRequest;
use crate::programs::shared::report::{OperationEffects, OperationReport, OperationState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallSubject {
    Language(LanguageKey),
    Project(ProjectIdentity),
}

pub(crate) async fn install_exact_python<F, Fut>(
    context: &OperationContext,
    request: &ExactPythonRequest,
    project: Option<&ObservedProjectInputs>,
    acquire: F,
) -> Result<OperationReport<InstallSubject>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<VerifiedArtifact>>,
{
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids language library writes"
    );
    let languages = LanguageRepository::open_readonly(context)?;
    let published = match languages.enumerate_published() {
        Ok(published) => published,
        Err(LanguageEnumerateError::Unreadable(error)) => {
            return Err(eyre::eyre!(
                "language library state is unreadable: {error:#}"
            ));
        }
        Err(LanguageEnumerateError::Corrupt(detail)) => {
            return Err(eyre::eyre!("language library state is damaged: {detail}"));
        }
    };
    let matching: Vec<_> = published
        .iter()
        .filter(|language| {
            language.version() == request.as_str() && language.matches_host_platform()
        })
        .collect();
    if matching.len() > 1 {
        eyre::bail!(
            "exact request {} matches {} published instances; pin one explicitly",
            request.as_str(),
            matching.len()
        );
    }
    if let Some(existing) = matching.first() {
        // Reuse: an existing published instance is acquired as-is; the
        // caller's acquire closure is never invoked.
        let lease = languages
            .acquire_by_key(&existing.key())?
            .context("the reused language instance is not Ready")?;
        lease.check_binding()?;
        return finish(context, project, existing.key(), true).await;
    }
    let artifact = acquire().await?;
    context.checkpoint()?;
    let spec = spec_from_artifact(&artifact, request)?;
    let lease = prepare_language(&spec, context, || async { Ok(artifact) }).await?;
    lease.check_binding()?;
    finish(context, project, spec.key(), false).await
}

/// Publishes the report and, for a project install, links the success
/// through the same controlled selection commit F1 uses.
async fn finish(
    context: &OperationContext,
    project: Option<&ObservedProjectInputs>,
    key: LanguageKey,
    reused: bool,
) -> Result<OperationReport<InstallSubject>> {
    let mut associations = Vec::new();
    if let Some(observed) = project {
        observed.recheck()?;
        let query = open_query_context(context, observed)?;
        match select_existing(&query)? {
            SelectionReport::Selected(candidate) => {
                ensure!(
                    candidate.key() == &key,
                    "the installed instance differs from the project's resolved selection"
                );
                apply_directory_selection(context, observed, &candidate)?;
                associations.push(InstallSubject::Project(ProjectIdentity::from_observation(
                    observed,
                )?));
            }
            other => {
                eyre::bail!("the project selection did not resolve after install: {other:?}");
            }
        }
    }
    let effects = OperationEffects {
        added: (!reused)
            .then(|| vec![InstallSubject::Language(key)])
            .unwrap_or_default(),
        reused: reused
            .then(|| vec![InstallSubject::Language(key)])
            .unwrap_or_default(),
        associations_changed: associations,
        retained: Vec::new(),
        removed: Vec::new(),
    };
    Ok(OperationReport::new(
        OperationState::Committed,
        effects,
        Vec::new(),
    ))
}

/// Rebuilds the install spec from the verified artifact's own filename and
/// digest; the exact request must match the acquired archive's version.
fn spec_from_artifact(
    artifact: &VerifiedArtifact,
    request: &ExactPythonRequest,
) -> Result<LanguageInstallSpec> {
    let filename = artifact.filename();
    let Some((version, build, target, flavor)) =
        crate::plugins::core::python::native::pbs_filename(filename)?
    else {
        eyre::bail!("acquired runtime {filename:?} is not an official PBS archive");
    };
    ensure!(
        version == request.as_str(),
        "acquired archive {filename:?} does not match the exact requested version {}",
        request.as_str()
    );
    let platform = crate::lockfile::PlatformInfo {
        url: Some(format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/{build}/{filename}"
        )),
        checksum: Some(artifact.digest().to_string().replacen('-', ":", 1)),
        ..Default::default()
    };
    let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_locked(
        &version, &target, flavor, &platform,
    )?;
    LanguageInstallSpec::from_plan(&plan, false)
}

#[cfg(test)]
mod tests;
