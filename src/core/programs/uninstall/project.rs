//! Explicitly release one catalog identity without touching project files.

use eyre::{Result, ensure};
use pakro_store::control::ControlLimits;
use uuid::Uuid;

use crate::libraries::project::{
    identity::ProjectIdentity, repository::ProjectRepository, state::ExpectedProjectState,
};
use crate::packages::python_environment::model::{CatalogModel, InputKind};
use crate::programs::{OperationContext, library::ProjectForgetReport, shared};

pub(crate) fn forget(
    context: &OperationContext,
    identity: &str,
    dry_run: bool,
) -> Result<ProjectForgetReport> {
    let project = Uuid::parse_str(identity).map_err(|_| {
        eyre::eyre!("project forget requires the exact ID shown by pakro project ls")
    })?;
    ensure!(
        !project.is_nil() && project.hyphenated().to_string() == identity,
        "project forget requires a complete lowercase hyphenated project ID"
    );
    context.checkpoint()?;
    shared::lock_transaction::ensure_identity_consistent(context, project)?;
    let repository = ProjectRepository::open_readonly(context)?;
    let report = describe(repository.model(), project, dry_run)?;
    if dry_run || report.already_forgotten {
        return Ok(report);
    }
    ensure!(
        context.policy().writes.project_selection,
        "operation policy forbids releasing project selections"
    );
    let identity = ProjectIdentity::from_uuid(project);
    let expected = ExpectedProjectState::from_model(repository.model(), identity);
    drop(repository);

    let mut repository = ProjectRepository::open_writer(context)?;
    let writer = repository.writer()?;
    let lock = shared::commit::open_existing_project_lock(&writer, &identity)?;
    let guard = writer
        .control(ControlLimits::default())?
        .acquire_project(&lock)?;
    shared::lock_transaction::ensure_identity_consistent(context, project)?;
    repository.reload()?;
    let current = describe(repository.model(), project, dry_run)?;
    // Concurrent identical forgets are idempotent, while a new registration or
    // any changed target/group must be admitted as a fresh explicit operation.
    if current.already_forgotten {
        return Ok(current);
    }
    expected.ensure_current(repository.model())?;
    let active: Vec<_> = repository
        .model()
        .selection_slots()
        .filter(|(key, stamp)| {
            key.project() == project && (stamp.language().is_some() || stamp.generation().is_some())
        })
        .map(|(key, stamp)| (key.clone(), stamp.clone()))
        .collect();
    for (key, stamp) in active {
        repository
            .model_mut()
            .apply_selection(&key, &stamp, None, Uuid::now_v7())?;
    }
    ensure!(
        repository.model_mut().unregister_project(project)?,
        "project registration disappeared"
    );
    context.checkpoint()?;
    guard.check_binding()?;
    lock.check_location(
        writer.build_handle(),
        &format!("project-{}.lock", project.hyphenated()),
    )?;
    shared::lock_transaction::ensure_identity_consistent(context, project)?;
    repository.commit_model()?;
    Ok(ProjectForgetReport {
        forgotten: true,
        ..current
    })
}

fn describe(model: &CatalogModel, project: Uuid, dry_run: bool) -> Result<ProjectForgetReport> {
    let registration = model.project_registration(project);
    let mut associations = Vec::new();
    for (key, stamp) in model
        .selection_slots()
        .filter(|(key, _)| key.project() == project)
    {
        if stamp.language().is_none() && stamp.generation().is_none() {
            continue;
        }
        ensure!(
            registration.is_some(),
            "project catalog has active selections without a registration"
        );
        let prefix = format!("group={} target={}", key.group(), hex::encode(key.target()));
        if let Some(language) = stamp.language() {
            associations.push(format!("{prefix} -> Python {language}"));
        }
        if let Some(allocation) = stamp.generation() {
            let record = model
                .generation(allocation)
                .ok_or_else(|| eyre::eyre!("project catalog selected layout is missing"))?;
            associations.push(format!("{prefix} -> layout {allocation}"));
            for input in record.inputs() {
                let kind = match input.kind() {
                    InputKind::Language => "Python",
                    InputKind::Package => "package",
                    InputKind::Artifact => "artifact",
                    InputKind::Runtime => "runtime",
                    InputKind::BuildInput => "build input",
                    InputKind::BuildRequest => "build request",
                    InputKind::BuildOutput => "build output",
                };
                associations.push(format!(
                    "{prefix} -> layout {allocation} -> {kind} {}",
                    input.digest()
                ));
            }
        }
    }
    Ok(ProjectForgetReport {
        identity: project.hyphenated().to_string(),
        path: registration.map(|fact| std::path::PathBuf::from(fact.root())),
        associations,
        dry_run,
        forgotten: false,
        already_forgotten: registration.is_none(),
    })
}

#[cfg(test)]
mod tests;
