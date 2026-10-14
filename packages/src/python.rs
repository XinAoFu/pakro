//! Actual interpreter observations wired into Python registry operations.
//! The temporary CPython prefix and legacy native matcher are not complete tags.

use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, bail, ensure};
use mise_index::{
    ArtifactDescriptor, ArtifactKind, Index, PackageMetadata, PythonMarkerContext,
    PythonMarkerContextBuilder, RuntimeInfo, wheel_matches,
};
use mise_resolver::solve::{Pep440, VersionAdapter};
use mise_resolver::{DependencyRequest, MetadataSource};
use mise_store::Platform;
use tokio::runtime::Handle;
use tokio::task::block_in_place;

use super::binding::{PackageRuntimeBinding, select_python_tool};
use crate::python_runtime::{
    self, PythonImplementation, PythonRuntimeDescriptor, PythonRuntimeHandle,
};
use crate::toolset::ToolVersion;

pub(crate) struct PythonPackageRuntime {
    pub handle: PythonRuntimeHandle,
    pub descriptor: PythonRuntimeDescriptor,
    pub context: PythonMarkerContext,
}

impl PythonPackageRuntime {
    pub(crate) async fn from_tool_version(tool: &ToolVersion) -> Result<Self> {
        let handle = PythonRuntimeHandle::from_tool_version(tool)?;
        let descriptor = python_runtime::probe(&handle, Duration::from_secs(10)).await?;
        let markers = &descriptor.markers;
        let context = PythonMarkerContext::new(PythonMarkerContextBuilder {
            implementation_name: &markers.implementation_name,
            implementation_version: &markers.implementation_version,
            os_name: &markers.os_name,
            platform_machine: &markers.platform_machine,
            platform_python_implementation: &markers.platform_python_implementation,
            platform_release: &markers.platform_release,
            platform_system: &markers.platform_system,
            platform_version: &markers.platform_version,
            python_full_version: &markers.python_full_version,
            python_version: &markers.python_version,
            sys_platform: &markers.sys_platform,
        })?;
        Ok(Self {
            handle,
            descriptor,
            context,
        })
    }

    fn legacy_native_profile(&self) -> bool {
        self.descriptor.implementation == PythonImplementation::CPython
            && self.descriptor.gil_disabled != Some(true)
            && self.descriptor.abiflags.as_deref() == Some("")
    }

    pub(crate) fn ensure_current_prefix(&self) -> Result<()> {
        ensure!(
            self.descriptor.implementation == PythonImplementation::CPython,
            "PyPy package installation requires the verified prefix implementation in H2"
        );
        ensure!(
            self.legacy_native_profile(),
            "Python ABI profile requires the verified prefix/tag implementation in H2"
        );
        Ok(())
    }
}

pub(crate) fn validate_metadata(
    metadata: &PackageMetadata,
    context: &PythonMarkerContext,
) -> Result<()> {
    if let Some(specifier) = &metadata.requires_python
        && !mise_index::python_requires_matches_for_context(specifier, context)?
    {
        bail!(
            "{} {} requires Python {specifier}; observed language version is {}",
            metadata.name,
            metadata.version,
            context.python_full_version()
        );
    }
    Ok(())
}

pub(crate) fn select_python_artifact<'a>(
    artifacts: &'a [ArtifactDescriptor],
    runtime: &PythonPackageRuntime,
    platform: &Platform,
) -> Option<&'a ArtifactDescriptor> {
    let observed = &runtime.descriptor.language_version;
    let legacy = RuntimeInfo {
        major: observed.major,
        minor: observed.minor,
    };
    let mut candidates: Vec<_> = artifacts
        .iter()
        .filter(|artifact| {
            let ArtifactKind::Wheel(tags) = &artifact.kind else {
                return false;
            };
            let pure = tags.python.iter().any(|tag| tag == "py3")
                && tags.abi.iter().any(|tag| tag == "none")
                && tags.platform.iter().any(|tag| tag == "any");
            artifact.digest.is_some()
                && (pure
                    || (runtime.legacy_native_profile() && wheel_matches(tags, &legacy, platform)))
        })
        .collect();
    candidates.sort_by(|left, right| left.filename.cmp(&right.filename));
    candidates.into_iter().next_back()
}

