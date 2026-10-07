//! Install orchestration: requests -> lock reuse -> solve -> artifact
//! selection -> download + verify -> store -> materialize -> lockfile. Every
//! network touch goes through mise-index or the shared http helper; every
//! byte that lands in the store is digest-checked first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, bail};
use tokio::runtime::Handle;
use tokio::task::block_in_place;

use mise_index::Index;
use mise_index::pypi::python_requires_matches;
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
        if !matches!(runtime_name.as_str(), "python" | "node") {
            bail!("native packages currently support python and node, got {runtime_name}");
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
                let solve_requests: Vec<DependencyRequest> = group_requests
                    .iter()
                    .map(|r| DependencyRequest {
                        eco: runtime_name.clone(),
                        name: r.name.clone(),
                        constraint: r.version.clone(),
                    })
                    .collect();
                let locked_bias = lock.graph_for(&group_key).unwrap_or_default();
                let index = Index::new(&locs.index_cache);
                match runtime_name.as_str() {
                    "node" => {
                        let source = NodeSource { index };
                        mise_resolver::solve(&solve_requests, &locked_bias, &source, "node")
                    }
                    _ => {
                        let source = PythonSource { index, runtime };
                        mise_resolver::solve(&solve_requests, &locked_bias, &source, "python")
                    }
                }
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
            let metadata = match runtime_name.as_str() {
                "node" => index
                    .npm_metadata(&node.name, &node.version, &runtime)
                    .await
                    .with_context(|| {
                        format!("fetching metadata for {} {}", node.name, node.version)
                    })?,
                _ => {
                    let metadata = index
                        .metadata(&node.name, &node.version, &runtime)
                        .await
                        .with_context(|| {
                            format!("fetching metadata for {} {}", node.name, node.version)
                        })?;
                    if let Some(requires) = &metadata.requires_python
                        && !python_requires_matches(requires, &runtime)
                    {
                        bail!(
                            "{} {} requires python {requires}, runtime is {runtime_version}",
                            node.name,
                            node.version
                        );
                    }
                    metadata
                }
            };
            let mut built_bytes: Option<Vec<u8>> = None;
            let artifact_owned;
            let artifact = match select_artifact(
                &metadata.artifacts,
                &runtime,
                &platform,
                runtime_name == "node",
            ) {
                Some(a) => a,
                None if runtime_name == "python" => {
                    let built = build_sdist_to_wheel(
                        &metadata,
                        &node.name,
                        &node.version,
                        &runtime_name,
                        &runtime_version,
                        &runtime,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "no wheel for {} {} on {platform}; source build also failed",
                            node.name, node.version
                        )
                    })?;
                    built_bytes = Some(built.bytes);
                    artifact_owned = built.descriptor;
                    &artifact_owned
                }
                None => bail!(
                    "no installable artifact for {} {} on {platform}",
                    node.name,
                    node.version
                ),
            };
            let digest_str = artifact
                .digest
                .clone()
                .expect("artifact selection requires a digest");
            let digest = digest_str
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
            let identity = match runtime_name.as_str() {
                // npm packages bind to the node major at most; the identity
                // keeps them runtime-free so one entry serves every project
                "node" => PackageIdentity {
                    eco: Eco::Node,
                    name: node.name.clone(),
                    version: node.version.clone(),
                    runtime: None,
                    platform,
                    digest,
                },
                _ => identity,
            };
            if !store.contains(&identity) {
                if let Some(bytes) = &built_bytes {
                    store.unpack(&identity, bytes)?;
                } else {
                    let bytes = http::HTTP
                        .get_bytes(artifact.url.as_str())
                        .await
                        .with_context(|| format!("downloading {}", artifact.filename))?;
                    store.unpack(&identity, bytes.as_ref())?;
                }
                downloaded += 1;
            }
            node.digest = Some(digest_str);
        }
        let graph = ResolvedGraph { nodes }.sorted();

        if runtime_name == "python" {
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
        } else {
            // node materialization lands with its own module; installs and
            // locking already work end to end
            crate::packages::materialize_node::materialize_node_modules(
                &locs.project_root,
                &graph,
                &store,
            )?;
        }
        if let Some(packages) = PackageLock::from_graph(&group_key, &graph)
            .groups
            .remove(&group_key)
        {
            lock.groups.insert(group_key, packages);
        }
    }
    lock.write(&lock_path)?;
    // go/maven: storage-layer absorption (design doc §六); go.mod and pom.xml
    // remain the declarations, mise takes over where artifacts live
    if super::system::install_go_modules(&locs.project_root, locs)? {
        downloaded += 1;
    }
    if super::system::install_maven_dependencies(&locs.project_root, locs)? {
        downloaded += 1;
    }
    maybe_auto_gc(&locs.store_root);
    Ok(downloaded)
}

/// Runs `store gc` at most once a day; the marker lives in STATE.
fn maybe_auto_gc(store_root: &std::path::Path) {
    let marker = crate::dirs::STATE.join("store-gc-last-run");
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let fresh = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < day);
    if fresh {
        return;
    }
    let store = Store::open(store_root);
    if let Ok(store) = store
        && let Ok(report) = mise_store::gc(&store)
        && !report.removed.is_empty()
    {
        info!(
            "store gc: removed {} entries ({} bytes freed)",
            report.removed.len(),
            report.freed_bytes
        );
    }
    let _ = std::fs::write(&marker, b"");
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
fn select_artifact<'a>(
    artifacts: &'a [mise_index::ArtifactDescriptor],
    runtime: &RuntimeInfo,
    platform: &Platform,
    node: bool,
) -> Option<&'a mise_index::ArtifactDescriptor> {
    let mut candidates: Vec<_> = artifacts
        .iter()
        .filter(|a| match &a.kind {
            // npm ships one platform-independent tarball per version
            ArtifactKind::Sdist if node => true,
            ArtifactKind::Wheel(tags) => !node && wheel_matches(tags, runtime, platform),
            ArtifactKind::Sdist => false,
        })
        .filter(|a| a.digest.is_some())
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
struct PythonSource {
    index: Index,
    runtime: RuntimeInfo,
}

