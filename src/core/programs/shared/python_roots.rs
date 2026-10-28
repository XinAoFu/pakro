//! Complete direct declarations and target-bound activation. No source or IO authority.

use crate::libraries::language::lease::LanguageLease;
use crate::packages::PackageInstallRequest;
use eyre::{Result, ensure};
use pakro_index::python_markers::{PythonMarkerScope, evaluate_python_marker};
use pakro_index::python_requirements::parse_python_requirement;
use pakro_index::python_source::NormalizedPythonName;
use pakro_resolver::solve::{Pep440, VersionAdapter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonLockIntent {
    Preserve,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PythonRoot {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) declaration: String,
}

impl PythonRoot {
    pub(crate) fn from_request(request: &PackageInstallRequest) -> Result<Self> {
        let declaration = request.requirement().map_err(|error| eyre::eyre!(error))?;
        Ok(Self {
            name: NormalizedPythonName::new(&request.name)?
                .as_str()
                .to_owned(),
            version: request.version.clone(),
            declaration,
        })
    }

    pub(crate) fn plain(name: &str, version: &str) -> Result<Self> {
        let name = NormalizedPythonName::new(name)?.as_str().to_owned();
        ensure!(
            !version.is_empty()
                && version.trim() == version
                && Pep440::parse_version(version).is_some(),
            "Python package {name} needs a complete exact upstream version"
        );
        Ok(Self {
            declaration: format!("{name}=={version}"),
            name,
            version: version.to_owned(),
        })
    }

    pub(crate) fn active(&self, language: &LanguageLease) -> Result<bool> {
        let syntax = parse_python_requirement(&self.declaration, Default::default())?;
        Ok(match syntax.marker() {
            None => true,
            Some(marker) => evaluate_python_marker(
                marker,
                language.target().environment(),
                PythonMarkerScope::Environment,
                language.target().semantics(),
                Default::default(),
            )?
            .value(),
        })
    }
}

pub(crate) fn declarations(roots: &[PythonRoot]) -> Vec<String> {
    roots.iter().map(|root| root.declaration.clone()).collect()
}

pub(crate) fn active_roots(
    roots: &[PythonRoot],
    language: &LanguageLease,
) -> Result<Vec<PythonRoot>> {
    ensure!(
        roots.len() <= 4096,
        "Python direct dependency budget exceeded"
    );
    let mut active = Vec::new();
    for root in roots {
        if root.active(language)? {
            active.push(root.clone());
        }
    }
    Ok(active)
}

pub(crate) fn pairs(roots: &[PythonRoot]) -> Vec<(&str, &str)> {
    roots
        .iter()
        .map(|root| (root.name.as_str(), root.version.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn p3_markers_use_the_selected_interpreter_and_keep_inactive_intent() {
        use crate::libraries::language::repository::tests::{
            archive, install_spec, operation_context, prepare,
        };
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().canonicalize().unwrap();
        let context = operation_context(&directory, true, std::time::Duration::from_secs(30));
        let bytes = archive("3.15.0");
        let language = prepare(&install_spec(&bytes), &context, &bytes)
            .await
            .unwrap();
        let mut enabled = PackageInstallRequest::pypi("python", "3.15.0", "enabled", "1.0");
        enabled.extras = vec!["feature".into()];
        enabled.marker = Some("python_version == '3.15'".into());
        let mut disabled = PackageInstallRequest::pypi("python", "3.15.0", "disabled", "1.0");
        disabled.marker = Some("python_version == '3.13'".into());
        let roots = [
            PythonRoot::from_request(&enabled).unwrap(),
            PythonRoot::from_request(&disabled).unwrap(),
        ];
        let active = active_roots(&roots, &language).unwrap();
        assert_eq!(active, vec![roots[0].clone()]);
        let original = declarations(&roots);
        assert_eq!(original.len(), 2);
        assert!(original[0].contains("enabled[feature]==1.0"));
        assert!(original[1].contains("python_version == '3.13'"));
        language.check_binding().unwrap();
    }
}
