//! Read-only persisted project and dependency reports.
use crate::libraries::project::repository::ProjectRepository;
use crate::programs::OperationContext;
use crate::programs::library::ProjectEntry;
use eyre::Result;

pub(crate) fn projects(context: &OperationContext) -> Result<Vec<ProjectEntry>> {
    let repository = ProjectRepository::open_readonly(context)?;
    let views = repository.read_views();
    let mut result = Vec::new();
    for project in views.registered_projects() {
        context.checkpoint()?;
        let selected = views.language_selection(project.identity());
        result.push(ProjectEntry {
            identity: project.identity().as_uuid().to_string(),
            path: project.root().clone(),
            accessible: project.root().try_exists()?,
            revision: project.revision(),
            language: selected
                .as_ref()
                .map(|selection| format!("language-{}", hex::encode(selection.language()))),
            packages: selected
                .map(|selection| {
                    selection
                        .package_references()
                        .iter()
                        .map(|key| format!("package-{}", hex::encode(key)))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    Ok(result)
}
