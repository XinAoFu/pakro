//! Selects one exact target without granting removal authority.
use crate::libraries::language::repository::{LanguageRepository, PublishedLanguage};
use crate::programs::OperationContext;
use crate::programs::shared::exact_version::exact_python_request;
use eyre::{Result, bail, ensure};

pub(crate) fn language(
    context: &OperationContext,
    version: &str,
    instance: Option<&str>,
) -> Result<PublishedLanguage> {
    let exact = exact_python_request(version)?;
    let repository = LanguageRepository::open_readonly(context)?;
    let mut candidates: Vec<_> = repository
        .enumerate_published()
        .map_err(|error| eyre::eyre!("language inventory is unavailable: {error:?}"))?
        .into_iter()
        .filter(|candidate| {
            candidate.version() == exact.version_only()
                && exact
                    .build_pin()
                    .is_none_or(|build| candidate.build() == Some(build))
                && instance.is_none_or(|instance| instance == candidate.key().filename())
        })
        .collect();
    ensure!(
        !candidates.is_empty(),
        "no published Python instance matches {version}; use pakro installs ls to inspect exact instances"
    );
    if candidates.len() != 1 {
        bail!(
            "multiple Python instances match {version}: {}; pass --instance with one complete instance identity",
            candidates
                .iter()
                .map(|candidate| candidate.key().filename())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(candidates.remove(0))
}

pub(crate) fn package(
    context: &OperationContext,
    request: &crate::packages::PackageInstallRequest,
    instance: Option<&str>,
) -> Result<crate::libraries::package::manifest::PackageManifest> {
    use crate::libraries::package::manifest;
    ensure!(
        request.runtime_name == "python",
        "only Python shared package removal is supported"
    );
    let runtime = exact_python_request(&request.runtime_version)?;
    let version: pep440_rs::Version = request.version.parse().map_err(|error| {
        eyre::eyre!("package removal requires a complete exact version: {error}")
    })?;
    let name = pakro_index::python_source::NormalizedPythonName::new(&request.name)?;
    let languages = LanguageRepository::open_readonly(context)?
        .enumerate_published()
        .map_err(|error| eyre::eyre!("language inventory is unavailable: {error:?}"))?;
    let language_keys: Vec<_> = languages
        .iter()
        .filter(|language| {
            language.version() == runtime.version_only()
                && runtime
                    .build_pin()
                    .is_none_or(|pin| language.build() == Some(pin))
        })
        .map(|language| language.key().fingerprint())
        .collect();
    let mut candidates: Vec<_> = manifest::enumerate_published(context)?
        .into_iter()
        .filter(|candidate| {
            candidate.name() == name.as_str()
                && candidate
                    .version()
                    .parse::<pep440_rs::Version>()
                    .ok()
                    .as_ref()
                    == Some(&version)
                && language_keys.contains(&candidate.language)
                && instance.is_none_or(|instance| {
                    instance == format!("package-{}", hex::encode(candidate.key()))
                })
        })
        .collect();
    ensure!(
        !candidates.is_empty(),
        "no published package matches {}@{} for Python {}; use pakro installs ls",
        request.name,
        request.version,
        request.runtime_version
    );
    if candidates.len() != 1 {
        bail!(
            "multiple package instances match: {}; pass --instance with one complete instance identity",
            candidates
                .iter()
                .map(|candidate| format!("package-{}", hex::encode(candidate.key())))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(candidates.remove(0))
}