/// Uses the same explicit context for candidate listing and dependencies.
/// The synchronous resolver is run on mise's multi-thread Tokio runtime.
pub(crate) struct PythonSource {
    pub index: Arc<Index>,
    pub context: PythonMarkerContext,
}

impl MetadataSource for PythonSource {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        block_in_place(|| {
            Handle::current().block_on(self.index.versions_for_context(name, &self.context))
        })
        .map_err(|error| error.to_string())
    }

    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        // The resolver can prefer a locked version without calling versions().
        // Such a preference must still satisfy this interpreter's release list.
        let versions = self.versions(name)?;
        let metadata = block_in_place(|| {
            Handle::current().block_on(self.index.metadata_for_context(
                name,
                version,
                &self.context,
            ))
        })
        .map_err(|error| error.to_string())?;
        validate_metadata(&metadata, &self.context).map_err(|error| error.to_string())?;
        let selected = Pep440::parse_version(version)
            .ok_or_else(|| format!("invalid Python package version {version:?}"))?;
        if !versions
            .iter()
            .any(|candidate| Pep440::parse_version(candidate).as_ref() == Some(&selected))
        {
            return Err(format!(
                "{name} {version} has no release compatible with observed Python {}",
                self.context.python_full_version()
            ));
        }
        Ok(metadata
            .deps
            .into_iter()
            .map(|dependency| DependencyRequest {
                eco: "python".into(),
                name: dependency.name,
                constraint: dependency.constraint,
            })
            .collect())
    }
}

/// A compatible wheel candidate or an explicit lack of an observable runtime.
/// Available candidates still depend on the temporary matcher; macOS/libc
/// floors and complete ordered tags are pending, so this is not an install guarantee.
pub enum PythonPackageLatest {
    Available(Option<String>),
    Unavailable(String),
}

