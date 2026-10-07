//! Package-venv visibility to the rest of mise (ADR-0007): the venvs
//! `[packages]` materializes are part of the project environment, so their
//! bin dirs join PATH through the same `Toolset` env assembly that serves
//! activate, `mise x` and shims.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::Config;

use super::PackageInstallRequest;

/// The bin dirs of materialized package venvs derived from `requests`:
/// python groups only (node materializes `node_modules/` instead), present
/// on disk, ordered by group key so the PATH is deterministic. Returns
/// `(group_key, bin_dir)` pairs; the key names the owning runtime group.
pub fn venv_bin_dirs_for(
    project_root: Option<&Path>,
    requests: &[PackageInstallRequest],
) -> Vec<(String, PathBuf)> {
    let Some(root) = project_root else {
        return vec![];
    };
    let mut groups: BTreeSet<(String, String)> = BTreeSet::new();
    for request in requests {
        if request.runtime_name == "python" {
            groups.insert((
                request.runtime_name.clone(),
                request.runtime_version.clone(),
            ));
        }
    }
    groups
        .into_iter()
        .map(|(name, version)| {
            (
                format!("{name}@{version}"),
                root.join(format!(".mise/venvs/{name}-{version}"))
                    .join("bin"),
            )
        })
        .filter(|(_, bin)| bin.is_dir())
        .collect()
}

/// [`venv_bin_dirs_for`] over the loaded config's package requests.
pub fn venv_bin_dirs(config: &Config) -> Vec<(String, PathBuf)> {
    venv_bin_dirs_for(config.project_root.as_deref(), &config.package_requests())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venv_bin_dirs_derivation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".mise/venvs/python-3.10.14/bin")).unwrap();
        let req = |runtime: &str, version: &str, name: &str| PackageInstallRequest {
            runtime_name: runtime.to_string(),
            runtime_version: version.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
        };
        let requests = vec![
            req("python", "3.10.14", "numpy"),
            req("python", "3.10.14", "six"), // same group: deduplicated
            req("python", "3.11.9", "six"),  // no venv on disk: dropped
            req("node", "22.14.0", "typescript"), // node: not a venv runtime
        ];
        let dirs = venv_bin_dirs_for(Some(root.path()), &requests);
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert_eq!(dirs[0].0, "python@3.10.14");
        assert!(dirs[0].1.ends_with(".mise/venvs/python-3.10.14/bin"));
        assert!(venv_bin_dirs_for(None, &requests).is_empty());
    }
}
