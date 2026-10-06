//! Install orchestration: requests -> lock reuse -> solve -> artifact
//! selection -> download + verify -> store -> materialize -> lockfile. Every
//! network touch goes through mise-index or the shared http helper; every
//! byte that lands in the store is digest-checked first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, bail};
use tokio::runtime::Handle;
use tokio::task::block_in_place;

use mise_index::pypi::{Index, python_requires_matches};
use mise_index::{ArtifactKind, RuntimeInfo, wheel_matches};
use mise_resolver::{DependencyRequest, MetadataSource, ResolvedGraph};
use mise_store::{Digest, Eco, PackageIdentity, Platform, RuntimeId, Store};

use crate::{dirs, http};

use super::PackageInstallRequest;
use super::lockfile::{LOCKFILE_NAME, PackageLock};
use super::materialize::{VenvSpec, materialize_venv};

pub struct Locations {
    /// directory receiving `mise.packages.lock`
    pub config_root: PathBuf,
    /// directory receiving `.mise/venvs/`
    pub project_root: PathBuf,
    pub store_root: PathBuf,
    pub index_cache: PathBuf,
}

impl Locations {
    pub fn from_config(project_root: &Path) -> eyre::Result<Self> {
        let data = dirs::INSTALLS
            .parent()
            .context("installs dir always has a parent")?
            .to_path_buf();
        Ok(Self {
            config_root: project_root.to_path_buf(),
            project_root: project_root.to_path_buf(),
            store_root: data.join("store"),
            index_cache: dirs::CACHE.join("index"),
        })
    }
}

/// Installs every request group. Returns how many store entries were newly
/// downloaded (0 means everything came from the store or the lock).
pub async fn install_packages(
    requests: &[PackageInstallRequest],
    locs: &Locations,
) -> eyre::Result<usize> {
    let mut groups: BTreeMap<(String, String), Vec<&PackageInstallRequest>> = BTreeMap::new();
    for request in requests {
        groups
            .entry((
                request.runtime_name.clone(),
                request.runtime_version.clone(),
            ))
            .or_default()
            .push(request);
    }
    let lock_path = locs.config_root.join(LOCKFILE_NAME);
    let mut lock = PackageLock::read(&lock_path)?.unwrap_or_default();
    let mut downloaded = 0;
    for ((runtime_name, runtime_version), group_requests) in groups {
        if runtime_name != "python" {
            bail!("native packages currently support python only, got {runtime_name}");
        }
        let group_key = format!("{runtime_name}@{runtime_version}");
        let runtime = runtime_info(&runtime_version)?;
        let platform = Platform::detect();

        let graph = match lock.graph_for(&group_key).filter(|locked| {
            top_level_matches(
                locked,
                group_requests
                    .iter()
                    .map(|r| (r.name.as_str(), r.version.as_str())),
            )
        }) {
            Some(locked) => locked,
            None => {
                let source = IndexSource {
                    index: Index::new(&locs.index_cache),
                    runtime,
                };
                let solve_requests: Vec<DependencyRequest> = group_requests
                    .iter()
                    .map(|r| DependencyRequest {
                        eco: "python".into(),
                        name: r.name.clone(),
                        constraint: r.version.clone(),
                    })
                    .collect();
                let locked_bias = lock.graph_for(&group_key).unwrap_or_default();
                mise_resolver::solve(&solve_requests, &locked_bias, &source, "python")
                    .map_err(|e| eyre::eyre!("resolving {group_key}: {e}"))?
            }
        };

        let store = Store::open(&locs.store_root)?;
        let index = Index::new(&locs.index_cache);
        let mut nodes = graph.nodes.clone();
        for node in &mut nodes {
            if node.digest.as_deref().is_some_and(|d| !d.is_empty()) {
                continue;
            }
            let metadata = index
                .metadata(&node.name, &node.version, &runtime)
                .await
                .with_context(|| format!("fetching metadata for {} {}", node.name, node.version))?;
            if let Some(requires) = &metadata.requires_python
                && !python_requires_matches(requires, &runtime)
            {
                bail!(
                    "{} {} requires python {requires}, runtime is {runtime_version}",
                    node.name,
                    node.version
                );
            }
            let artifact = select_wheel(&metadata.artifacts, &runtime, &platform)
                .with_context(|| {
                    format!(
                        "no compatible wheel for {} {} on {platform} (source builds are not installed in this version)",
                        node.name, node.version
                    )
                })?;
            let sha256 = artifact
                .sha256
                .clone()
                .expect("select_wheel requires a digest");
            let digest = format!("sha256-{sha256}")
                .parse::<Digest>()
                .context("registry digest is malformed")?;
            let identity = PackageIdentity {
                eco: Eco::Python,
                name: node.name.clone(),
                version: node.version.clone(),
                runtime: Some(RuntimeId {
                    eco: Eco::Python,
                    version: runtime_version.clone(),
                }),
                platform,
                digest,
            };
            if !store.contains(&identity) {
                let bytes = http::HTTP
                    .get_bytes(artifact.url.as_str())
                    .await
                    .with_context(|| format!("downloading {}", artifact.filename))?;
                store.unpack(&identity, bytes.as_ref())?;
                downloaded += 1;
            }
            node.digest = Some(format!("sha256-{sha256}"));
        }
        let graph = ResolvedGraph { nodes }.sorted();

        let interpreter_home = dirs::INSTALLS.join(&runtime_name).join(&runtime_version);
        if !interpreter_home.join("bin/python3").exists() {
            bail!("runtime {group_key} is not installed; run `mise install {group_key}` first");
        }
        materialize_venv(
            &VenvSpec {
                project_root: &locs.project_root,
                runtime_name: &runtime_name,
                runtime_version: &runtime_version,
                interpreter_home: &interpreter_home,
            },
            &graph,
            &store,
        )?;
        if let Some(packages) = PackageLock::from_graph(&group_key, &graph)
            .groups
            .remove(&group_key)
        {
            lock.groups.insert(group_key, packages);
        }
    }
    lock.write(&lock_path)?;
    Ok(downloaded)
}

