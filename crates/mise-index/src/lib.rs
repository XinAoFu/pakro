//! Package registry metadata for mise. The `Index` client fetches and
//! caches registry data (PyPI and npm); parsing and platform-tag matching
//! are pure logic in ecological modules (`pypi`, `npm`, `wheel`).

pub mod cache;
pub mod npm;
pub mod pypi;
pub mod types;
pub mod wheel;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub use npm::{parse_npm_version_metadata, parse_npm_versions};
pub use pypi::{parse_pypi_response, parse_pypi_versions, python_requires_matches};
pub use types::{
    ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata, normalize_py_name,
};
pub use wheel::{RuntimeInfo, WheelFile, WheelTags, parse_wheel_filename, wheel_matches};

// ---------------------------------------------------------------------------
// Index client
// ---------------------------------------------------------------------------

const DEFAULT_PYPI_URL: &str = "https://pypi.org/pypi";
const DEFAULT_NPM_REGISTRY: &str = "https://registry.npmjs.org";
const TTL: Duration = Duration::from_secs(60 * 60 * 24);

/// A metadata client for PyPI and npm, with disk caching.
pub struct Index {
    cache: cache::MetadataCache,
    client: reqwest::Client,
    pypi_url: String,
    npm_registry: String,
}

impl Index {
    pub fn new(cache_root: impl AsRef<Path>) -> Self {
        Self {
            cache: cache::MetadataCache::new(cache_root),
            client: reqwest::Client::new(),
            pypi_url: DEFAULT_PYPI_URL.to_string(),
            npm_registry: DEFAULT_NPM_REGISTRY.to_string(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.pypi_url = url.into();
        self
    }

    pub fn with_npm_registry(mut self, url: impl Into<String>) -> Self {
        self.npm_registry = url.into();
        self
    }

    // -- python (PyPI) --

    /// Metadata for one exact version; markers already evaluated for `runtime`.
    pub async fn metadata(
        &self,
        name: &str,
        version: &str,
        runtime: &RuntimeInfo,
    ) -> Result<PackageMetadata> {
        let url = format!(
            "{}/{}/{version}/json",
            self.pypi_url,
            normalize_py_name(name)
        );
        let value = self.fetch_cached("python", name, version, &url).await?;
        parse_pypi_response(&value, runtime)
    }

    /// All released versions of a python package.
    pub async fn versions(&self, name: &str) -> Result<Vec<String>> {
        let url = format!("{}/{}/json", self.pypi_url, normalize_py_name(name));
        let value = self
            .fetch_cached("python", name, "__versions__", &url)
            .await?;
        parse_pypi_versions(&value)
    }

    /// Released versions whose `requires_python` admits the runtime, so the
    /// solver never proposes candidates install-time checks would reject.
    /// Same cache entry as [`Index::versions`]; no extra request.
    pub async fn versions_for_runtime(
        &self,
        name: &str,
        runtime: &RuntimeInfo,
    ) -> Result<Vec<String>> {
        let url = format!("{}/{}/json", self.pypi_url, normalize_py_name(name));
        let value = self
            .fetch_cached("python", name, "__versions__", &url)
            .await?;
        pypi::parse_pypi_versions_for_runtime(&value, runtime)
    }

    /// Newest non-prerelease version, pep440 ordering.
    pub async fn latest_stable_python(&self, name: &str) -> Result<Option<String>> {
        let versions = self.versions(name).await?;
        Ok(latest_stable(versions, |v| {
            pep440_rs::Version::from_str(v).ok()
        }))
    }

    // -- node (npm) --

    /// Metadata for one exact version from the npm registry.
    pub async fn npm_metadata(
        &self,
        name: &str,
        version: &str,
        _runtime: &RuntimeInfo,
    ) -> Result<PackageMetadata> {
        let url = format!("{}/{}/{version}", self.npm_registry, name);
        let value = self.fetch_cached("node", name, version, &url).await?;
        parse_npm_version_metadata(&value)
    }

    /// All released versions of an npm package.
    pub async fn npm_versions(&self, name: &str) -> Result<Vec<String>> {
        let url = format!("{}/{}", self.npm_registry, name);
        let value = self
            .fetch_cached("node", name, "__versions__", &url)
            .await?;
        parse_npm_versions(&value)
    }

    /// Newest non-prerelease version, semver ordering.
    pub async fn latest_stable_npm(&self, name: &str) -> Result<Option<String>> {
        let versions = self.npm_versions(name).await?;
        Ok(latest_stable(versions, |v| {
            node_semver::Version::from_str(v).ok()
        }))
    }

    // -- shared --

    async fn fetch_cached(
        &self,
        eco: &str,
        name: &str,
        key: &str,
        url: &str,
    ) -> Result<serde_json::Value> {
        if let Some(bytes) = self.cache.read_with_ttl(eco, name, key, TTL)? {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let response = self.client.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        self.cache.write(eco, name, key, &bytes)?;
        Ok(value)
    }
}

/// Filters prereleases and picks the newest by the ecosystem's ordering.
fn latest_stable<T: Ord + Clone>(
    versions: Vec<String>,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<String> {
    versions
        .into_iter()
        .filter(|v| !v.split('+').next().unwrap_or_default().contains('-'))
        .filter_map(|v| parse(&v).map(|p| (p, v)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, v)| v)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid registry response for {package}: {reason}")]
    BadResponse { package: String, reason: String },
    #[error("invalid requirement string {0:?}")]
    BadRequirement(String, #[source] pep508_rs::Pep508Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IndexError>;
