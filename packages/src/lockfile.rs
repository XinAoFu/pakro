//! The package-graph lockfile (`pakro.packages.lock`), a sidecar next to
//! `pakro.lock`. The main lockfile has a hand-rolled TOML reader/writer and a
//! revision-merge machinery that this must not disturb; pakro already keeps
//! native dependency graphs (aube, uv) in sidecars, so this follows the
//! established pattern. Format is plain serde TOML so it can merge back into
//! `pakro.lock` later once stabilized.

use std::collections::BTreeMap;
use std::path::Path;

use eyre::Context;
use pakro_resolver::{DependencyRequest, ResolvedGraph, ResolvedNode};

pub(crate) const LOCKFILE_NAME: &str = "pakro.packages.lock";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PackageLock {
    /// runtime group key ("python@3.10.14") -> resolved nodes, sorted
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<String, Vec<LockedPackage>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LockedPackage {
    pub name: String,
    pub version: String,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<LockedDep>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LockedDep {
    pub name: String,
    pub constraint: String,
}

impl PackageLock {
    pub(crate) fn from_graph(runtime: &str, graph: &ResolvedGraph) -> Self {
        let mut packages: Vec<LockedPackage> = graph
            .nodes
            .iter()
            .map(|node| LockedPackage {
                name: node.name.clone(),
                version: node.version.clone(),
                digest: node.digest.clone().unwrap_or_default(),
                deps: node
                    .deps
                    .iter()
                    .map(|dep| LockedDep {
                        name: dep.name.clone(),
                        constraint: dep.constraint.clone(),
                    })
                    .collect(),
            })
            .collect();
        packages.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        let mut lock = Self::default();
        lock.groups.insert(runtime.to_string(), packages);
        lock
    }

    /// The graph recorded for a runtime group, if any.
    pub(crate) fn graph_for(&self, runtime: &str) -> Option<ResolvedGraph> {
        let packages = self.groups.get(runtime)?;
        Some(ResolvedGraph {
            nodes: packages
                .iter()
                .map(|p| ResolvedNode {
                    eco: "python".into(),
                    name: p.name.clone(),
                    version: p.version.clone(),
                    digest: if p.digest.is_empty() {
                        None
                    } else {
                        Some(p.digest.clone())
                    },
                    deps: p
                        .deps
                        .iter()
                        .map(|d| DependencyRequest {
                            eco: "python".into(),
                            name: d.name.clone(),
                            constraint: d.constraint.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    pub(crate) fn read(path: &Path) -> eyre::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(body) => toml::from_str(&body)
                .with_context(|| format!("parsing {}", path.display()))
                .map(Some),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub(crate) fn write(&self, path: &Path) -> eyre::Result<()> {
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> ResolvedGraph {
        ResolvedGraph {
            nodes: vec![
                ResolvedNode {
                    eco: "python".into(),
                    name: "contingent-pyprops".into(),
                    version: "0.2.1".into(),
                    digest: Some("sha256-bbb".into()),
                    deps: vec![],
                },
                ResolvedNode {
                    eco: "python".into(),
                    name: "numpy".into(),
                    version: "4.2.2".into(),
                    digest: Some("sha256-aaa".into()),
                    deps: vec![DependencyRequest {
                        eco: "python".into(),
                        name: "contingent-pyprops".into(),
                        constraint: ">=0.2, <0.3".into(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCKFILE_NAME);
        let lock = PackageLock::from_graph("python@3.10.14", &sample_graph());
        lock.write(&path).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("numpy"));
        assert!(body.contains("sha256-aaa"));

        let loaded = PackageLock::read(&path).unwrap().unwrap();
        assert_eq!(loaded, lock);
        let graph = loaded.graph_for("python@3.10.14").unwrap();
        assert_eq!(graph, sample_graph());
    }

    #[test]
    fn missing_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            PackageLock::read(&dir.path().join(LOCKFILE_NAME))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unknown_group_is_none() {
        let lock = PackageLock::from_graph("python@3.10.14", &sample_graph());
        assert!(lock.graph_for("node@22").is_none());
    }
}
