//! Local library facts; reads never initialize, repair or register content.
use crate::libraries::language::repository::LanguageRepository;
use crate::libraries::project::repository::ProjectRepository;
use crate::programs::OperationContext;
use crate::programs::library::LibraryEntry;
use eyre::{ContextCompat, Result};

fn source_runtime_configuration(
    input: &crate::libraries::language::source::SourcePythonBuildInput,
) -> (u32, String) {
    (
        input.recipe,
        if input.includes_test_modules() {
            "含CPython自身测试套件及测试扩展"
        } else {
            "不含CPython自身测试套件及测试扩展"
        }
        .to_owned(),
    )
}

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
            .acquire_by_key_for_query(&language.key())?
            .context("language publication disappeared during query")?;
        lease.check_metadata()?;
        platforms.insert(
            language.key().fingerprint(),
            format!(
                "{}-{}",
                language.platform_system(),
                language.platform_machine()
            ),
        );
        let configuration = language
            .spec()
            .plan()
            .source_build()
            .map(source_runtime_configuration);
        result.push(LibraryEntry {
            language: None,
            dependencies: Vec::new(),
            kind: "language".into(),
            instance: language.key().filename(),
            name: "python".into(),
            version: language.version(),
            build: language.build().map(str::to_owned),
            source_recipe: configuration.as_ref().map(|(recipe, _)| *recipe),
            runtime_configuration: configuration.map(|(_, description)| description),
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
        let lease =
            crate::libraries::package::lease::acquire_package_for_query(context, package.key())?
                .context("package publication disappeared during query")?;
        lease.check_metadata()?;
        result.push(LibraryEntry {
            kind: "package".into(),
            instance: format!("package-{}", hex::encode(package.key())),
            name: package.name().to_owned(),
            version: package.version().to_owned(),
            build: None,
            source_recipe: None,
            runtime_configuration: None,
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
