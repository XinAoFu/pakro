//! Local library facts; reads never initialize, repair or register content.
use crate::libraries::language::repository::LanguageRepository;
use crate::libraries::project::repository::ProjectRepository;
use crate::programs::OperationContext;
use crate::programs::library::LibraryEntry;
use eyre::{ContextCompat, Result};

pub(crate) fn inventory(context: &OperationContext) -> Result<Vec<LibraryEntry>> {
    let projects = ProjectRepository::open_readonly(context)?;
    let languages = LanguageRepository::open_readonly(context)?;
    let mut result = Vec::new();
    let mut platforms = std::collections::BTreeMap::new();
    for language in languages
        .enumerate_published()
        .map_err(|error| eyre::eyre!("language library is unavailable: {error:?}"))?
    {
        context.checkpoint()?;
        let lease = languages
            .acquire_by_key(&language.key())?
            .context("language publication disappeared during query")?;
        lease.check_binding()?;
        platforms.insert(
            language.key().fingerprint(),
            format!(
                "{}-{}",
                language.platform_system(),
                language.platform_machine()
            ),
        );
        result.push(LibraryEntry {
            language: None,
            dependencies: Vec::new(),
            kind: "language".into(),
            instance: language.key().filename(),
            name: "python".into(),
            version: language.version(),
            build: language.build().map(str::to_owned),
            platform: format!(
                "{}-{}",
                language.platform_system(),
                language.platform_machine()
            ),
            path: language.payload_root().to_owned(),
            users: crate::programs::uninstall::reachability::references_with_project_snapshot(
                context,
                language.key().fingerprint(),
                &projects,
            )?,
        });
    }
    for package in crate::libraries::package::manifest::enumerate_published(context)? {
        let lease = crate::libraries::package::lease::acquire_package(context, package.key())?
            .context("package publication disappeared during query")?;
        lease.check_binding()?;
        result.push(LibraryEntry {
            kind: "package".into(),
            instance: format!("package-{}", hex::encode(package.key())),
            name: package.name().to_owned(),
            version: package.version().to_owned(),
            build: None,
            platform: platforms
                .get(&package.language)
                .cloned()
                .context("package language instance is missing from the published library")?,
            language: Some(format!("language-{}", hex::encode(package.language))),
            dependencies: package
                .facts
                .dependencies
                .iter()
                .map(|key| format!("package-{}", hex::encode(key)))
                .collect(),
            path: lease.root().to_owned(),
            users: crate::programs::uninstall::reachability::references_with_project_snapshot(
                context,
                package.key(),
                &projects,
            )?,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
