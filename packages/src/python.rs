//! Actual interpreter observations wired into Python registry operations.
//! The temporary CPython prefix and legacy native matcher are not complete tags.

use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, ensure};
use pakro_index::{
    ArtifactDescriptor, ArtifactKind, Index, PackageMetadata, PythonMarkerContext,
    PythonMarkerContextBuilder, SupportedWheelTags, normalize_py_name, select_wheel_artifact,
};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use pakro_resolver::{DependencyRequest, MetadataSource};
use pakro_store::{Digest, Platform};
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
    pub supported: SupportedWheelTags,
    pub observed_platform: Option<Platform>,
    pub capability_reason: Option<String>,
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
        let target = super::python_target::wheel_target(&descriptor)?;
        if let Some(reason) = &target.capability_reason {
            warn!("Python compatibility: {reason}");
        }
        Ok(Self {
            handle,
            supported: target.supported,
            observed_platform: target.observed_platform,
            capability_reason: target.capability_reason,
            descriptor,
            context,
        })
    }

    fn legacy_native_profile(&self) -> bool {
        self.descriptor.implementation == PythonImplementation::CPython
            && self.descriptor.gil_disabled == Some(false)
            && self
                .descriptor
                .compatibility
                .as_ref()
                .and_then(|value| value.debug)
                == Some(false)
            && self.descriptor.abiflags.as_deref() == Some("")
    }

    pub(crate) fn ensure_current_prefix(&self) -> Result<()> {
        ensure!(
            self.descriptor.implementation == PythonImplementation::CPython,
            "PyPy package installation requires the verified prefix implementation in H2"
        );
        ensure!(
            self.observed_platform.is_some(),
            "Python installation requires a verified observed target: {}",
            self.capability_reason
                .as_deref()
                .unwrap_or("target cannot be materialized")
        );
        ensure!(
            self.legacy_native_profile(),
            "Python ABI profile requires the verified prefix/tag implementation in H2"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonCandidatePolicy {
    WheelQuery,
    Install,
}
#[derive(Debug)]
pub(crate) enum PythonReleaseCandidate<'a> {
    Wheel {
        artifact: &'a ArtifactDescriptor,
        digest: Digest,
    },
    LegacySdist {
        artifact: &'a ArtifactDescriptor,
        digest: Digest,
    },
}
pub(crate) fn python_sha256(artifact: &ArtifactDescriptor) -> Result<Digest> {
    let raw = artifact
        .digest
        .as_deref()
        .ok_or_else(|| eyre::eyre!("selected Python artifact has no SHA256 digest"))?;
    let digest = raw
        .parse::<Digest>()
        .map_err(|_| eyre::eyre!("selected Python artifact has malformed SHA256 digest"))?;
    ensure!(
        matches!(digest, Digest::Sha256(_)),
        "selected Python artifact requires SHA256 digest"
    );
    Ok(digest)
}
pub(crate) fn safe_source_filename(filename: &str) -> Result<()> {
    ensure!(
        !filename.is_empty()
            && filename != "."
            && filename != ".."
            && !filename
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\'),
        "selected source artifact filename is not a safe single component"
    );
    Ok(())
}
fn requested_python_identity(metadata: &PackageMetadata, name: &str, version: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !metadata.name.is_empty(),
        "Python package name is empty"
    );
    // PyPA distribution names start/end alphanumeric and contain only these ASCII atoms.
    // Full wheel/metadata validation remains in pakro-index; no transitive crate dependency.
    let valid_name = |value: &str| {
        value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    ensure!(valid_name(name), "invalid requested Python package name");
    ensure!(
        valid_name(&metadata.name),
        "invalid metadata Python package name"
    );
    ensure!(
        normalize_py_name(name) == normalize_py_name(&metadata.name),
        "Python metadata name does not match requested package"
    );
    let expected_version = Pep440::parse_version(version)
        .ok_or_else(|| eyre::eyre!("invalid requested Python package version"))?;
    let actual_version = Pep440::parse_version(&metadata.version)
        .ok_or_else(|| eyre::eyre!("invalid metadata Python package version"))?;
    ensure!(
        expected_version == actual_version,
        "Python metadata version does not match requested release"
    );
    Ok(())
}
pub(crate) fn select_python_release<'a>(
    metadata: &'a PackageMetadata,
    name: &str,
    version: &str,
    context: &PythonMarkerContext,
    supported: &SupportedWheelTags,
    policy: PythonCandidatePolicy,
) -> Result<Option<PythonReleaseCandidate<'a>>> {
    requested_python_identity(metadata, name, version)?;
    let wheel = select_wheel_artifact(metadata, supported)?;
    if let Some(specifier) = &metadata.requires_python
        && !pakro_index::python_requires_matches_for_context(specifier, context)?
    {
        return Ok(None);
    }
    if let Some(artifact) = wheel {
        return Ok(Some(PythonReleaseCandidate::Wheel {
            artifact,
            digest: python_sha256(artifact)?,
        }));
    }
    if policy == PythonCandidatePolicy::WheelQuery {
        return Ok(None);
    }
    let Some(artifact) = metadata
        .artifacts
        .iter()
        .find(|artifact| matches!(artifact.kind, ArtifactKind::Sdist))
    else {
        return Ok(None);
    };
    safe_source_filename(&artifact.filename)?;
    Ok(Some(PythonReleaseCandidate::LegacySdist {
        artifact,
        digest: python_sha256(artifact)?,
    }))
}
pub(crate) fn python_install_platform(runtime: &PythonPackageRuntime) -> Result<Platform> {
    runtime.ensure_current_prefix()?;
    runtime.observed_platform.ok_or_else(|| {
        eyre::eyre!("Python installation requires a validated observed target platform")
    })
}

