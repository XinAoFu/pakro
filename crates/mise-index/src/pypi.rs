//! PyPI JSON API client and response parsing. The network fetch itself is a
//! thin layer over the cache; everything downstream of `parse_pypi_response`
//! is offline-testable.

use std::path::Path;
use std::time::Duration;

use pep508_rs::{MarkerEnvironment, MarkerEnvironmentBuilder, Requirement};
use serde_json::Value;

use crate::cache::MetadataCache;
use crate::error::{IndexError, Result};
use crate::types::{
    ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata, normalize_py_name,
};
use crate::wheel::{RuntimeInfo, parse_wheel_filename};

const DEFAULT_BASE_URL: &str = "https://pypi.org/pypi";
const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60 * 24);

pub struct Index {
    cache: MetadataCache,
    client: reqwest::Client,
    base_url: String,
}

impl Index {
    pub fn new(cache_root: impl AsRef<Path>) -> Self {
        Self {
            cache: MetadataCache::new(cache_root),
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub async fn metadata(
        &self,
        name: &str,
        version: &str,
        runtime: &RuntimeInfo,
    ) -> Result<PackageMetadata> {
        if let Some(bytes) = self
            .cache
            .read_with_ttl("python", name, version, DEFAULT_TTL)?
        {
            let value: Value = serde_json::from_slice(&bytes)?;
            return parse_pypi_response(&value, runtime);
        }
        let url = format!(
            "{}/{}/{version}/json",
            self.base_url,
            normalize_py_name(name)
        );
        let response = self.client.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        self.cache.write("python", name, version, &bytes)?;
        parse_pypi_response(&value, runtime)
    }
}

/// Parse a PyPI `/pypi/{name}/{version}/json` response into metadata whose
/// dependency list is already filtered to `runtime` via PEP 508 markers.
pub fn parse_pypi_response(value: &Value, runtime: &RuntimeInfo) -> Result<PackageMetadata> {
    let bad = |reason: &str| IndexError::BadResponse {
        package: value
            .pointer("/info/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        reason: reason.to_string(),
    };
    let info = value.get("info").ok_or_else(|| bad("missing info"))?;
    let name = info
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing name"))?;
    let version = info
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing version"))?;
    let requires_python = info
        .get("requires_python")
        .and_then(Value::as_str)
        .map(str::to_string);
    let env = marker_environment(runtime).map_err(|e| bad(&e.to_string()))?;
    let mut deps = vec![];
    if let Some(dist) = info.get("requires_dist").and_then(Value::as_array) {
        for entry in dist {
            let Some(req_str) = entry.as_str() else {
                continue;
            };
            let req = Requirement::parse(req_str, std::path::Path::new(""))
                .map_err(|e| IndexError::BadRequirement(req_str.to_string(), e))?;
            // extras-gated and marker-inapplicable requirements are dropped;
            // M1 installs no extras
            if !req.evaluate_markers(&env, &[]) {
                continue;
            }
            let constraint = match &req.version_or_url {
                Some(pep508_rs::VersionOrUrl::VersionSpecifier(specifiers)) => {
                    specifiers.to_string()
                }
                Some(pep508_rs::VersionOrUrl::Url(_)) => {
                    return Err(bad(&format!(
                        "dependency {req_str} is URL-based, not supported in M1"
                    )));
                }
                None => String::new(),
            };
            deps.push(DependencyConstraint {
                name: normalize_py_name(req.name.as_ref()),
                constraint,
            });
        }
    }
    let mut artifacts = vec![];
    if let Some(urls) = value.get("urls").and_then(Value::as_array) {
        for url_entry in urls {
            let filename = url_entry.get("filename").and_then(Value::as_str);
            let url = url_entry.get("url").and_then(Value::as_str);
            let (Some(filename), Some(url)) = (filename, url) else {
                continue;
            };
            let kind = if filename.ends_with(".whl") {
                match parse_wheel_filename(filename) {
                    Some(wheel) => ArtifactKind::Wheel(wheel.tags),
                    None => continue,
                }
            } else {
                ArtifactKind::Sdist
            };
            artifacts.push(ArtifactDescriptor {
                filename: filename.to_string(),
                url: url.to_string(),
                sha256: url_entry
                    .pointer("/digests/sha256")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                kind,
            });
        }
    }
    if artifacts.is_empty() {
        return Err(bad("no downloadable artifacts"));
    }
    Ok(PackageMetadata {
        name: normalize_py_name(name),
        version: version.to_string(),
        requires_python,
        deps,
        artifacts,
    })
}

fn marker_environment(
    runtime: &RuntimeInfo,
) -> std::result::Result<MarkerEnvironment, pep440_rs::VersionParseError> {
    let py_version = format!("{}.{}", runtime.major, runtime.minor);
    let platform_system = if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    };
    MarkerEnvironment::try_from(MarkerEnvironmentBuilder {
        implementation_name: "cpython",
        implementation_version: &py_version,
        os_name: "posix",
        platform_machine: if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x86_64"
        },
        platform_python_implementation: "CPython",
        platform_release: "",
        platform_system,
        platform_version: "",
        python_full_version: &py_version,
        python_version: &py_version,
        sys_platform: if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
    })
}
