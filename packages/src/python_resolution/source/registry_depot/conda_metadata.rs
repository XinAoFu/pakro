//! Python distributions proven by an extracted Conda artifact and held package.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;

use eyre::{ContextCompat, Result, ensure};
use rattler_conda_types::package::PathType;

use crate::libraries::package::lease::PackageLease;
use crate::packages::conda_artifact::CondaTree;
use crate::packages::python_resolution::source::wheel_metadata::project_core_metadata;
use crate::packages::python_wheel::{
    WheelCoreMetadata, WheelLimits, parse_installed_core_metadata,
};
use crate::programs::OperationContext;
use pakro_index::python_metadata::{PythonDependencyMetadataFacts, PythonMetadataLimits};

/// The extracted tree and actual published package remain held for the life
/// of these facts; neither the filename nor the Conda name is a mapping proof.
pub(crate) struct CondaDistribution<'a> {
    package: PackageLease,
    tree: &'a CondaTree,
    path: PathBuf,
    metadata: WheelCoreMetadata,
    facts: PythonDependencyMetadataFacts,
}

impl CondaDistribution<'_> {
    pub(crate) fn package(&self) -> &PackageLease {
        &self.package
    }
    pub(crate) fn tree(&self) -> &CondaTree {
        self.tree
    }
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub(crate) fn metadata(&self) -> &WheelCoreMetadata {
        &self.metadata
    }
    pub(crate) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        self.package.check_binding()
    }
}

pub(crate) fn inspect_conda_distributions<'a>(
    context: &OperationContext,
    package: &PackageLease,
    tree: &'a CondaTree,
) -> Result<Vec<CondaDistribution<'a>>> {
    context.checkpoint()?;
    package.ensure_library_context(context)?;
    package.check_binding()?;
    ensure!(
        package.manifest().conda.as_ref() == Some(tree.record()),
        "Conda metadata owner differs from the held package"
    );
    let mut distributions = Vec::new();
    let mut names = BTreeSet::new();
    for entry in &tree.paths().paths {
        let path = &entry.relative_path;
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(parent) = path
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let stem = match filename {
            "METADATA" => parent.strip_suffix(".dist-info"),
            "PKG-INFO" => parent.strip_suffix(".egg-info"),
            _ => None,
        };
        let Some(stem) = stem else { continue };
        ensure!(
            entry.path_type == PathType::HardLink,
            "Conda distribution metadata is not a regular owned file"
        );
        let (name, version) = stem
            .rsplit_once('-')
            .context("Conda distribution metadata directory has no exact name and version")?;
        let name = pakro_index::python_source::NormalizedPythonName::new(name)?;
        let mut file = std::fs::File::open(tree.root().join(path))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take((WheelLimits::default().max_metadata_bytes + 1) as u64)
            .read_to_end(&mut bytes)?;
        let metadata =
            parse_installed_core_metadata(bytes, name.as_str(), version, WheelLimits::default())?;
        let facts = project_core_metadata(
            &metadata,
            name.as_str(),
            version,
            PythonMetadataLimits::default(),
        )?;
        ensure!(
            names.insert(facts.name().as_str().to_owned()),
            "Conda artifact provides the same Python distribution more than once"
        );
        distributions.push(CondaDistribution {
            package: package.clone(),
            tree,
            path: path.clone(),
            metadata,
            facts,
        });
    }
    package.check_binding()?;
    context.checkpoint()?;
    Ok(distributions)
}