/// Uses the same explicit context for candidate listing and dependencies.
/// The synchronous resolver is run on pakro's multi-thread Tokio runtime.
pub(crate) struct PythonSource {
    pub index: Arc<Index>,
    pub context: PythonMarkerContext,
    pub supported: SupportedWheelTags,
}
async fn compatible_python_versions(
    index: &Index,
    name: &str,
    context: &PythonMarkerContext,
    supported: &SupportedWheelTags,
) -> Result<Vec<String>> {
    let listed = index.versions_for_context(name, context).await?;
    let mut compatible = Vec::new();
    for version in listed {
        let metadata = index.metadata_for_context(name, &version, context).await?;
        if select_python_release(
            &metadata,
            name,
            &version,
            context,
            supported,
            PythonCandidatePolicy::Install,
        )?
        .is_some()
        {
            compatible.push(version);
        }
    }
    Ok(compatible)
}
impl MetadataSource for PythonSource {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        block_in_place(|| {
            Handle::current().block_on(compatible_python_versions(
                &self.index,
                name,
                &self.context,
                &self.supported,
            ))
        })
        .map_err(|error| error.to_string())
    }
    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        let versions = self.versions(name)?;
        let metadata = block_in_place(|| {
            Handle::current().block_on(self.index.metadata_for_context(
                name,
                version,
                &self.context,
            ))
        })
        .map_err(|error| error.to_string())?;
        let selected = Pep440::parse_version(version)
            .ok_or_else(|| "invalid Python package version".to_string())?;
        let listed = versions
            .iter()
            .any(|candidate| Pep440::parse_version(candidate).as_ref() == Some(&selected));
        let candidate = select_python_release(
            &metadata,
            name,
            version,
            &self.context,
            &self.supported,
            PythonCandidatePolicy::Install,
        )
        .map_err(|error| error.to_string())?;
        if !listed || candidate.is_none() {
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
/// A returned version has a compatible wheel and SHA256 for the observed target.
/// Full prefix/layout and transactional installation remain separate guarantees.
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
    for version in index
        .stable_python_versions_for_context(name, &runtime.context)
        .await?
    {
        let metadata = index
            .metadata_for_context(name, &version, &runtime.context)
            .await?;
        if let Some(PythonReleaseCandidate::Wheel { .. }) = select_python_release(
            &metadata,
            name,
            &version,
            &runtime.context,
            &runtime.supported,
            PythonCandidatePolicy::WheelQuery,
        )? {
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
    use pakro_resolver::{ResolvedGraph, ResolvedNode};
    use pakro_store::Digest;
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
            "ext_suffix":if pypy {".pypy310-pp73-aarch64-linux-gnu.so"} else {".cpython-310-aarch64-linux-gnu.so"},"abiflags":"","extension_suffixes":[".so"],
            "gil_disabled":false,
            "compatibility":{"pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,
                "platform":{"os":"linux","elf":{"class":2,"little_endian":true,"machine":183,"flags":0,
                    "interpreter":"/lib/ld-linux-aarch64.so.1"},"libc":{"family":"glibc","major":2,"minor":17},
                    "manylinux":{"state":"default"}}},
            "sysconfig_paths":{"stdlib":"/fixture/lib/python3.10"}
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
            kind: pakro_index::parse_wheel_filename(filename)
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
                    json!([{"filename":format!("demo-{version}-py3-none-any.whl"),
                        "url":format!("http://fixture/demo-{version}-py3-none-any.whl"),
                        "digests":{"sha256":"a".repeat(64)},"packagetype":"bdist_wheel",
                        "requires_python":specifier}]),
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
        let parsed =
            pakro_index::parse_pypi_response_for_context(&value, &runtime.context).unwrap();
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
        let mut allowed = metadata(
            "demo",
            &[],
            "demo-2.0-py3-none-any.whl",
            "http://fixture/two",
            &"a".repeat(64),
        );
        allowed["info"]["version"] = json!("2.0");
        let allowed_exact = server
            .mock("GET", "/demo/2.0/json")
            .with_body(allowed.to_string())
            .expect(1)
            .create_async()
            .await;
        let source = PythonSource {
            index: Arc::new(Index::new(root.path().join("cache")).with_base_url(server.url())),
            context: actual.context,
            supported: actual.supported,
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
        allowed_exact.assert_async().await;
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
            supported: actual.supported,
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
    async fn pypy_queries_observed_native_tags_but_unverified_prefix_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, true))
            .await
            .unwrap();
        for filename in [
            "demo-1.0-cp310-cp310-linux_aarch64.whl",
            "demo-1.0-cp39-abi3-linux_aarch64.whl",
        ] {
            let metadata = package_metadata(vec![artifact(filename)]);
            assert!(
                select_python_release(
                    &metadata,
                    "demo",
                    "1.0",
                    &runtime.context,
                    &runtime.supported,
                    PythonCandidatePolicy::WheelQuery
                )
                .unwrap()
                .is_none()
            );
        }
        for filename in [
            "demo-1.0-pp310-pypy310_pp73-linux_aarch64.whl",
            "demo-1.0-py3-none-any.whl",
        ] {
            let metadata = package_metadata(vec![artifact(filename)]);
            assert!(matches!(
                select_python_release(
                    &metadata,
                    "demo",
                    "1.0",
                    &runtime.context,
                    &runtime.supported,
                    PythonCandidatePolicy::WheelQuery
                )
                .unwrap(),
                Some(PythonReleaseCandidate::Wheel { .. })
            ));
        }
        assert!(
            runtime
                .ensure_current_prefix()
                .unwrap_err()
                .to_string()
                .contains("PyPy")
        );
    }

    #[tokio::test]
    async fn abi_observation_errors_and_query_capabilities_are_separate_from_prefix() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        runtime.descriptor.gil_disabled = Some(true);
        assert!(super::super::python_target::wheel_target(&runtime.descriptor).is_err());
        assert!(runtime.ensure_current_prefix().is_err());
        runtime.descriptor.gil_disabled = Some(false);
        runtime.descriptor.abiflags = Some("d".into());
        runtime.descriptor.compatibility.as_mut().unwrap().debug = Some(true);
        let target = super::super::python_target::wheel_target(&runtime.descriptor).unwrap();
        let native =
            pakro_index::parse_wheel_filename("demo-1.0-cp310-cp310d-linux_aarch64.whl").unwrap();
        assert!(target.supported.best_rank(&native.tags).unwrap().is_some());
        assert!(runtime.ensure_current_prefix().is_err());
        runtime.descriptor.compatibility = None;
        runtime.descriptor.abiflags = None;
        runtime.descriptor.gil_disabled = None;
        let target = super::super::python_target::wheel_target(&runtime.descriptor).unwrap();
        assert!(target.capability_reason.unwrap().contains("pure"));
        assert!(target.supported.best_rank(&native.tags).unwrap().is_none());
        assert!(runtime.ensure_current_prefix().is_err());
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
        assert!(!root.path().join(".pakro/venvs").exists());
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
        let venv = root.path().join(".pakro/venvs/python-pbs-build-opaque");
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
            "demo-3.0-cp310-cp310-linux_aarch64.whl",
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
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an existing absolute CPython executable"]
    async fn host_cpython_materialized_prefix_imports_pure_wheel() {
        let executable = std::path::PathBuf::from(
            std::env::var_os("PAKRO_TEST_PYTHON")
                .expect("set PAKRO_TEST_PYTHON to an existing absolute CPython"),
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
        let venv = root.path().join(".pakro/venvs/python-host-observed");
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
    fn ranked_tags() -> SupportedWheelTags {
        SupportedWheelTags::new(vec![
            pakro_index::WheelTag::new("cp310", "cp310", "manylinux_2_17_aarch64").unwrap(),
            pakro_index::WheelTag::new("cp39", "abi3", "manylinux_2_17_aarch64").unwrap(),
            pakro_index::WheelTag::new("py3", "none", "any").unwrap(),
        ])
        .unwrap()
    }
    fn package_metadata(files: Vec<ArtifactDescriptor>) -> PackageMetadata {
        PackageMetadata {
            name: "demo".into(),
            version: "1.0".into(),
            requires_python: None,
            deps: vec![],
            artifacts: files,
        }
    }
    async fn fixture_context() -> (tempfile::TempDir, PythonMarkerContext) {
        let root = tempfile::tempdir().unwrap();
        let actual = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        (root, actual.context)
    }
    #[tokio::test]
    async fn integration_request_identity_rejects_foreign_and_empty_metadata() {
        let (_root, context) = fixture_context().await;
        for (name, version) in [
            ("other", "1.0"),
            ("demo", "2.0"),
            ("", "1.0"),
            ("demo", ""),
            ("bad name", "1.0"),
        ] {
            let mut value = package_metadata(vec![artifact("demo-1.0-py3-none-any.whl")]);
            value.name = name.into();
            value.version = version.into();
            assert!(
                select_python_release(
                    &value,
                    "demo",
                    "1.0",
                    &context,
                    &ranked_tags(),
                    PythonCandidatePolicy::Install
                )
                .is_err(),
                "{name:?} {version:?}"
            );
        }
        let valid = package_metadata(vec![artifact("demo-1.0-py3-none-any.whl")]);
        assert!(
            select_python_release(
                &valid,
                "",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
        assert!(
            select_python_release(
                &valid,
                "demo",
                "opaque-build",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn integration_request_identity_accepts_normalized_name_and_equivalent_version() {
        let (_root, context) = fixture_context().await;
        let mut value = package_metadata(vec![artifact("Demo_Pkg-1.0.post1-py3-none-any.whl")]);
        value.name = "demo-pkg".into();
        value.version = "1.0.post1".into();
        assert!(matches!(
            select_python_release(
                &value,
                "Demo.Pkg",
                "1.0-1",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .unwrap(),
            Some(PythonReleaseCandidate::Wheel { .. })
        ));
    }
    #[tokio::test]
    async fn integration_rank_and_numeric_build_win_before_lexical_filename() {
        let (_root, context) = fixture_context().await;
        let files = vec![
            artifact("demo-1.0-py3-none-any.whl"),
            artifact("demo-1.0-2-cp310-cp310-manylinux_2_17_aarch64.whl"),
            artifact("demo-1.0-10-cp310-cp310-manylinux_2_17_aarch64.whl"),
            artifact("demo-1.0-cp310-cp310-manylinux_2_28_aarch64.whl"),
            artifact("demo-1.0-cp310-cp310-manylinux_2_17_x86_64.whl"),
        ];
        for files in [files.clone(), files.into_iter().rev().collect()] {
            let value = package_metadata(files);
            let Some(PythonReleaseCandidate::Wheel { artifact, .. }) = select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install,
            )
            .unwrap() else {
                panic!("expected wheel")
            };
            assert_eq!(
                artifact.filename,
                "demo-1.0-10-cp310-cp310-manylinux_2_17_aarch64.whl"
            );
        }
    }
    #[tokio::test]
    async fn integration_winning_sha256_failure_never_downgrades_or_builds() {
        let (_root, context) = fixture_context().await;
        for digest in [
            None,
            Some("sha256-no".into()),
            Some(format!("sha512-{}", "a".repeat(128))),
        ] {
            let mut winner = artifact("demo-1.0-10-cp310-cp310-manylinux_2_17_aarch64.whl");
            winner.digest = digest;
            let value = package_metadata(vec![
                artifact("demo-1.0-2-cp310-cp310-manylinux_2_17_aarch64.whl"),
                artifact("demo-1.0-py3-none-any.whl"),
                artifact("demo-1.0.tar.gz"),
                winner,
            ]);
            for policy in [
                PythonCandidatePolicy::Install,
                PythonCandidatePolicy::WheelQuery,
            ] {
                let error =
                    select_python_release(&value, "demo", "1.0", &context, &ranked_tags(), policy)
                        .unwrap_err();
                assert!(error.to_string().contains("SHA256"), "{error}");
            }
        }
    }
    #[tokio::test]
    async fn integration_valid_none_uses_explicit_legacy_policy() {
        let (_root, context) = fixture_context().await;
        let value = package_metadata(vec![
            artifact("demo-1.0-cp311-cp311-manylinux_2_17_aarch64.whl"),
            artifact("demo-1.0.tar.gz"),
        ]);
        assert!(matches!(
            select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .unwrap(),
            Some(PythonReleaseCandidate::LegacySdist { .. })
        ));
        assert!(
            select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::WheelQuery
            )
            .unwrap()
            .is_none()
        );
        let mut missing = value;
        missing.artifacts[1].digest = None;
        assert!(
            select_python_release(
                &missing,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
        assert!(
            select_python_release(
                &missing,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::WheelQuery
            )
            .unwrap()
            .is_none()
        );
    }
    #[tokio::test]
    async fn integration_ambiguity_and_invalid_later_wheel_are_errors() {
        let (_root, context) = fixture_context().await;
        let a = artifact("demo-1.0-cp310-cp310-manylinux_2_17_aarch64.whl");
        let mut b = a.clone();
        b.url = "http://fixture/other".into();
        let ambiguous = package_metadata(vec![a.clone(), b, artifact("demo-1.0.tar.gz")]);
        assert!(
            select_python_release(
                &ambiguous,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
        let invalid = package_metadata(vec![
            a,
            artifact("other-1.0-cp311-cp311-any.whl"),
            artifact("demo-1.0.tar.gz"),
        ]);
        assert!(
            select_python_release(
                &invalid,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn integration_info_requires_python_mismatch_is_none_and_invalid_is_error() {
        let (_root, context) = fixture_context().await;
        let mut value = package_metadata(vec![artifact("demo-1.0-py3-none-any.whl")]);
        value.requires_python = Some("!=3.10.14".into());
        assert!(
            select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .unwrap()
            .is_none()
        );
        value.requires_python = Some("this is not a specifier".into());
        assert!(
            select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn integration_legacy_source_filename_is_a_safe_single_component() {
        let (_root, context) = fixture_context().await;
        for filename in [
            "",
            ".",
            "..",
            "../demo.tar.gz",
            "/demo.tar.gz",
            "dir/demo.tar.gz",
            "dir\\demo.tar.gz",
            "demo\n.tar.gz",
        ] {
            let mut file = artifact("demo-1.0.tar.gz");
            file.filename = filename.into();
            let value = package_metadata(vec![file]);
            assert!(
                select_python_release(
                    &value,
                    "demo",
                    "1.0",
                    &context,
                    &ranked_tags(),
                    PythonCandidatePolicy::Install
                )
                .is_err(),
                "{filename:?}"
            );
        }
    }
    #[tokio::test]
    async fn integration_no_compatible_wheel_or_source_is_plain_none() {
        let (_root, context) = fixture_context().await;
        let value = package_metadata(vec![artifact(
            "demo-1.0-cp311-cp311-manylinux_2_17_aarch64.whl",
        )]);
        assert!(
            select_python_release(
                &value,
                "demo",
                "1.0",
                &context,
                &ranked_tags(),
                PythonCandidatePolicy::Install
            )
            .unwrap()
            .is_none()
        );
    }
    fn release_response(
        name: &str,
        version: &str,
        files: &[(&str, Option<&str>)],
        deps: &[&str],
        requires: &str,
        base: &str,
    ) -> Value {
        let urls: Vec<Value> = files
            .iter()
            .map(|(filename, digest)| {
                let mut value = json!({"filename":filename,
            "url":format!("{base}/{filename}"), "requires_python":">=3.0",
            "packagetype":if filename.ends_with(".whl") {"bdist_wheel"} else {"sdist"}});
                if let Some(digest) = digest {
                    value["digests"] = json!({"sha256":digest});
                }
                value
            })
            .collect();
        json!({"info":{"name":name,"version":version,"requires_python":requires,
                   "requires_dist":deps}, "urls":urls})
    }
    fn release_listing(name: &str, versions: &[&str]) -> Value {
        let releases: serde_json::Map<String, Value> = versions
            .iter()
            .map(|version| {
                (
                    version.to_string(),
                    json!([{"filename":format!("{name}-{version}-py3-none-any.whl"),
            "url":format!("http://fixture/{name}-{version}-py3-none-any.whl"),
            "digests":{"sha256":"a".repeat(64)}, "packagetype":"bdist_wheel",
            "requires_python":">=3.0"}]),
                )
            })
            .collect();
        json!({"info":{"name":name,"version":versions.first().copied().unwrap_or("1.0")},
           "releases":releases})
    }
    async fn mock_value(
        server: &mut mockito::ServerGuard,
        path: &str,
        value: Value,
    ) -> mockito::Mock {
        server
            .mock("GET", path)
            .with_body(value.to_string())
            .expect(1)
            .create_async()
            .await
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_target_candidate_filter_and_dependencies_agree() {
        let (root, context) = fixture_context().await;
        let mut server = mockito::Server::new_async().await;
        let list = mock_value(
            &mut server,
            "/demo/json",
            release_listing("demo", &["1.0", "2.0", "3.0"]),
        )
        .await;
        let hash = "a".repeat(64);
        let base = server.url();
        let one = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response(
                "demo",
                "1.0",
                &[("demo-1.0-py3-none-any.whl", Some(&hash))],
                &["dep==1.0"],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let two = mock_value(
            &mut server,
            "/demo/2.0/json",
            release_response(
                "demo",
                "2.0",
                &[(
                    "demo-2.0-cp311-cp311-manylinux_2_17_aarch64.whl",
                    Some(&hash),
                )],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let three = mock_value(
            &mut server,
            "/demo/3.0/json",
            release_response(
                "demo",
                "3.0",
                &[("demo-3.0.tar.gz", Some(&hash))],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let source = PythonSource {
            index: Arc::new(Index::new(root.path().join("cache")).with_base_url(server.url())),
            context,
            supported: ranked_tags(),
        };
        let versions = source.versions("demo").unwrap();
        assert!(versions.contains(&"1.0".into()) && versions.contains(&"3.0".into()));
        assert!(!versions.contains(&"2.0".into()));
        assert_eq!(source.dependencies("demo", "1.0").unwrap()[0].name, "dep");
        assert!(
            source
                .dependencies("demo", "2.0")
                .unwrap_err()
                .contains("no release compatible")
        );
        assert!(source.dependencies("demo", "3.0").unwrap().is_empty());
        for mock in [list, one, two, three] {
            mock.assert_async().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_valid_info_mismatch_skips_but_winning_hash_error_propagates() {
        for bad_hash in [false, true] {
            let (root, context) = fixture_context().await;
            let mut server = mockito::Server::new_async().await;
            let list = mock_value(
                &mut server,
                "/demo/json",
                release_listing("demo", &["1.0", "2.0"]),
            )
            .await;
            let hash = "a".repeat(64);
            let base = server.url();
            let one = mock_value(
                &mut server,
                "/demo/1.0/json",
                release_response(
                    "demo",
                    "1.0",
                    &[("demo-1.0-py3-none-any.whl", Some(&hash))],
                    &[],
                    ">=3.0",
                    &base,
                ),
            )
            .await;
            let two = mock_value(
                &mut server,
                "/demo/2.0/json",
                release_response(
                    "demo",
                    "2.0",
                    &[(
                        "demo-2.0-py3-none-any.whl",
                        if bad_hash { None } else { Some(&hash) },
                    )],
                    &[],
                    if bad_hash { ">=3.0" } else { "!=3.10.14" },
                    &base,
                ),
            )
            .await;
            let source = PythonSource {
                index: Arc::new(Index::new(root.path().join("cache")).with_base_url(base)),
                context,
                supported: ranked_tags(),
            };
            if bad_hash {
                assert!(source.versions("demo").unwrap_err().contains("SHA256"));
            } else {
                assert_eq!(source.versions("demo").unwrap(), ["1.0"]);
            }
            for mock in [list, one, two] {
                mock.assert_async().await;
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_preferred_pin_rechecks_semantic_listing_and_target() {
        let (root, context) = fixture_context().await;
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let hash = "a".repeat(64);
        let list = mock_value(&mut server, "/demo/json", release_listing("demo", &["1.0"])).await;
        let exact = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response(
                "demo",
                "1.0",
                &[(
                    "demo-1.0-cp311-cp311-manylinux_2_17_aarch64.whl",
                    Some(&hash),
                )],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let source = PythonSource {
            index: Arc::new(Index::new(root.path().join("cache")).with_base_url(base)),
            context,
            supported: ranked_tags(),
        };
        let bias = ResolvedGraph {
            nodes: vec![ResolvedNode {
                eco: "python".into(),
                name: "demo".into(),
                version: "1.0".into(),
                digest: Some(format!("sha256-{}", "f".repeat(64))),
                deps: vec![],
            }],
        };
        let requests = [DependencyRequest {
            eco: "python".into(),
            name: "demo".into(),
            constraint: "1.0".into(),
        }];
        assert!(pakro_resolver::solve(&requests, &bias, &source, "python").is_err());
        list.assert_async().await;
        exact.assert_async().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn latest_skips_incompatible_and_legacy_source_versions_but_never_installs() {
        let root = tempfile::tempdir().unwrap();
        let binding = PackageRuntimeBinding {
            tool: tool(root.path(), 14, false),
            locked: false,
        };
        let runtime = PythonPackageRuntime::from_tool_version(&binding.tool)
            .await
            .unwrap();
        assert!(
            runtime
                .supported
                .best_rank(
                    &pakro_index::parse_wheel_filename("demo-1.0-py3-none-any.whl")
                        .unwrap()
                        .tags
                )
                .unwrap()
                .is_some()
        );
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let hash = "a".repeat(64);
        let list = mock_value(
            &mut server,
            "/demo/json",
            release_listing("demo", &["1.0", "2.0", "3.0"]),
        )
        .await;
        let three = mock_value(
            &mut server,
            "/demo/3.0/json",
            release_response(
                "demo",
                "3.0",
                &[(
                    "demo-3.0-cp311-cp311-manylinux_2_17_x86_64.whl",
                    Some(&hash),
                )],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let two = mock_value(
            &mut server,
            "/demo/2.0/json",
            release_response(
                "demo",
                "2.0",
                &[("demo-2.0.tar.gz", None)],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let one = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response(
                "demo",
                "1.0",
                &[("demo-1.0-py3-none-any.whl", Some(&hash))],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let index = Arc::new(Index::new(root.path().join("cache")).with_base_url(base));
        assert!(
            matches!(latest_python_package(index, "demo", "pbs-build-opaque", &[binding], false)
        .await.unwrap(), PythonPackageLatest::Available(Some(v)) if v=="1.0")
        );
        assert!(!root.path().join("store").exists());
        assert!(!root.path().join(".pakro/venvs").exists());
        for mock in [list, three, two, one] {
            mock.assert_async().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn latest_winning_hash_error_does_not_return_a_lower_release() {
        let root = tempfile::tempdir().unwrap();
        let binding = PackageRuntimeBinding {
            tool: tool(root.path(), 14, false),
            locked: false,
        };
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let list = mock_value(
            &mut server,
            "/demo/json",
            release_listing("demo", &["1.0", "2.0"]),
        )
        .await;
        let two = mock_value(
            &mut server,
            "/demo/2.0/json",
            release_response(
                "demo",
                "2.0",
                &[("demo-2.0-py3-none-any.whl", None)],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let one = server
            .mock("GET", "/demo/1.0/json")
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(root.path().join("cache")).with_base_url(base));
        let error =
            latest_python_package(index.clone(), "demo", "pbs-build-opaque", &[binding], false)
                .await
                .err()
                .unwrap();
        assert!(error.to_string().contains("SHA256"), "{error}");
        let no_http = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        assert!(
            matches!(latest_python_package(index,"demo","missing",&[],true).await.unwrap(),
        PythonPackageLatest::Unavailable(reason) if reason.contains("H3"))
        );
        for mock in [list, two, one, no_http] {
            mock.assert_async().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_install_rank_build_and_observed_store_platform() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, false);
        let runtime = PythonPackageRuntime::from_tool_version(&actual)
            .await
            .unwrap();
        let expected_platform: Platform = "linux-aarch64-gnu".parse().unwrap();
        assert_eq!(runtime.observed_platform, Some(expected_platform));
        let locs = locations(root.path());
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let payload = wheel("demo");
        let digest = Digest::from_sha256_bytes(&payload);
        let hash = digest.hex();
        let files = [
            ("demo-1.0-999-py3-none-any.whl", Some(hash.as_str())),
            (
                "demo-1.0-2-cp310-cp310-linux_aarch64.whl",
                Some(hash.as_str()),
            ),
            (
                "demo-1.0-10-cp310-cp310-linux_aarch64.whl",
                Some(hash.as_str()),
            ),
            (
                "demo-1.0-cp310-cp310-manylinux_2_28_aarch64.whl",
                Some(hash.as_str()),
            ),
            (
                "demo-1.0-cp310-cp310-manylinux_2_17_x86_64.whl",
                Some(hash.as_str()),
            ),
        ];
        let list = mock_value(&mut server, "/demo/json", release_listing("demo", &["1.0"])).await;
        let exact = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response("demo", "1.0", &files, &[], ">=3.0", &base),
        )
        .await;
        let winner = server
            .mock("GET", "/demo-1.0-10-cp310-cp310-linux_aarch64.whl")
            .with_body(payload)
            .expect(1)
            .create_async()
            .await;
        let losers = server
            .mock(
                "GET",
                mockito::Matcher::Regex(
                    r"/demo-1\.0-(999-py3|2-cp310|cp310-cp310-manylinux).*\.whl".into(),
                ),
            )
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(base));
        let requests = inputs(actual);
        assert_eq!(
            install_packages(&requests, &locs, index.clone())
                .await
                .unwrap(),
            1
        );
        let lock = PackageLock::read(&root.path().join(LOCKFILE_NAME))
            .unwrap()
            .unwrap();
        let graph = lock.graph_for("python@pbs-build-opaque").unwrap();
        assert_eq!(graph.nodes[0].digest, Some(digest.to_string()));
        let identity = pakro_store::PackageIdentity {
            eco: pakro_store::Eco::Python,
            name: "demo".into(),
            version: "1.0".into(),
            runtime: Some(pakro_store::RuntimeId {
                eco: pakro_store::Eco::Python,
                version: "pbs-build-opaque".into(),
            }),
            platform: expected_platform,
            digest,
        };
        let store = pakro_store::Store::open(&locs.store_root).unwrap();
        let stored: pakro_store::PackageIdentity = serde_json::from_slice(
            &std::fs::read(store.entry_path(&identity).join(".pakro-meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored.platform, expected_platform);
        assert_eq!(install_packages(&requests, &locs, index).await.unwrap(), 0);
        for mock in [list, exact, winner, losers] {
            mock.assert_async().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_install_candidate_errors_precede_all_artifact_mutations() {
        for case in [
            "missing_hash",
            "malformed_hash",
            "foreign_name",
            "foreign_version",
            "ambiguity",
            "invalid_wheel",
            "kind_mismatch",
        ] {
            let root = tempfile::tempdir().unwrap();
            let actual = tool(root.path(), 14, false);
            let locs = locations(root.path());
            let before = format!(
                "[groups]\n'python@pbs-build-opaque' = [{{name='demo',version='1.0',digest='sha256-{}'}}]\n",
                "f".repeat(64)
            );
            let lock = root.path().join(LOCKFILE_NAME);
            std::fs::write(&lock, &before).unwrap();
            let mut server = mockito::Server::new_async().await;
            let base = server.url();
            let hash = "a".repeat(64);
            let mut value = release_response(
                "demo",
                "1.0",
                &[
                    ("demo-1.0-10-cp310-cp310-linux_aarch64.whl", Some(&hash)),
                    ("demo-1.0-2-cp310-cp310-linux_aarch64.whl", Some(&hash)),
                    ("demo-1.0-py3-none-any.whl", Some(&hash)),
                    ("demo-1.0.tar.gz", Some(&hash)),
                ],
                &[],
                ">=3.0",
                &base,
            );
            match case {
                "missing_hash" => value["urls"][0].as_object_mut().unwrap().remove("digests"),
                "malformed_hash" => {
                    value["urls"][0]["digests"]["sha256"] = json!("wrong");
                    None
                }
                "foreign_name" => {
                    value["info"]["name"] = json!("other");
                    None
                }
                "foreign_version" => {
                    value["info"]["version"] = json!("2.0");
                    None
                }
                "ambiguity" => {
                    let mut duplicate = value["urls"][0].clone();
                    duplicate["url"] = json!(format!("{base}/different.whl"));
                    value["urls"].as_array_mut().unwrap().push(duplicate);
                    None
                }
                "invalid_wheel" => {
                    value["urls"][0]["filename"] = json!("broken.whl");
                    None
                }
                "kind_mismatch" => {
                    value["urls"][0]["packagetype"] = json!("sdist");
                    None
                }
                _ => unreachable!(),
            };
            let list =
                mock_value(&mut server, "/demo/json", release_listing("demo", &["1.0"])).await;
            let exact = mock_value(&mut server, "/demo/1.0/json", value).await;
            let artifacts = server
                .mock("GET", mockito::Matcher::Regex(r".*\.(whl|tar\.gz)$".into()))
                .expect(0)
                .create_async()
                .await;
            let index = Arc::new(Index::new(&locs.index_cache).with_base_url(base));
            let error = install_packages(&inputs(actual), &locs, index)
                .await
                .unwrap_err();
            if matches!(case, "missing_hash" | "malformed_hash") {
                assert!(error.to_string().contains("SHA256"), "{case}: {error}");
            }
            assert!(!locs.store_root.exists(), "{case}");
            assert!(!root.path().join(".pakro/venvs").exists(), "{case}");
            assert_eq!(std::fs::read_to_string(lock).unwrap(), before, "{case}");
            for mock in [list, exact, artifacts] {
                mock.assert_async().await;
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_all_nodes_preflight_precedes_first_source_download() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, false);
        let runtime = PythonPackageRuntime::from_tool_version(&actual)
            .await
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let hash = "a".repeat(64);
        let one = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response(
                "demo",
                "1.0",
                &[("demo-1.0.tar.gz", Some(&hash))],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let two = mock_value(
            &mut server,
            "/bad/1.0/json",
            release_response(
                "other",
                "1.0",
                &[("other-1.0-py3-none-any.whl", Some(&hash))],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let artifacts = server
            .mock("GET", mockito::Matcher::Regex(r".*\.(whl|tar\.gz)$".into()))
            .expect(0)
            .create_async()
            .await;
        let index = Index::new(root.path().join("cache")).with_base_url(base);
        let nodes: Vec<ResolvedNode> = ["demo", "bad"]
            .into_iter()
            .map(|name| ResolvedNode {
                eco: "python".into(),
                name: name.into(),
                version: "1.0".into(),
                digest: Some(format!("sha256-{}", "f".repeat(64))),
                deps: vec![],
            })
            .collect();
        let error = crate::packages::install::prepare_python_artifacts(&nodes, &runtime, &index)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("name"), "{error}");
        assert!(!root.path().join("store").exists());
        assert!(!root.path().join(".pakro/venvs").exists());
        for mock in [one, two, artifacts] {
            mock.assert_async().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_warm_store_and_old_digest_do_not_authorize_incompatible_target() {
        let root = tempfile::tempdir().unwrap();
        let actual = tool(root.path(), 14, false);
        let locs = locations(root.path());
        let payload = wheel("demo");
        let digest = Digest::from_sha256_bytes(&payload);
        let identity = pakro_store::PackageIdentity {
            eco: pakro_store::Eco::Python,
            name: "demo".into(),
            version: "1.0".into(),
            runtime: Some(pakro_store::RuntimeId {
                eco: pakro_store::Eco::Python,
                version: "pbs-build-opaque".into(),
            }),
            platform: "linux-aarch64-gnu".parse().unwrap(),
            digest,
        };
        let store = pakro_store::Store::open(&locs.store_root).unwrap();
        store.unpack(&identity, &payload).unwrap();
        let meta_path = store.entry_path(&identity).join(".pakro-meta.json");
        let before_meta = std::fs::read(&meta_path).unwrap();
        let before = format!(
            "[groups]\n'python@pbs-build-opaque' = [{{name='demo',version='1.0',digest='{digest}'}}]\n"
        );
        let lock = root.path().join(LOCKFILE_NAME);
        std::fs::write(&lock, &before).unwrap();
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let hash = digest.hex();
        let list = mock_value(&mut server, "/demo/json", release_listing("demo", &["1.0"])).await;
        let exact = mock_value(
            &mut server,
            "/demo/1.0/json",
            release_response(
                "demo",
                "1.0",
                &[(
                    "demo-1.0-cp310-cp310-manylinux_2_28_aarch64.whl",
                    Some(&hash),
                )],
                &[],
                ">=3.0",
                &base,
            ),
        )
        .await;
        let bytes = server
            .mock("GET", mockito::Matcher::Regex(r".*\.whl$".into()))
            .expect(0)
            .create_async()
            .await;
        let index = Arc::new(Index::new(&locs.index_cache).with_base_url(base));
        assert!(
            install_packages(&inputs(actual), &locs, index)
                .await
                .is_err()
        );
        assert!(!root.path().join(".pakro/venvs").exists());
        assert_eq!(std::fs::read_to_string(lock).unwrap(), before);
        assert_eq!(std::fs::read(meta_path).unwrap(), before_meta);
        assert!(store.contains(&identity));
        for mock in [list, exact, bytes] {
            mock.assert_async().await;
        }
    }
    #[tokio::test]
    async fn integration_missing_observed_platform_cannot_be_replaced_by_compiled_host() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = PythonPackageRuntime::from_tool_version(&tool(root.path(), 14, false))
            .await
            .unwrap();
        runtime.observed_platform = None;
        assert!(
            python_install_platform(&runtime)
                .unwrap_err()
                .to_string()
                .contains("observed")
        );
    }
}
