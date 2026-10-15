//! Install orchestration: requests -> lock reuse -> solve -> artifact
//! selection -> download + verify -> store -> materialize -> lockfile. Every
//! network touch goes through pakro-index or the shared http helper; every
//! byte that lands in the store is digest-checked first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Context, ContextCompat, bail};
use tokio::runtime::Handle;
use tokio::task::block_in_place;

use pakro_index::Index;
use pakro_index::{ArtifactKind, RuntimeInfo};
use pakro_resolver::{DependencyRequest, MetadataSource, ResolvedGraph};
use pakro_store::{Digest, Eco, PackageIdentity, Platform, RuntimeId, Store};

use crate::{dirs, http};

use super::PackageInstallRequest;
use super::binding::{
    PackageInstallInputs, guard_python_sidecar_migration, merge_owned_requests, prepare_inputs,
};
pub use super::binding::{
    PackageInvocationPolicy, PackageRuntimeBinding, snapshot_runtime_bindings,
};
use super::lockfile::{LOCKFILE_NAME, PackageLock};
use super::materialize::{VenvSpec, materialize_venv};
pub use super::python::{PythonPackageLatest, latest_python_package};
use super::python::{
    PythonPackageRuntime, PythonSource, select_python_artifact, validate_metadata,
};

pub struct Locations {
    /// directory receiving `pakro.packages.lock`
    pub config_root: PathBuf,
    /// directory receiving `.pakro/venvs/`
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
pub(crate) async fn install_packages(
    inputs: &PackageInstallInputs,
    locs: &Locations,
    index: Arc<Index>,
) -> eyre::Result<usize> {
    guard_python_sidecar_migration(inputs)?;
    let mut python_runtimes = BTreeMap::new();
    for (key, tool) in &inputs.python_runtimes {
        let runtime = PythonPackageRuntime::from_tool_version(tool).await?;
        runtime.ensure_current_prefix()?;
        python_runtimes.insert(key.clone(), runtime);
    }
    let mut groups: BTreeMap<(String, String), Vec<&PackageInstallRequest>> = BTreeMap::new();
    for request in &inputs.requests {
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
        let python = python_runtimes.get(&group_key);
        if runtime_name == "python" && python.is_none() {
            bail!("missing observed Python binding for {group_key}");
        }
        let node_runtime = if runtime_name == "node" {
            Some(runtime_info(&runtime_version)?)
        } else {
            None
        };
        let platform = Platform::detect();
        // Legacy Python locks have neither roots nor a context fingerprint.
        let reusable = if runtime_name == "node" {
            lock.graph_for(&group_key).filter(|locked| {
                top_level_matches(
                    locked,
                    group_requests
                        .iter()
                        .map(|request| (request.name.as_str(), request.version.as_str())),
                )
            })
        } else {
            None
        };
        let graph = match reusable {
            Some(locked) => locked,
            None => {
                let solve_requests: Vec<DependencyRequest> = group_requests
                    .iter()
                    .map(|request| DependencyRequest {
                        eco: runtime_name.clone(),
                        name: request.name.clone(),
                        constraint: request.version.clone(),
                    })
                    .collect();
                let mut bias = lock.graph_for(&group_key).unwrap_or_default();
                if python.is_some() {
                    for node in &mut bias.nodes {
                        node.digest = None;
                    }
                }
                let result = if let Some(python) = python {
                    let source = PythonSource {
                        index: Arc::clone(&index),
                        context: python.context.clone(),
                    };
                    pakro_resolver::solve(&solve_requests, &bias, &source, "python")
                } else {
                    let source = NodeSource {
                        index: Arc::clone(&index),
                    };
                    pakro_resolver::solve(&solve_requests, &bias, &source, "node")
                };
                result.map_err(|error| eyre::eyre!("resolving {group_key}: {error}"))?
            }
        };
        let store = Store::open(&locs.store_root)?;
        let mut nodes = graph.nodes;
        for node in &mut nodes {
            if python.is_some() {
                node.digest = None;
            }
            if node
                .digest
                .as_deref()
                .is_some_and(|digest| !digest.is_empty())
            {
                continue;
            }
            let metadata = if let Some(python) = python {
                let metadata = index
                    .metadata_for_context(&node.name, &node.version, &python.context)
                    .await
                    .with_context(|| {
                        format!("fetching metadata for {} {}", node.name, node.version)
                    })?;
                validate_metadata(&metadata, &python.context)?;
                metadata
            } else {
                index
                    .npm_metadata(
                        &node.name,
                        &node.version,
                        node_runtime.as_ref().context("Node runtime missing")?,
                    )
                    .await
                    .with_context(|| {
                        format!("fetching metadata for {} {}", node.name, node.version)
                    })?
            };
            let mut built_bytes: Option<Vec<u8>> = None;
            let artifact_owned;
            let selected = if let Some(python) = python {
                select_python_artifact(&metadata.artifacts, python, &platform)
            } else {
                select_node_artifact(&metadata.artifacts)
            };
            let artifact = match selected {
                Some(artifact) => artifact,
                None if python.is_some() => {
                    let python = python.context("Python source-build binding missing")?;
                    let built = build_sdist_to_wheel(
                        &metadata,
                        &node.name,
                        &node.version,
                        python.handle.executable(),
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
                .context("selected artifact has no digest")?;
            let digest = digest_str
                .parse::<Digest>()
                .context("registry digest is malformed")?;
            let identity = PackageIdentity {
                eco: if python.is_some() {
                    Eco::Python
                } else {
                    Eco::Node
                },
                name: node.name.clone(),
                version: node.version.clone(),
                runtime: python.map(|_| RuntimeId {
                    eco: Eco::Python,
                    version: runtime_version.clone(),
                }),
                platform,
                digest,
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
        if let Some(python) = python {
            materialize_venv(
                &VenvSpec {
                    project_root: &locs.project_root,
                    runtime_name: &runtime_name,
                    runtime_version: &runtime_version,
                    interpreter_executable: python.handle.executable(),
                    language_version: &python.descriptor.language_version,
                },
                &graph,
                &store,
            )?;
        } else {
            super::materialize_node::materialize_node_modules(&locs.project_root, &graph, &store)?;
        }
        if let Some(packages) = PackageLock::from_graph(&group_key, &graph)
            .groups
            .remove(&group_key)
        {
            lock.groups.insert(group_key, packages);
        }
    }
    lock.write(&lock_path)?;
    if super::system::install_go_modules(&locs.project_root, locs)? {
        downloaded += 1;
    }
    if super::system::install_maven_dependencies(&locs.project_root, locs)? {
        downloaded += 1;
    }
    maybe_auto_gc(&locs.store_root);
    Ok(downloaded)
}

fn select_node_artifact(
    artifacts: &[pakro_index::ArtifactDescriptor],
) -> Option<&pakro_index::ArtifactDescriptor> {
    let mut candidates: Vec<_> = artifacts
        .iter()
        .filter(|artifact| {
            matches!(artifact.kind, ArtifactKind::Sdist) && artifact.digest.is_some()
        })
        .collect();
    candidates.sort_by(|left, right| left.filename.cmp(&right.filename));
    candidates.into_iter().next_back()
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
        && let Ok(report) = pakro_store::gc(&store)
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

/// npm metadata for the solver. Prerelease versions are filtered out of the
/// candidate list (npm's own default); exact prerelease pins are not
/// supported for node in this version.
struct NodeSource {
    index: Arc<Index>,
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
/// Shared by `pakro install` and `pakro use`.
pub async fn install_config_packages(
    config: &Arc<crate::config::Config>,
    extra: Vec<super::PackageInstallRequest>,
    runtimes: &[PackageRuntimeBinding],
    invocation: PackageInvocationPolicy,
) -> eyre::Result<usize> {
    let cli = super::CLI_PACKAGE_ARGS
        .get()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let owned = merge_owned_requests(
        config
            .config_files
            .values()
            .map(|cf| (cf.source(), cf.to_package_requests())),
        &extra,
        cli,
    );
    if owned.is_empty() {
        return Ok(0);
    }
    let inputs = prepare_inputs(&owned, runtimes, invocation.offline, |source| {
        config.invocation_locked_for(source, invocation.locked) || config.tool_config_locked(source)
    })?;
    guard_python_sidecar_migration(&inputs)?;
    let project_root = config
        .project_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| eyre::eyre!("could not locate project root"))?;
    let locations = Locations::from_config(&project_root)?;
    let index = Arc::new(Index::from_env(&locations.index_cache));
    install_packages(&inputs, &locations, index).await
}

/// A wheel produced by building an sdist locally.
struct BuiltArtifact {
    descriptor: pakro_index::ArtifactDescriptor,
    bytes: Vec<u8>,
}

/// Legacy manager fallback, pending native source builds in H4.
/// Uses the explicitly bound interpreter to invoke `pip wheel --no-deps`.
/// Correct executable binding does not make this a native build.
async fn build_sdist_to_wheel(
    metadata: &pakro_index::PackageMetadata,
    name: &str,
    version: &str,
    interpreter_executable: &Path,
) -> eyre::Result<BuiltArtifact> {
    let sdist = metadata
        .artifacts
        .iter()
        .find(|a| matches!(a.kind, pakro_index::ArtifactKind::Sdist))
        .context("no sdist available either")?;
    warn!(
        "no prebuilt wheel for {name} {version}; building from source (this runs the package's build code)"
    );
    let interpreter = interpreter_executable;
    if !interpreter.exists() {
        bail!("interpreter {} is not installed", interpreter.display());
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
    let status = std::process::Command::new(interpreter)
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
    let digest = pakro_store::Digest::from_sha256_bytes(&bytes).to_string();
    Ok(BuiltArtifact {
        descriptor: pakro_index::ArtifactDescriptor {
            filename,
            url: String::new(),
            digest: Some(digest),
            kind: pakro_index::ArtifactKind::Sdist,
        },
        bytes,
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod source_path_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn legacy_source_build_invokes_explicit_executable() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("selected-python");
        let marker = root.path().join("invoked");
        let quoted_marker = marker.display().to_string().replace('\'', "'\\''");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{quoted_marker}'\nexit 7\n"),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut server = mockito::Server::new_async().await;
        let download = server
            .mock("GET", "/demo.tar.gz")
            .with_body("controlled source bytes")
            .create_async()
            .await;
        let metadata = pakro_index::PackageMetadata {
            name: "demo".into(),
            version: "1.0".into(),
            requires_python: None,
            deps: vec![],
            artifacts: vec![pakro_index::ArtifactDescriptor {
                filename: "demo-1.0.tar.gz".into(),
                url: format!("{}/demo.tar.gz", server.url()),
                digest: None,
                kind: ArtifactKind::Sdist,
            }],
        };
        let error = build_sdist_to_wheel(&metadata, "demo", "1.0", &executable)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("pip wheel failed"), "{error}");
        assert!(
            std::fs::read_to_string(marker)
                .unwrap()
                .starts_with("-m\npip\nwheel\n")
        );
        download.assert_async().await;
    }
}