impl MetadataSource for PythonSource {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        let index = &self.index;
        let runtime = &self.runtime;
        // runtime-filtered: a version whose requires-python rejects this
        // interpreter must never become a solver candidate
        block_in_place(|| Handle::current().block_on(index.versions_for_runtime(name, runtime)))
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

/// npm metadata for the solver. Prerelease versions are filtered out of the
/// candidate list (npm's own default); exact prerelease pins are not
/// supported for node in this version.
struct NodeSource {
    index: Index,
}

impl MetadataSource for NodeSource {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        let index = &self.index;
        let versions = block_in_place(|| Handle::current().block_on(index.npm_versions(name)))
            .map_err(|e| e.to_string())?;
        // a '-' after the core version introduces a prerelease
        Ok(versions
            .into_iter()
            .filter(|v| !v.split('+').next().unwrap_or_default().contains('-'))
            .collect())
    }

    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        let index = &self.index;
        let runtime = RuntimeInfo {
            major: 22,
            minor: 0,
        };
        let metadata = block_in_place(|| {
            Handle::current().block_on(index.npm_metadata(name, version, &runtime))
        })
        .map_err(|e| e.to_string())?;
        Ok(metadata
            .deps
            .into_iter()
            .map(|dep| DependencyRequest {
                eco: "node".into(),
                name: dep.name,
                constraint: dep.constraint,
            })
            .collect())
    }
}

/// Installs every `[packages]` group plus any extra CLI pins (bound or bare).
/// Shared by `mise install` and `mise use`.
pub async fn install_config_packages(
    config: &crate::config::Config,
    extra: Vec<super::PackageInstallRequest>,
) -> eyre::Result<usize> {
    let mut requests = config.package_requests();
    requests.extend(extra);
    if let Some(cli_pins) = super::CLI_PACKAGE_ARGS.get() {
        requests.extend(cli_pins.iter().cloned());
    }
    if requests.is_empty() {
        return Ok(0);
    }
    let project_root = config
        .project_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| eyre::eyre!("could not locate project root"))?;
    let locations = Locations::from_config(&project_root)?;
    install_packages(&requests, &locations).await
}

/// A wheel produced by building an sdist locally.
struct BuiltArtifact {
    descriptor: mise_index::ArtifactDescriptor,
    bytes: Vec<u8>,
}

/// Builds an sdist into a wheel using the mise-installed python, returning
/// the wheel bytes. This runs `pip wheel --no-deps` which executes the
/// package's build backend — the same code `pip install` would run, with
/// the same security implications; it only triggers when no prebuilt wheel
/// matches the platform.
#[allow(clippy::too_many_arguments)]
async fn build_sdist_to_wheel(
    metadata: &mise_index::PackageMetadata,
    name: &str,
    version: &str,
    runtime_name: &str,
    runtime_version: &str,
    _runtime: &RuntimeInfo,
) -> eyre::Result<BuiltArtifact> {
    let sdist = metadata
        .artifacts
        .iter()
        .find(|a| matches!(a.kind, mise_index::ArtifactKind::Sdist))
        .context("no sdist available either")?;
    warn!(
        "no prebuilt wheel for {name} {version}; building from source (this runs the package's build code)"
    );
    let interpreter = dirs::INSTALLS
        .join(runtime_name)
        .join(runtime_version)
        .join("bin/python3");
    if !interpreter.exists() {
        bail!("runtime {runtime_name}@{runtime_version} not installed");
    }
    let sdist_bytes = http::HTTP
        .get_bytes(sdist.url.as_str())
        .await
        .context("downloading sdist")?;
    let tmp = tempfile::tempdir()?;
    let sdist_path = tmp.path().join(&sdist.filename);
    std::fs::write(&sdist_path, &sdist_bytes)?;
    let wheel_dir = tmp.path().join("wheels");
    std::fs::create_dir_all(&wheel_dir)?;
    let status = std::process::Command::new(&interpreter)
        .args([
            "-m",
            "pip",
            "wheel",
            "--no-deps",
            "--no-build-isolation",
            "-w",
        ])
        .arg(&wheel_dir)
        .arg(&sdist_path)
        .status()
        .context("running pip wheel")?;
    if !status.success() {
        bail!("pip wheel failed with {status}");
    }
    let wheel_file = std::fs::read_dir(&wheel_dir)?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().ends_with(".whl"))
        .context("pip wheel produced no wheel")?;
    let bytes = std::fs::read(wheel_file.path())?;
    let filename = wheel_file.file_name().to_string_lossy().to_string();
    let digest = mise_store::Digest::from_sha256_bytes(&bytes).to_string();
    Ok(BuiltArtifact {
        descriptor: mise_index::ArtifactDescriptor {
            filename,
            url: String::new(),
            digest: Some(digest),
            kind: mise_index::ArtifactKind::Sdist,
        },
        bytes,
    })
}
