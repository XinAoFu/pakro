//! Package registry metadata for mise. The `Index` client fetches and
//! caches registry data (PyPI and npm); parsing and platform-tag matching
//! are pure logic in ecological modules (`pypi`, `npm`, `wheel`).

pub mod artifact;
pub mod cache;
pub mod npm;
pub mod pypi;
pub mod python_context;
pub mod types;
pub mod wheel;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub use npm::{parse_npm_version_metadata, parse_npm_versions};
pub use pypi::{
    parse_pypi_response_for_context, parse_pypi_versions, parse_pypi_versions_for_context,
    python_requires_matches_for_context, rewrite_files_host,
};
pub use python_context::{PythonMarkerContext, PythonMarkerContextBuilder};
pub use types::{
    ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata, normalize_py_name,
};
pub use wheel::{
    RuntimeInfo, WheelBuildTag, WheelFile, WheelTags, parse_wheel_filename, wheel_matches,
};

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
    pypi_files_url: Option<String>,
}

impl Index {
    pub fn new(cache_root: impl AsRef<Path>) -> Self {
        Self {
            cache: cache::MetadataCache::new(cache_root),
            client: reqwest::Client::new(),
            pypi_url: DEFAULT_PYPI_URL.to_string(),
            npm_registry: DEFAULT_NPM_REGISTRY.to_string(),
            pypi_files_url: None,
        }
    }

    /// [`Index::new`] with registry endpoints overridden by the
    /// `MISE_PYPI_URL` / `MISE_NPM_REGISTRY` / `MISE_PYPI_FILES_URL`
    /// environment variables — the configurable mirrors FR-11 promises.
    /// Registry JSON from mirror sites still points artifact downloads at
    /// `files.pythonhosted.org`, so mirrors need `MISE_PYPI_FILES_URL` to
    /// actually reroute the bytes.
    pub fn from_env(cache_root: impl AsRef<Path>) -> Self {
        let mut index = Self::new(cache_root);
        if let Ok(url) = std::env::var("MISE_PYPI_URL") {
            index = index.with_base_url(url);
        }
        if let Ok(url) = std::env::var("MISE_NPM_REGISTRY") {
            index = index.with_npm_registry(url);
        }
        if let Ok(url) = std::env::var("MISE_PYPI_FILES_URL") {
            index = index.with_pypi_files_url(url);
        }
        index
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.pypi_url = url.into();
        self
    }

    pub fn with_npm_registry(mut self, url: impl Into<String>) -> Self {
        self.npm_registry = url.into();
        self
    }

    /// Base that replaces `https://files.pythonhosted.org` in artifact URLs,
    /// for mirrors that proxy the same `/packages/...` paths.
    pub fn with_pypi_files_url(mut self, url: impl Into<String>) -> Self {
        self.pypi_files_url = Some(url.into());
        self
    }

    // -- python (PyPI) --

    /// Evaluate dependencies afresh against the caller's explicit marker fields.
    /// Invalid info or file Requires-Python is an error; valid nonmatching files
    /// are removed. Valid info constraints remain for the caller's install check.
    /// The cache stores raw JSON. A cache miss may fetch online; this method
    /// provides no offline guarantee or proof of interpreter/ABI compatibility.
    pub async fn metadata_for_context(
        &self,
        name: &str,
        version: &str,
        context: &PythonMarkerContext,
    ) -> Result<PackageMetadata> {
        let value = self.fetch_python_metadata(name, version).await?;
        let metadata = parse_pypi_response_for_context(&value, context)?;
        Ok(self.rewrite_python_artifacts(metadata))
    }

    /// Raw release names with files, without target-context compatibility checks.
    /// A cache miss may fetch online; this method provides no offline guarantee.
    pub async fn versions(&self, name: &str) -> Result<Vec<String>> {
        let value = self.fetch_python_version_listing(name).await?;
        parse_pypi_versions(&value)
    }