/// Query against one actual, already installed binding without installing tools.
/// Offline returns an explicit unavailable state before metadata access; H3 will
/// add verified metadata replay. Probe/registry errors are returned, never raw-latest fallback.
pub async fn latest_python_package(
    index: Arc<Index>,
    name: &str,
    selector: &str,
    bindings: &[PackageRuntimeBinding],
    offline: bool,
) -> Result<PythonPackageLatest> {
    if offline {
        return Ok(PythonPackageLatest::Unavailable(
            "offline metadata replay requires H3".into(),
        ));
    }
    let binding = match select_python_tool(bindings, selector) {
        Ok(binding) => binding,
        Err(error) => return Ok(PythonPackageLatest::Unavailable(error.to_string())),
    };
    let runtime = PythonPackageRuntime::from_tool_version(&binding.tool).await?;
    let versions = index
        .stable_python_versions_for_context(name, &runtime.context)
        .await?;
    for version in versions {
        let metadata = index
            .metadata_for_context(name, &version, &runtime.context)
            .await?;
        if let Some(specifier) = &metadata.requires_python
            && !mise_index::python_requires_matches_for_context(specifier, &runtime.context)?
        {
            continue;
        }
        if select_python_artifact(&metadata.artifacts, &runtime, &Platform::detect()).is_some() {
            return Ok(PythonPackageLatest::Available(Some(version)));
        }
    }
    Ok(PythonPackageLatest::Available(None))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use crate::packages::PackageInstallRequest;
    use crate::packages::binding::{GroupPolicy, PackageInstallInputs};
    use crate::packages::install::{Locations, install_packages};
    use crate::packages::lockfile::{LOCKFILE_NAME, PackageLock};
    use crate::toolset::{ToolRequest, ToolSource};
    use base64::Engine as _;
    use mise_resolver::{ResolvedGraph, ResolvedNode};
    use mise_store::Digest;
    use serde_json::{Value, json};
    use sha2::Digest as _;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn tool(root: &Path, micro: u32, pypy: bool) -> ToolVersion {
        let install = root.join("actual-runtime");
        std::fs::create_dir_all(install.join("bin")).unwrap();
        let executable = install.join("bin/python");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = executable.canonicalize().unwrap();
        let language =
            json!({"major":3,"minor":10,"micro":micro,"release_level":"final","serial":0});
        let implementation = if pypy {
            json!({"major":7,"minor":3,"micro":17,"release_level":"final","serial":0})
        } else {
            language.clone()
        };
        let full = format!("3.10.{micro}");
        let value = json!({
            "schema":1,"observation_mode":"base_runtime_site_disabled",
            "probe_flags":["-E","-s","-S","-B","-c"],"executable":executable,
            "implementation":if pypy {"pypy"} else {"cpython"},
            "language_version":language,"implementation_version":implementation,
            "implementation_version_source":if pypy {"pypy_version_info"} else {"sys_version_info"},
            "markers":{
                "implementation_name":if pypy {"pypy"} else {"cpython"},
                "implementation_version":if pypy {"7.3.17"} else {&full},
                "os_name":"posix","platform_machine":"aarch64",
                "platform_python_implementation":if pypy {"PyPy"} else {"CPython"},
                "platform_release":"6.1.0","platform_system":"Linux","platform_version":"fixture kernel",
                "python_full_version":full,"python_version":"3.10","sys_platform":"linux"
            },
            "soabi":if pypy {"pypy310-pp73"} else {"cpython-310"},
            "ext_suffix":".so","abiflags":"","extension_suffixes":[".so"],
            "gil_disabled":false,"sysconfig_paths":{"stdlib":"/fixture/lib/python3.10"}
        });
        let body = value.to_string().replace('\'', "'\\''");
        std::fs::write(&executable, format!("#!/bin/sh\nprintf '%s' '{body}'\n")).unwrap();
        let backend = Arc::new(crate::plugins::core::new_backend_arg("python"));
        let request = ToolRequest::new(backend, "pbs-build-opaque", ToolSource::Argument).unwrap();
        let mut tool = ToolVersion::new(request, "pbs-build-opaque".into());
        tool.install_path = Some(install);
        tool.install_satisfied = Some(true);
        tool
    }

    fn artifact(filename: &str) -> ArtifactDescriptor {
        ArtifactDescriptor {
            filename: filename.into(),
            url: "http://fixture/unused".into(),
            digest: Some(format!("sha256-{}", "a".repeat(64))),
            kind: mise_index::parse_wheel_filename(filename)
                .map(|wheel| ArtifactKind::Wheel(wheel.tags))
                .unwrap_or(ArtifactKind::Sdist),
        }
    }

    fn listing(files: &[(&str, &str)]) -> Value {
        let releases: serde_json::Map<String, Value> = files
            .iter()
            .map(|(version, specifier)| {
                (
                    version.to_string(),
                    json!([{"filename":"fixture.whl","requires_python":specifier}]),
                )
            })
            .collect();
        json!({"info":{"name":"demo","version":"1.0"},"releases":releases})
    }

    fn metadata(name: &str, deps: &[&str], filename: &str, url: &str, digest: &str) -> Value {
        json!({"info":{"name":name,"version":"1.0","requires_python":">=3.10.14",
            "requires_dist":deps},"urls":[{"filename":filename,"url":url,
            "digests":{"sha256":digest},"packagetype":"bdist_wheel"}]})
    }

    fn wheel(name: &str) -> Vec<u8> {
        let files = [
            (format!("{name}/__init__.py"), b"VALUE = 42\n".to_vec()),
            (format!("{name}-1.0.dist-info/METADATA"),
                format!("Metadata-Version: 2.1\nName: {name}\nVersion: 1.0\n").into_bytes()),
            (format!("{name}-1.0.dist-info/WHEEL"),
                b"Wheel-Version: 1.0\nGenerator: fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec()),
        ];
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let mut record = String::new();
        for (filename, bytes) in files {
            let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(&bytes));
            record.push_str(&format!("{filename},sha256={digest},{}\n", bytes.len()));
            writer
                .start_file(filename, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let record_name = format!("{name}-1.0.dist-info/RECORD");
        record.push_str(&format!("{record_name},,\n"));
        writer
            .start_file(record_name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(record.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn inputs(tool: ToolVersion) -> PackageInstallInputs {
        let key = "python@pbs-build-opaque".to_string();
        PackageInstallInputs {
            requests: vec![PackageInstallRequest {
                runtime_name: "python".into(),
                runtime_version: "pbs-build-opaque".into(),
                name: "demo".into(),
                version: "1.0".into(),
            }],
            python_runtimes: [(key.clone(), tool)].into_iter().collect(),
            policies: [(
                key,
                GroupPolicy {
                    locked: false,
                    offline: false,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn locations(root: &Path) -> Locations {
        Locations {
            config_root: root.into(),
            project_root: root.into(),
            store_root: root.join("store"),
            index_cache: root.join("index"),
        }
    }

    #[tokio::test]
    async fn adapter_uses_actual_patch_implementation_and_all_observed_markers() {
        let root = tempfile::tempdir().unwrap();
        let runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, true))
            .await
            .unwrap();
        assert_eq!(runtime.context.python_full_version(), "3.10.14");
        assert_eq!(runtime.context.implementation_name(), "pypy");
        let value = metadata(
            "demo",
            &[
                "patchdep==1.0; python_full_version == '3.10.14'",
                "impldep==1.0; implementation_name == 'pypy' and implementation_version == '7.3.17'",
                "platformdep==1.0; os_name == 'posix' and platform_machine == 'aarch64' and platform_python_implementation == 'PyPy' and platform_release == '6.1.0' and platform_system == 'Linux' and platform_version == 'fixture kernel' and python_version == '3.10' and sys_platform == 'linux'",
                "wrongdep==1.0; implementation_name == 'cpython'",
            ],
            "demo-1.0-py3-none-any.whl",
            "http://fixture/demo.whl",
            &"a".repeat(64),
        );
        let parsed = mise_index::parse_pypi_response_for_context(&value, &runtime.context).unwrap();
        assert_eq!(
            parsed
                .deps
                .iter()
                .map(|dep| dep.name.as_str())
                .collect::<Vec<_>>(),
            ["patchdep", "impldep", "platformdep"]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_checks_patch_both_in_listing_and_exact_metadata() {
        let root = tempfile::tempdir().unwrap();
        let actual = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/demo/json")
            .with_body(listing(&[("1.0", "!=3.10.14"), ("2.0", ">=3.10.14")]).to_string())
            .create_async()
            .await;
        let mut value = metadata(
            "demo",
            &[],
            "demo-1.0-py3-none-any.whl",
            "http://fixture/a",
            &"a".repeat(64),
        );
        value["info"]["requires_python"] = json!("!=3.10.14");
        let exact = server
            .mock("GET", "/demo/1.0/json")
            .with_body(value.to_string())
            .create_async()
            .await;
        let source = PythonSource {
            index: Arc::new(Index::new(root.path().join("cache")).with_base_url(server.url())),
            context: actual.context,
        };
        assert_eq!(source.versions("demo").unwrap(), ["2.0"]);
        assert!(
            source
                .dependencies("demo", "1.0")
                .unwrap_err()
                .contains("3.10.14")
        );
        list.assert_async().await;
        exact.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preferred_version_still_checks_context_listing_when_exact_metadata_allows_it() {
        let root = tempfile::tempdir().unwrap();
        let actual = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/demo/json")
            .with_body(listing(&[("1.0", "!=3.10.14")]).to_string())
            .create_async()
            .await;
        let value = metadata(
            "demo",
            &[],
            "demo-1.0-py3-none-any.whl",
            "http://fixture/demo.whl",
            &"a".repeat(64),
        );
        let exact = server
            .mock("GET", "/demo/1.0/json")
            .with_body(value.to_string())
            .create_async()
            .await;
        let source = PythonSource {
            index: Arc::new(Index::new(root.path().join("cache")).with_base_url(server.url())),
            context: actual.context,
        };
        let error = source.dependencies("demo", "1.0").unwrap_err();
        assert!(
            error.contains("no release compatible with observed Python 3.10.14"),
            "{error}"
        );
        list.assert_async().await;
        exact.assert_async().await;
    }

    #[tokio::test]
    async fn pypy_only_has_generic_pure_candidate_and_unverified_prefix_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, true))
            .await
            .unwrap();
        for filename in [
            "demo-1.0-cp310-cp310-any.whl",
            "demo-1.0-cp39-abi3-any.whl",
            "demo-1.0-pp310-pypy310_pp73-any.whl",
            "demo-1.0.tar.gz",
        ] {
            assert!(
                select_python_artifact(&[artifact(filename)], &runtime, &Platform::detect())
                    .is_none()
            );
        }
        assert!(
            select_python_artifact(
                &[artifact("demo-1.0-py3-none-any.whl")],
                &runtime,
                &Platform::detect()
            )
            .is_some()
        );
        assert!(
            runtime
                .ensure_current_prefix()
                .unwrap_err()
                .to_string()
                .contains("PyPy")
        );
    }

    #[tokio::test]
    async fn free_threaded_and_nonstandard_abi_cannot_use_legacy_native_matcher() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        for (flags, gil) in [
            (Some(""), Some(true)),
            (Some("d"), Some(false)),
            (None, None),
        ] {
            runtime.descriptor.abiflags = flags.map(str::to_string);
            runtime.descriptor.gil_disabled = gil;
            for filename in ["demo-1.0-cp310-cp310-any.whl", "demo-1.0-cp39-abi3-any.whl"] {
                assert!(
                    select_python_artifact(&[artifact(filename)], &runtime, &Platform::detect())
                        .is_none()
                );
            }
            assert!(
                select_python_artifact(
                    &[artifact("demo-1.0-py3-none-any.whl")],
                    &runtime,
                    &Platform::detect()
                )
                .is_some()
            );
            assert!(runtime.ensure_current_prefix().is_err());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pypy_install_rejects_before_metadata_store_or_lock_even_with_old_digest() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, true);
        let locs = locations(root.path());
        let lock = root.path().join(LOCKFILE_NAME);
        let before = format!(
            "[groups]\n'python@pbs-build-opaque' = [{{name='demo',version='1.0',digest='sha256-{}'}}]\n",
            "a".repeat(64)
        );
        std::fs::write(&lock, &before).unwrap();
        let mut server = mockito::Server::new_async().await;
        let no_metadata = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(503)
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(server.url()));
        let error = install_packages(&inputs(actual), &locs, index)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("PyPy package installation"),
            "{error}"
        );
        assert!(!locs.store_root.exists());
        assert!(!locs.index_cache.exists());
        assert!(!root.path().join(".mise/venvs").exists());
        assert_eq!(std::fs::read_to_string(lock).unwrap(), before);
        no_metadata.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn online_legacy_graph_is_resolved_again_with_new_markers_and_roots() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, false);
        let locs = locations(root.path());
        let old = ResolvedGraph {
            nodes: ["demo", "olddep", "removed-root"]
                .into_iter()
                .map(|name| ResolvedNode {
                    eco: "python".into(),
                    name: name.into(),
                    version: "1.0".into(),
                    digest: Some(format!("sha256-{}", "f".repeat(64))),
                    deps: vec![],
                })
                .collect(),
        };
        PackageLock::from_graph("python@pbs-build-opaque", &old)
            .write(&root.path().join(LOCKFILE_NAME))
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut mocks = Vec::new();
        for name in ["demo", "newdep"] {
            mocks.push(
                server
                    .mock("GET", format!("/{name}/json").as_str())
                    .with_body(listing(&[("1.0", ">=3.10.14")]).to_string())
                    .create_async()
                    .await,
            );
            let bytes = wheel(name);
            let digest = Digest::from_sha256_bytes(&bytes).to_string();
            let deps: Vec<&str> = if name == "demo" {
                vec![
                    "newdep==1.0; python_full_version >= '3.10.14'",
                    "olddep==1.0; python_full_version < '3.10.14'",
                ]
            } else {
                vec![]
            };
            let value = metadata(
                name,
                &deps,
                &format!("{name}-1.0-py3-none-any.whl"),
                &format!("{}/{name}.whl", server.url()),
                digest.strip_prefix("sha256-").unwrap(),
            );
            mocks.push(
                server
                    .mock("GET", format!("/{name}/1.0/json").as_str())
                    .with_body(value.to_string())
                    .create_async()
                    .await,
            );
            mocks.push(
                server
                    .mock("GET", format!("/{name}.whl").as_str())
                    .with_body(bytes)
                    .create_async()
                    .await,
            );
        }
        let old_metadata = server
            .mock("GET", "/olddep/1.0/json")
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(server.url()));
        assert_eq!(
            install_packages(&inputs(actual.clone()), &locs, index)
                .await
                .unwrap(),
            2
        );
        let graph = PackageLock::read(&root.path().join(LOCKFILE_NAME))
            .unwrap()
            .unwrap()
            .graph_for("python@pbs-build-opaque")
            .unwrap();
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["demo", "newdep"]
        );
        let old_digest = format!("sha256-{}", "f".repeat(64));
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.digest.as_deref() != Some(old_digest.as_str()))
        );
        let venv = root.path().join(".mise/venvs/python-pbs-build-opaque");
        assert_eq!(
            std::fs::read_link(venv.join("bin/python")).unwrap(),
            actual
                .install_path()
                .join("bin/python")
                .canonicalize()
                .unwrap()
        );
        assert!(
            venv.join("lib/python3.10/site-packages/demo/__init__.py")
                .is_file()
        );
        let cfg = std::fs::read_to_string(venv.join("pyvenv.cfg")).unwrap();
        assert!(cfg.contains("version = 3.10.14"));
        assert!(!cfg.contains("version = pbs-build-opaque"));
        for mock in mocks {
            mock.assert_async().await;
        }
        old_metadata.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn latest_uses_real_patch_and_pypy_pure_candidates_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let releases = server
            .mock("GET", "/demo/json")
            .with_body(
                listing(&[("1.0", ">=3.10"), ("2.0", "!=3.10.14"), ("3.0", ">=3.10")]).to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let mut native = metadata(
            "demo",
            &[],
            "demo-3.0-cp310-cp310-any.whl",
            "http://fixture/3",
            &"a".repeat(64),
        );
        native["info"]["version"] = json!("3.0");
        native["info"]["requires_python"] = json!(">=3.10");
        let native_mock = server
            .mock("GET", "/demo/3.0/json")
            .with_body(native.to_string())
            .expect(1)
            .create_async()
            .await;
        let mut pure = metadata(
            "demo",
            &[],
            "demo-1.0-py3-none-any.whl",
            "http://fixture/1",
            &"a".repeat(64),
        );
        pure["info"]["requires_python"] = json!(">=3.10");
        let pure_mock = server
            .mock("GET", "/demo/1.0/json")
            .with_body(pure.to_string())
            .create_async()
            .await;
        let index = Arc::new(Index::new(root.path().join("cache")).with_base_url(server.url()));
        let cpython = PackageRuntimeBinding {
            tool: tool(root.path(), 13, false),
            locked: false,
        };
        let current =
            latest_python_package(index.clone(), "demo", "pbs-build-opaque", &[cpython], false)
                .await
                .unwrap();
        assert!(
            matches!(current, PythonPackageLatest::Available(Some(version)) if version == "3.0")
        );
        let pypy = PackageRuntimeBinding {
            tool: tool(root.path(), 14, true),
            locked: false,
        };
        let current =
            latest_python_package(index.clone(), "demo", "pbs-build-opaque", &[pypy], false)
                .await
                .unwrap();
        assert!(
            matches!(current, PythonPackageLatest::Available(Some(version)) if version == "1.0")
        );
        assert!(matches!(
            latest_python_package(index.clone(), "demo", "missing", &[], false)
                .await
                .unwrap(),
            PythonPackageLatest::Unavailable(_)
        ));
        assert!(matches!(
            latest_python_package(index, "demo", "missing", &[], true)
                .await
                .unwrap(),
            PythonPackageLatest::Unavailable(_)
        ));
        releases.assert_async().await;
        native_mock.assert_async().await;
        pure_mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_requires_python_rejection_precedes_download_store_and_lock_write() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, false);
        let locs = locations(root.path());
        let lock_path = root.path().join(LOCKFILE_NAME);
        let before =
            "[groups]\n'python@pbs-build-opaque' = [{name='demo',version='1.0',digest=''}]\n";
        std::fs::write(&lock_path, before).unwrap();
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/demo/json")
            .with_body(listing(&[("1.0", ">=3.10")]).to_string())
            .create_async()
            .await;
        let mut value = metadata(
            "demo",
            &[],
            "demo-1.0-py3-none-any.whl",
            &format!("{}/demo.whl", server.url()),
            &"a".repeat(64),
        );
        value["urls"][0]["requires_python"] = json!("!=3.10.14");
        let exact = server
            .mock("GET", "/demo/1.0/json")
            .with_body(value.to_string())
            .create_async()
            .await;
        let bytes = server
            .mock("GET", "/demo.whl")
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(server.url()));
        let error = install_packages(&inputs(actual), &locs, index)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no downloadable artifacts"),
            "{error}"
        );
        assert!(!locs.store_root.exists());
        assert_eq!(std::fs::read_to_string(lock_path).unwrap(), before);
        list.assert_async().await;
        exact.assert_async().await;
        bytes.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires PAK_TEST_PYTHON pointing to an existing absolute CPython executable"]
    async fn host_cpython_materialized_prefix_imports_pure_wheel() {
        let executable = std::path::PathBuf::from(
            std::env::var_os("PAK_TEST_PYTHON")
                .expect("set PAK_TEST_PYTHON to an existing absolute CPython"),
        );
        assert!(executable.is_absolute());
        let executable = executable.canonicalize().unwrap();
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("host-runtime");
        std::fs::create_dir_all(install.join("bin")).unwrap();
        std::os::unix::fs::symlink(&executable, install.join("bin/python")).unwrap();
        let backend = Arc::new(crate::plugins::core::new_backend_arg("python"));
        let request = ToolRequest::new(backend, "host-observed", ToolSource::Argument).unwrap();
        let mut actual = ToolVersion::new(request, "host-observed".into());
        actual.install_path = Some(install);
        actual.install_satisfied = Some(true);
        let observed = PythonPackageRuntime::from_tool_version(&actual)
            .await
            .unwrap();
        assert_eq!(
            observed.descriptor.implementation,
            PythonImplementation::CPython
        );
        observed.ensure_current_prefix().unwrap();
        let locs = locations(root.path());
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/demo/json")
            .with_body(listing(&[("1.0", ">=3.0")]).to_string())
            .create_async()
            .await;
        let wheel_bytes = wheel("demo");
        let digest = Digest::from_sha256_bytes(&wheel_bytes).to_string();
        let mut value = metadata(
            "demo",
            &[],
            "demo-1.0-py3-none-any.whl",
            &format!("{}/demo.whl", server.url()),
            digest.strip_prefix("sha256-").unwrap(),
        );
        value["info"]["requires_python"] = json!(">=3.0");
        let exact = server
            .mock("GET", "/demo/1.0/json")
            .with_body(value.to_string())
            .create_async()
            .await;
        let bytes = server
            .mock("GET", "/demo.whl")
            .with_body(wheel_bytes)
            .create_async()
            .await;
        let key = "python@host-observed".to_string();
        let invocation = PackageInstallInputs {
            requests: vec![PackageInstallRequest {
                runtime_name: "python".into(),
                runtime_version: "host-observed".into(),
                name: "demo".into(),
                version: "1.0".into(),
            }],
            python_runtimes: [(key.clone(), actual)].into_iter().collect(),
            policies: [(
                key,
                GroupPolicy {
                    locked: false,
                    offline: false,
                },
            )]
            .into_iter()
            .collect(),
        };
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(server.url()));
        assert_eq!(
            install_packages(&invocation, &locs, index).await.unwrap(),
            1
        );
        let venv = root.path().join(".mise/venvs/python-host-observed");
        let output = std::process::Command::new(venv.join("bin/python"))
            .env_clear()
            .args([
                "-E",
                "-c",
                "import demo,sys; print(demo.VALUE); print(sys.prefix)",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("42"));
        assert_eq!(
            std::path::PathBuf::from(lines.next().unwrap())
                .canonicalize()
                .unwrap(),
            venv.canonicalize().unwrap()
        );
        list.assert_async().await;
        exact.assert_async().await;
        bytes.assert_async().await;
    }
}