fn runtime_info(version: &str) -> eyre::Result<RuntimeInfo> {
    let mut parts = version.split('.');
    let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
        bail!("runtime version {version:?} must be at least major.minor");
    };
    Ok(RuntimeInfo {
        major: major.parse()?,
        minor: minor.parse()?,
    })
}

/// Picks the wheel to install: platform-compatible, carrying a digest, with
/// the longest filename last as a stable tie-break (more specific tags sort
/// later in practice).
fn select_wheel<'a>(
    artifacts: &'a [mise_index::ArtifactDescriptor],
    runtime: &RuntimeInfo,
    platform: &Platform,
) -> Option<&'a mise_index::ArtifactDescriptor> {
    let mut candidates: Vec<_> = artifacts
        .iter()
        .filter(|a| match &a.kind {
            ArtifactKind::Wheel(tags) => wheel_matches(tags, runtime, platform),
            ArtifactKind::Sdist => false,
        })
        .filter(|a| a.sha256.is_some())
        .collect();
    candidates.sort_by(|a, b| a.filename.cmp(&b.filename));
    candidates.into_iter().next_back()
}

fn top_level_matches<'a>(
    locked: &ResolvedGraph,
    pinned: impl Iterator<Item = (&'a str, &'a str)>,
) -> bool {
    let mut remaining: BTreeMap<&str, &str> = pinned.collect::<BTreeMap<_, _>>();
    for node in &locked.nodes {
        if let Some(pinned_version) = remaining.remove(node.name.as_str())
            && pinned_version != node.version
        {
            return false;
        }
    }
    remaining.is_empty()
}

/// Bridges the sync solver against the async index client. `block_in_place`
/// is safe here because mise runs a multi-thread tokio runtime and solving
/// happens on a worker thread.
struct IndexSource {
    index: Index,
    runtime: RuntimeInfo,
}

impl MetadataSource for IndexSource {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        let index = &self.index;
        block_in_place(|| Handle::current().block_on(index.versions(name)))
            .map_err(|e| e.to_string())
    }

    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        let index = &self.index;
        let runtime = &self.runtime;
        let metadata =
            block_in_place(|| Handle::current().block_on(index.metadata(name, version, runtime)))
                .map_err(|e| e.to_string())?;
        Ok(metadata
            .deps
            .into_iter()
            .map(|dep| DependencyRequest {
                eco: "python".into(),
                name: dep.name,
                constraint: dep.constraint,
            })
            .collect())
    }
}