    /// Releases with at least one file admitting the full explicit language version.
    /// Invalid file Requires-Python constraints are errors. This is not a wheel
    /// ABI/installability check. The cache stores raw JSON and is evaluated for
    /// each context; a cache miss may fetch online with no offline guarantee.
    pub async fn versions_for_context(
        &self,
        name: &str,
        context: &PythonMarkerContext,
    ) -> Result<Vec<String>> {
        let value = self.fetch_python_version_listing(name).await?;
        parse_pypi_versions_for_context(&value, context)
    }

    async fn fetch_python_metadata(&self, name: &str, version: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/{}/{version}/json",
            self.pypi_url,
            normalize_py_name(name)
        );
        self.fetch_cached("python", name, version, &url).await
    }

    async fn fetch_python_version_listing(&self, name: &str) -> Result<serde_json::Value> {
        let url = format!("{}/{}/json", self.pypi_url, normalize_py_name(name));
        self.fetch_cached("python", name, "__versions__", &url)
            .await
    }

    fn rewrite_python_artifacts(&self, mut metadata: PackageMetadata) -> PackageMetadata {
        if let Some(base) = &self.pypi_files_url {
            for artifact in &mut metadata.artifacts {
                artifact.url = rewrite_files_host(&artifact.url, base);
            }
        }
        metadata
    }

    /// Stable release candidates whose files admit the full explicit language
    /// version, in descending PEP 440 order. Returned names remain unchanged.
    /// Invalid file Requires-Python constraints are errors; invalid release
    /// names are excluded. This does not establish wheel ABI compatibility or
    /// installability. Cached raw JSON is evaluated for each context; a cache
    /// miss may fetch online, with no offline guarantee.
    pub async fn stable_python_versions_for_context(
        &self,
        name: &str,
        context: &PythonMarkerContext,
    ) -> Result<Vec<String>> {
        let versions = self.versions_for_context(name, context).await?;
        Ok(stable_python_release_names(versions))
    }

    /// First stable candidate for the explicit target context, without a
    /// raw-listing fallback. Empty candidate sets return None; query and
    /// constraint errors propagate. This is not a wheel/installability check,
    /// and a cache miss may fetch online with no offline guarantee.
    pub async fn latest_stable_python_for_context(
        &self,
        name: &str,
        context: &PythonMarkerContext,
    ) -> Result<Option<String>> {
        Ok(self
            .stable_python_versions_for_context(name, context)
            .await?
            .into_iter()
            .next())
    }

    /// Newest stable raw Python release by PEP 440, preserving its original name.
    /// This excludes pre/dev releases and includes post releases, but performs
    /// no target Requires-Python or wheel compatibility check. A cache miss
    /// may fetch online; this method provides no offline guarantee.
    pub async fn latest_stable_python(&self, name: &str) -> Result<Option<String>> {
        let versions = self.versions(name).await?;
        Ok(stable_python_release_names(versions).into_iter().next())
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
        // cache scope includes the registry host: metadata from a mirror
        // carries the mirror's download URLs, so it must not leak back when
        // the endpoint is switched
        let host = url
            .split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .next()
            .unwrap_or("unknown");
        let scope = format!("{eco}__{host}");
        if let Some(bytes) = self.cache.read_with_ttl(&scope, name, key, TTL)? {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let response = self.client.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        self.cache.write(&scope, name, key, &bytes)?;
        Ok(value)
    }
}

fn stable_python_release_names(versions: Vec<String>) -> Vec<String> {
    let mut parsed = versions
        .into_iter()
        .filter_map(|raw| {
            let version = pep440_rs::Version::from_str(&raw).ok()?;
            version.is_stable().then_some((version, raw))
        })
        .collect::<Vec<_>>();
    parsed.sort_by(|(a, a_raw), (b, b_raw)| b.cmp(a).then_with(|| b_raw.cmp(a_raw)));
    parsed.into_iter().map(|(_, raw)| raw).collect()
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
    #[error("invalid Python marker context: {0}")]
    InvalidPythonContext(String),
    #[error("invalid Requires-Python {specifier:?}: {reason}")]
    InvalidPythonSpecifier { specifier: String, reason: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IndexError>;
