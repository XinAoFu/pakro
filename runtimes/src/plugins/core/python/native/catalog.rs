//! Official Python distribution metadata fetched through explicit inputs.
//!
//! All PBS release pages and required same-release checksum manifests must pass
//! validation before a raw cache snapshot is atomically published. Cache identity
//! includes every source endpoint, distribution, target and exact flavor. Offline
//! requests only read existing snapshots, including expired ones, and never send
//! HTTP requests. Locked-artifact replay belongs to the caller and bypasses this
//! catalog entirely. Explicit URL replacements change request locators only;
//! pagination and artifact identities remain canonical. An ordered mapping
//! fingerprint separates cached metadata without storing routing credentials.
//!
//! Verification contract:
//! - Preserve older PBS releases after a stable page; never stop on stability or
//!   silently truncate page/response/aggregate limits.
//! - Restrict pagination to the same endpoint with sequential pages; do not use
//!   shared GitHub mirrors or synthesize a continuation after malformed metadata.
//! - Reuse pure required_pbs_checksums candidate validation; only missing hashes
//!   for the requested target/flavor may request a fixed same-release manifest.
//! - Fetch both PyPy metadata sources and preserve full language/implementation
//!   identities and upstream stability through the pure parser.
//! - Revalidate cached raw data; reject copied identities, malformed snapshots
//!   and missing offline items without a network fallback.
//! - Bound writes and publish using one same-directory temporary file. Failed
//!   fetches or validation leave the previous snapshot untouched.
//! - Read no global Settings/environment and invoke no processes or managers.
//!   Loopback tests explicitly disable reqwest proxies on their local clients.
//! - Keep HTTP/parser errors free of endpoint credentials, queries and bodies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eyre::{Result, ensure};
use pakro_index::artifact::NetworkPolicy;
use pakro_store::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    RuntimeArchiveTarget, RuntimeDistribution, RuntimeFlavor, RuntimePlatform, RuntimeRelease,
    parse_pbs_releases, parse_pypy_releases, required_pbs_checksums,
};

#[derive(Clone)]
pub(in super::super) struct CatalogEndpoints {
    pbs_releases: String,
    pbs_downloads_base: String,
    pypy_versions: String,
    pypy_checksums: String,
}

impl CatalogEndpoints {
    pub(in super::super) fn new(
        pbs_releases: &str,
        pbs_downloads_base: &str,
        pypy_versions: &str,
        pypy_checksums: &str,
    ) -> Result<Self> {
        Ok(Self {
            pbs_releases: validated_endpoint(pbs_releases, false)?,
            pbs_downloads_base: validated_endpoint(pbs_downloads_base, true)?,
            pypy_versions: validated_endpoint(pypy_versions, false)?,
            pypy_checksums: validated_endpoint(pypy_checksums, false)?,
        })
    }

    pub(in super::super) fn official() -> Result<Self> {
        Self::new(
            "https://api.github.com/repositories/162334160/releases",
            "https://github.com/astral-sh/python-build-standalone/releases/download/",
            "https://downloads.python.org/pypy/versions.json",
            "https://pypy.org/checksums.html",
        )
    }
}

/// Explicit failure limits; exceeding one is an error, never partial success.
#[derive(Debug, Clone, Copy)]
pub(in super::super) struct CatalogLimits {
    pub(in super::super) max_pages: usize,
    pub(in super::super) max_response_bytes: usize,
    pub(in super::super) max_total_bytes: usize,
    pub(in super::super) max_cache_bytes: usize,
}

impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            max_pages: 256,
            max_response_bytes: 64 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_cache_bytes: 768 * 1024 * 1024,
        }
    }
}

/// Metadata only: locked-artifact replay must bypass this catalog entirely.
pub(in super::super) struct RuntimeCatalog {
    client: reqwest::Client,
    cache_root: PathBuf,
    endpoints: CatalogEndpoints,
    policy: NetworkPolicy,
    fresh_for: Duration,
    limits: CatalogLimits,
    url_replacements: Option<indexmap::IndexMap<String, String>>,
}

impl RuntimeCatalog {
    pub(in super::super) fn new(
        client: reqwest::Client,
        cache_root: impl AsRef<Path>,
        endpoints: CatalogEndpoints,
        policy: NetworkPolicy,
        fresh_for: Duration,
        limits: CatalogLimits,
    ) -> Result<Self> {
        ensure!(
            limits.max_pages > 0
                && limits.max_response_bytes > 0
                && limits.max_total_bytes > 0
                && limits.max_cache_bytes > 0,
            "Python runtime catalog limits must be positive"
        );
        Ok(Self {
            client,
            cache_root: cache_root.as_ref().to_path_buf(),
            endpoints,
            policy,
            fresh_for,
            limits,
            url_replacements: None,
        })
    }

    pub(in super::super) fn with_url_replacements(
        mut self,
        replacements: Option<&indexmap::IndexMap<String, String>>,
    ) -> Self {
        self.url_replacements = replacements.filter(|mapping| !mapping.is_empty()).cloned();
        self
    }

    pub(in super::super) async fn pbs_releases(
        &self,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<Vec<RuntimeRelease>> {
        let distribution = RuntimeDistribution::Pbs;
        if let Some(releases) = self.cached_releases(distribution, target, flavor)? {
            return Ok(releases);
        }
        let mut consumed = 0;
        let releases = self
            .fetch_pbs_pages(target, flavor, None, &mut consumed)
            .await?;
        let raw = self
            .complete_pbs_metadata(releases, target, flavor, &mut consumed)
            .await?;
        let releases = raw.parse(target, flavor)?;
        self.publish_cache(distribution, target, flavor, raw)?;
        Ok(releases)
    }

    async fn fetch_pbs_pages(
        &self,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
        scope: Option<&ExactPbsScope>,
        consumed: &mut usize,
    ) -> Result<Value> {
        let base = catalog_url(&self.endpoints.pbs_releases)?;
        let mut url = base.clone();
        url.query_pairs_mut()
            .append_pair("per_page", "10")
            .append_pair("page", "1");
        let mut page_number = 1_usize;
        let mut seen = BTreeSet::new();
        let mut builds = BTreeSet::new();
        let mut releases = Vec::new();
        loop {
            ensure!(
                seen.insert(url.to_string()),
                "Python runtime catalog pagination repeats a page"
            );
            let (bytes, headers) = self.fetch_bytes(&url, consumed).await?;
            let raw: Value = serde_json::from_slice(&bytes)
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog JSON"))?;
            let page = raw.as_array().ok_or_else(|| {
                eyre::eyre!("Python runtime catalog release page is not an array")
            })?;
            if let Some(scope) = scope {
                let selected = scope.select_metadata(&raw, target, flavor)?;
                for candidate in super::pbs_candidates(&selected, target, flavor)? {
                    builds.insert(candidate.build);
                }
                scope.ensure_unique_build(&builds)?;
                releases.extend(
                    selected
                        .as_array()
                        .expect("selected release array")
                        .iter()
                        .cloned(),
                );
            } else {
                releases.extend(page.iter().cloned());
            }
            let Some(next) = next_page(&headers, &base, page_number)? else {
                break;
            };
            ensure!(
                page_number < self.limits.max_pages,
                "Python runtime catalog page limit exceeded"
            );
            page_number = page_number
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("Python runtime catalog page limit exceeded"))?;
            url = next;
        }
        Ok(Value::Array(releases))
    }

    async fn complete_pbs_metadata(
        &self,
        releases: Value,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
        consumed: &mut usize,
    ) -> Result<RawCatalog> {
        let mut release_checksums = BTreeMap::new();
        for tag in required_pbs_checksums(&releases, target, flavor)? {
            // The pure candidate parser validates both the date and its binding
            // to matching archives before any checksum request is constructed.
            ensure!(
                tag.len() == 8 && tag.bytes().all(|byte| byte.is_ascii_digit()),
                "invalid Python runtime catalog checksum release identity"
            );
            let url = catalog_url(&self.endpoints.pbs_downloads_base)?
                .join(&format!("{tag}/SHA256SUMS"))
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog checksum endpoint"))?;
            let (bytes, _) = self.fetch_bytes(&url, consumed).await?;
            let text = String::from_utf8(bytes)
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog checksum encoding"))?;
            release_checksums.insert(tag, text);
        }
        Ok(RawCatalog::Pbs {
            releases,
            release_checksums,
        })
    }

    /// Explicit exact installation scope; full version-list callers keep the
    /// original complete catalog protocol above.
    pub(in super::super) async fn pbs_releases_for_exact(
        &self,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
        request: &crate::programs::shared::exact_version::ExactPythonRequest,
    ) -> Result<Vec<RuntimeRelease>> {
        let scope = ExactPbsScope::new(request)?;
        let mut identity = self.identity(RuntimeDistribution::Pbs, target, flavor)?;
        identity.exact_pbs_request = Some(scope.clone());
        if let Some(releases) = self.cached_releases_for_identity(&identity, target, flavor)? {
            ensure!(
                releases.iter().all(|release| scope.matches(release)),
                "Python runtime catalog exact cache contains a different request"
            );
            return scope.select_releases(releases);
        }
        // A complete snapshot is also sufficient evidence. A scoped snapshot
        // is never read by complete-list callers or by another exact request.
        let full_identity = self.identity(RuntimeDistribution::Pbs, target, flavor)?;
        if let Some(releases) = self.cached_releases_for_identity(&full_identity, target, flavor)? {
            return scope.select_releases(releases);
        }
        ensure!(
            self.policy != NetworkPolicy::Offline,
            "Python runtime catalog exact cache item is missing"
        );
        let mut consumed = 0;
        let releases = if let Some(build) = &scope.build {
            let mut url = catalog_url(&self.endpoints.pbs_releases)?;
            url.path_segments_mut()
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog tag endpoint"))?
                .pop_if_empty()
                .push("tags")
                .push(build);
            let (bytes, _) = self.fetch_bytes(&url, &mut consumed).await?;
            let release: Value = serde_json::from_slice(&bytes)
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog JSON"))?;
            ensure!(
                release.get("tag_name").and_then(Value::as_str) == Some(build.as_str()),
                "Python runtime catalog tag response has a different build"
            );
            scope.select_metadata(&Value::Array(vec![release]), target, flavor)?
        } else {
            self.fetch_pbs_pages(target, flavor, Some(&scope), &mut consumed)
                .await?
        };
        let raw = self
            .complete_pbs_metadata(releases, target, flavor, &mut consumed)
            .await?;
        let releases = scope.select_releases(raw.parse(target, flavor)?)?;
        self.publish_cache_for_identity(identity, raw)?;
        Ok(releases)
    }

    pub(in super::super) async fn pypy_releases(
        &self,
        target: &RuntimeArchiveTarget,
    ) -> Result<Vec<RuntimeRelease>> {
        let distribution = RuntimeDistribution::PyPy;
        let flavor = RuntimeFlavor::PyPy;
        if let Some(releases) = self.cached_releases(distribution, target, flavor)? {
            return Ok(releases);
        }
        let mut consumed = 0;
        let (bytes, _) = self
            .fetch_bytes(&catalog_url(&self.endpoints.pypy_versions)?, &mut consumed)
            .await?;
        let releases: Value = serde_json::from_slice(&bytes)
            .map_err(|_| eyre::eyre!("invalid Python runtime catalog JSON"))?;
        ensure!(
            releases.is_array(),
            "Python runtime catalog release page is not an array"
        );
        let (bytes, _) = self
            .fetch_bytes(&catalog_url(&self.endpoints.pypy_checksums)?, &mut consumed)
            .await?;
        let checksums_html = String::from_utf8(bytes)
            .map_err(|_| eyre::eyre!("invalid Python runtime catalog checksum encoding"))?;
        let raw = RawCatalog::PyPy {
            releases,
            checksums_html,
        };
        let releases = raw.parse(target, flavor)?;
        self.publish_cache(distribution, target, flavor, raw)?;
        Ok(releases)
    }

    fn identity(
        &self,
        distribution: RuntimeDistribution,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<CacheIdentity> {
        let (distribution, source_urls) = match distribution {
            RuntimeDistribution::Pbs => {
                ensure!(
                    flavor != RuntimeFlavor::PyPy,
                    "PyPy flavor is not a PBS archive flavor"
                );
                (
                    "pbs",
                    vec![
                        self.endpoints.pbs_releases.clone(),
                        self.endpoints.pbs_downloads_base.clone(),
                    ],
                )
            }
            RuntimeDistribution::PyPy => {
                ensure!(
                    flavor == RuntimeFlavor::PyPy,
                    "invalid PyPy catalog archive flavor"
                );
                (
                    "pypy",
                    vec![
                        self.endpoints.pypy_versions.clone(),
                        self.endpoints.pypy_checksums.clone(),
                    ],
                )
            }
            RuntimeDistribution::CpythonSource => eyre::bail!("source CPython uses its fixed official source plan, not the precompiled release catalog"),
            RuntimeDistribution::Conda => eyre::bail!("Conda Python uses its fixed package record, not the precompiled release catalog"),
        };
        let platform = match target.platform() {
            RuntimePlatform::Macos => "macos",
            RuntimePlatform::LinuxGnu => "linux-gnu",
            RuntimePlatform::LinuxMusl => "linux-musl",
        };
        let flavor = match flavor {
            RuntimeFlavor::InstallOnly => "install-only",
            RuntimeFlavor::InstallOnlyStripped => "install-only-stripped",
            RuntimeFlavor::FreeThreadedInstallOnly => "free-threaded-install-only",
            RuntimeFlavor::FreeThreadedInstallOnlyStripped => "free-threaded-install-only-stripped",
            RuntimeFlavor::PyPy => "pypy",
        };
        Ok(CacheIdentity {
            schema: 1,
            distribution: distribution.into(),
            source_urls,
            architecture: target.arch().into(),
            platform: platform.into(),
            flavor: flavor.into(),
            transport_mapping_sha256: super::transport::mapping_fingerprint(
                self.url_replacements.as_ref(),
            )?,
            exact_pbs_request: None,
        })
    }

    fn cache_path(
        &self,
        distribution: RuntimeDistribution,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<PathBuf> {
        self.cache_path_for_identity(&self.identity(distribution, target, flavor)?)
    }

    fn cache_path_for_identity(&self, identity: &CacheIdentity) -> Result<PathBuf> {
        let bytes = serde_json::to_vec(identity).map_err(|_| {
            eyre::eyre!("Python runtime catalog cache identity could not be encoded")
        })?;
        let key = Digest::from_sha256_bytes(&bytes).hex();
        Ok(self.cache_root.join(format!("{key}.json")))
    }

    fn read_cache(&self, path: &Path, identity: &CacheIdentity) -> Result<Option<CachedCatalog>> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(eyre::eyre!(
                    "Python runtime catalog cache could not be read"
                ));
            }
        };
        let cap = u64::try_from(self.limits.max_cache_bytes)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or_else(|| eyre::eyre!("Python runtime catalog cache size limit is invalid"))?;
        let mut bytes = Vec::new();
        file.take(cap)
            .read_to_end(&mut bytes)
            .map_err(|_| eyre::eyre!("Python runtime catalog cache could not be read"))?;
        ensure!(
            bytes.len() <= self.limits.max_cache_bytes,
            "Python runtime catalog cache size limit exceeded"
        );
        let cache: CachedCatalog = serde_json::from_slice(&bytes)
            .map_err(|_| eyre::eyre!("Python runtime catalog cache is malformed"))?;
        ensure!(
            &cache.identity == identity,
            "Python runtime catalog cache identity does not match the requested source and target"
        );
        ensure!(
            cache.raw.distribution() == cache.identity.distribution,
            "Python runtime catalog cache distribution is inconsistent"
        );
        Ok(Some(cache))
    }

    fn cached_releases(
        &self,
        distribution: RuntimeDistribution,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<Option<Vec<RuntimeRelease>>> {
        let identity = self.identity(distribution, target, flavor)?;
        let releases = self.cached_releases_for_identity(&identity, target, flavor)?;
        if releases.is_none() {
            ensure!(
                self.policy != NetworkPolicy::Offline,
                "Python runtime catalog cache item is missing for {} {} {} {}",
                identity.distribution,
                identity.architecture,
                identity.platform,
                identity.flavor
            );
        }
        Ok(releases)
    }

    fn cached_releases_for_identity(
        &self,
        identity: &CacheIdentity,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<Option<Vec<RuntimeRelease>>> {
        let path = self.cache_path_for_identity(identity)?;
        let Some(cache) = self.read_cache(&path, identity)? else {
            return Ok(None);
        };
        let fresh = !self.fresh_for.is_zero()
            && unix_now()?
                .checked_sub(cache.fetched_at_unix)
                .is_some_and(|age| Duration::from_secs(age) < self.fresh_for);
        if self.policy == NetworkPolicy::Offline || fresh {
            return cache.raw.parse(target, flavor).map(Some).map_err(|_| {
                eyre::eyre!("Python runtime catalog cache contains invalid release metadata")
            });
        }
        Ok(None)
    }

    fn publish_cache(
        &self,
        distribution: RuntimeDistribution,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
        raw: RawCatalog,
    ) -> Result<()> {
        let identity = self.identity(distribution, target, flavor)?;
        self.publish_cache_for_identity(identity, raw)
    }

    fn publish_cache_for_identity(&self, identity: CacheIdentity, raw: RawCatalog) -> Result<()> {
        let path = self.cache_path_for_identity(&identity)?;
        let cache = CachedCatalog {
            identity,
            fetched_at_unix: unix_now()?,
            raw,
        };
        std::fs::create_dir_all(&self.cache_root).map_err(|_| {
            eyre::eyre!("Python runtime catalog cache directory could not be created")
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.cache_root).map_err(|_| {
            eyre::eyre!("Python runtime catalog cache temporary file could not be created")
        })?;
        {
            let mut writer = BoundedWriter {
                inner: temporary.as_file_mut(),
                remaining: self.limits.max_cache_bytes,
                exceeded: false,
            };
            let result = serde_json::to_writer(&mut writer, &cache);
            ensure!(
                !writer.exceeded,
                "Python runtime catalog cache size limit exceeded"
            );
            result.map_err(|_| eyre::eyre!("Python runtime catalog cache could not be encoded"))?;
            writer
                .flush()
                .map_err(|_| eyre::eyre!("Python runtime catalog cache write failed"))?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| eyre::eyre!("Python runtime catalog cache write failed"))?;
        temporary
            .persist(path)
            .map_err(|_| eyre::eyre!("Python runtime catalog cache could not be published"))?;
        Ok(())
    }

    async fn fetch_bytes(
        &self,
        url: &reqwest::Url,
        consumed: &mut usize,
    ) -> Result<(Vec<u8>, reqwest::header::HeaderMap)> {
        ensure!(
            self.policy == NetworkPolicy::Allowed,
            "Python runtime catalog network access is disabled"
        );
        let request_url =
            super::transport::metadata_request_url(url.clone(), self.url_replacements.as_ref())?;
        let mut response = self
            .client
            .get(request_url)
            .send()
            .await
            .map_err(|_| eyre::eyre!("Python runtime catalog HTTP request failed"))?;
        let status = response.status();
        if !status.is_success() {
            let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || (status == reqwest::StatusCode::FORBIDDEN
                    && response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|value| value.as_bytes() == b"0"));
            ensure!(
                !rate_limited,
                "Python runtime catalog upstream rate limit exhausted"
            );
            return Err(eyre::eyre!("Python runtime catalog HTTP status {status}"));
        }
        let remaining = self.limits.max_total_bytes.saturating_sub(*consumed);
        if let Some(length) = response.content_length() {
            ensure!(
                usize::try_from(length).is_ok_and(|length| length
                    <= self.limits.max_response_bytes
                    && length <= remaining),
                "Python runtime catalog response byte limit exceeded"
            );
        }
        let headers = response.headers().clone();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| eyre::eyre!("Python runtime catalog response read failed"))?
        {
            let size = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
                eyre::eyre!("Python runtime catalog response byte limit exceeded")
            })?;
            ensure!(
                size <= self.limits.max_response_bytes,
                "Python runtime catalog response byte limit exceeded"
            );
            let total = consumed.checked_add(chunk.len()).ok_or_else(|| {
                eyre::eyre!("Python runtime catalog aggregate byte limit exceeded")
            })?;
            ensure!(
                total <= self.limits.max_total_bytes,
                "Python runtime catalog aggregate byte limit exceeded"
            );
            *consumed = total;
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, headers))
    }
}

fn catalog_url(value: &str) -> Result<reqwest::Url> {
    reqwest::Url::parse(value).map_err(|_| eyre::eyre!("invalid Python runtime catalog endpoint"))
}

fn validated_endpoint(value: &str, base: bool) -> Result<String> {
    let url = catalog_url(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid Python runtime catalog endpoint"
    );
    ensure!(
        !base || url.path().ends_with('/'),
        "Python runtime catalog downloads endpoint must end with a slash"
    );
    Ok(url.into())
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| eyre::eyre!("Python runtime catalog clock is before the Unix epoch"))
}

fn next_page(
    headers: &reqwest::header::HeaderMap,
    base: &reqwest::Url,
    current_page: usize,
) -> Result<Option<reqwest::Url>> {
    let mut next = None;
    for value in headers.get_all(reqwest::header::LINK).iter() {
        let value = value
            .to_str()
            .map_err(|_| eyre::eyre!("invalid Python runtime catalog pagination header"))?;
        for link in value.split(',') {
            let (locator, parameters) = link
                .trim()
                .strip_prefix('<')
                .and_then(|link| link.split_once('>'))
                .ok_or_else(|| eyre::eyre!("invalid Python runtime catalog pagination header"))?;
            ensure!(
                parameters.trim_start().starts_with(';'),
                "invalid Python runtime catalog pagination header"
            );
            let mut is_next = false;
            let mut found_rel = false;
            for parameter in parameters.split(';').skip(1) {
                let (key, value) = parameter.trim().split_once('=').ok_or_else(|| {
                    eyre::eyre!("invalid Python runtime catalog pagination relation")
                })?;
                if key.trim().eq_ignore_ascii_case("rel") {
                    ensure!(
                        !found_rel,
                        "ambiguous Python runtime catalog pagination relation"
                    );
                    found_rel = true;
                    let value = value.trim();
                    let value = if value.starts_with('"') {
                        value
                            .strip_prefix('"')
                            .and_then(|value| value.strip_suffix('"'))
                            .ok_or_else(|| {
                                eyre::eyre!("invalid Python runtime catalog pagination relation")
                            })?
                    } else {
                        ensure!(
                            !value.contains('"'),
                            "invalid Python runtime catalog pagination relation"
                        );
                        value
                    };
                    is_next = value
                        .split_ascii_whitespace()
                        .any(|relation| relation.eq_ignore_ascii_case("next"));
                }
            }
            ensure!(
                found_rel,
                "Python runtime catalog pagination relation is missing"
            );
            if !is_next {
                continue;
            }
            ensure!(next.is_none(), "ambiguous Python runtime catalog next page");
            let url = base
                .join(locator)
                .map_err(|_| eyre::eyre!("invalid Python runtime catalog next page"))?;
            ensure!(
                url.origin() == base.origin()
                    && url.path() == base.path()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none(),
                "Python runtime catalog pagination changed its source endpoint"
            );
            let pairs: Vec<_> = url.query_pairs().collect();
            ensure!(
                pairs.len() == 2,
                "invalid Python runtime catalog pagination query"
            );
            let mut page = None;
            let mut per_page = false;
            for (key, value) in pairs {
                match key.as_ref() {
                    "per_page" => {
                        ensure!(
                            !per_page && value == "10",
                            "invalid Python runtime catalog pagination size"
                        );
                        per_page = true;
                    }
                    "page" => {
                        ensure!(
                            page.is_none(),
                            "invalid Python runtime catalog pagination page"
                        );
                        let number = value.parse::<usize>().map_err(|_| {
                            eyre::eyre!("invalid Python runtime catalog pagination page")
                        })?;
                        ensure!(
                            value == number.to_string(),
                            "invalid Python runtime catalog pagination page"
                        );
                        page = Some(number);
                    }
                    _ => {
                        return Err(eyre::eyre!(
                            "invalid Python runtime catalog pagination query"
                        ));
                    }
                }
            }
            ensure!(
                per_page && page == current_page.checked_add(1),
                "Python runtime catalog pagination is not sequential"
            );
            next = Some(url);
        }
    }
    Ok(next)
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CacheIdentity {
    schema: u32,
    distribution: String,
    source_urls: Vec<String>,
    architecture: String,
    platform: String,
    flavor: String,
    transport_mapping_sha256: String,
    // Omitting None preserves the complete catalog's existing cache keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exact_pbs_request: Option<ExactPbsScope>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExactPbsScope {
    version: String,
    build: Option<String>,
}

impl ExactPbsScope {
    fn new(request: &crate::programs::shared::exact_version::ExactPythonRequest) -> Result<Self> {
        ensure!(
            !request.version_only().starts_with("pypy"),
            "an exact PBS request must identify CPython"
        );
        if let Some(build) = request.build_pin() {
            super::pbs_build_date(build)?;
        }
        Ok(Self {
            version: request.version_only().into(),
            build: request.build_pin().map(str::to_owned),
        })
    }

    fn matches(&self, release: &RuntimeRelease) -> bool {
        release.key().python_version == self.version
            && self
                .build
                .as_ref()
                .is_none_or(|build| *build == release.key().distribution_version)
    }

    fn ensure_unique_build(&self, builds: &BTreeSet<String>) -> Result<()> {
        ensure!(
            builds.len() < 2,
            "multiple official PBS builds match {}; pin one explicitly: {}",
            self.version,
            builds
                .iter()
                .map(|build| format!("{}+{build}", self.version))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(())
    }

    fn select_releases(&self, releases: Vec<RuntimeRelease>) -> Result<Vec<RuntimeRelease>> {
        let selected: Vec<_> = releases
            .into_iter()
            .filter(|release| self.matches(release))
            .collect();
        self.ensure_unique_build(
            &selected
                .iter()
                .map(|release| release.key().distribution_version.clone())
                .collect(),
        )?;
        Ok(selected)
    }

    /// Validate official filename/tag/locator/digest facts with the original
    /// candidate parser, then retain those same JSON assets. No RuntimeRelease
    /// or digest is manufactured while proving request uniqueness.
    fn select_metadata(
        &self,
        raw: &Value,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<Value> {
        let names: BTreeSet<_> = super::pbs_candidates(raw, target, flavor)?
            .into_iter()
            .filter(|candidate| {
                candidate.python_version == self.version
                    && self
                        .build
                        .as_ref()
                        .is_none_or(|build| *build == candidate.build)
            })
            .map(|candidate| candidate.filename)
            .collect();
        let mut selected = Vec::new();
        for release in raw
            .as_array()
            .ok_or_else(|| eyre::eyre!("Python runtime catalog release page is not an array"))?
        {
            let mut release = release.clone();
            let assets = release
                .get_mut("assets")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| eyre::eyre!("invalid official PBS release metadata"))?;
            assets.retain(|asset| {
                asset
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| names.contains(name))
            });
            if !assets.is_empty() {
                selected.push(release);
            }
        }
        Ok(Value::Array(selected))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedCatalog {
    identity: CacheIdentity,
    fetched_at_unix: u64,
    raw: RawCatalog,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "distribution", rename_all = "snake_case", deny_unknown_fields)]
enum RawCatalog {
    Pbs {
        releases: Value,
        release_checksums: BTreeMap<String, String>,
    },
    #[serde(rename = "pypy")]
    PyPy {
        releases: Value,
        checksums_html: String,
    },
}

impl RawCatalog {
    fn distribution(&self) -> &'static str {
        match self {
            Self::Pbs { .. } => "pbs",
            Self::PyPy { .. } => "pypy",
        }
    }

    fn parse(
        &self,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
    ) -> Result<Vec<RuntimeRelease>> {
        match self {
            Self::Pbs {
                releases,
                release_checksums,
            } => parse_pbs_releases(releases, target, flavor, release_checksums),
            Self::PyPy {
                releases,
                checksums_html,
            } => {
                ensure!(
                    flavor == RuntimeFlavor::PyPy,
                    "invalid PyPy catalog archive flavor"
                );
                parse_pypy_releases(releases, checksums_html, target)
            }
        }
    }
}

struct BoundedWriter<W> {
    inner: W,
    remaining: usize,
    exceeded: bool,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "Python runtime catalog cache size limit exceeded",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    use super::super::RuntimePlatform;

    include!("fixtures.rs");

    fn target() -> RuntimeArchiveTarget {
        RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap()
    }

    fn endpoints(base: &str) -> CatalogEndpoints {
        CatalogEndpoints::new(
            &format!("{base}/pbs/releases"),
            &format!("{base}/pbs/downloads/"),
            &format!("{base}/pypy/versions.json"),
            &format!("{base}/pypy/checksums.html"),
        )
        .unwrap()
    }

    fn catalog(
        root: &Path,
        base: &str,
        policy: NetworkPolicy,
        fresh_for: Duration,
        limits: CatalogLimits,
    ) -> RuntimeCatalog {
        RuntimeCatalog::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            root,
            endpoints(base),
            policy,
            fresh_for,
            limits,
        )
        .unwrap()
    }

    fn online(root: &Path, base: &str) -> RuntimeCatalog {
        catalog(
            root,
            base,
            NetworkPolicy::Allowed,
            Duration::from_secs(3600),
            CatalogLimits::default(),
        )
    }

    async fn page(
        server: &mut mockito::ServerGuard,
        number: u32,
        body: String,
        next: Option<&str>,
    ) -> mockito::Mock {
        let mut request = server
            .mock("GET", "/pbs/releases")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("per_page".into(), "10".into()),
                mockito::Matcher::UrlEncoded("page".into(), number.to_string()),
            ]))
            .with_body(body)
            .expect(1);
        if let Some(next) = next {
            request = request.with_header("link", next);
        }
        request.create_async().await
    }

    fn old_pbs() -> Value {
        let mut raw: Value = serde_json::from_str(PBS_FIXTURE).unwrap();
        raw[0]["tag_name"] = json!("20240909");
        raw[0]["published_at"] = json!("2024-09-09T00:00:00Z");
        raw[0]["assets"] = json!([{
            "name":"cpython-3.10.14+20240909-aarch64-apple-darwin-install_only.tar.gz",
            "browser_download_url":"https://github.com/astral-sh/python-build-standalone/releases/download/20240909/cpython-3.10.14%2B20240909-aarch64-apple-darwin-install_only.tar.gz",
            "digest":format!("sha256:{}", "a".repeat(64))
        }]);
        raw
    }

    fn expire(path: &Path) {
        let mut raw: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        raw["fetched_at_unix"] = json!(0);
        std::fs::write(path, serde_json::to_vec(&raw).unwrap()).unwrap();
    }

    #[test]
    fn official_pbs_endpoint_uses_githubs_canonical_repository_identity() {
        assert_eq!(
            CatalogEndpoints::official().unwrap().pbs_releases,
            "https://api.github.com/repositories/162334160/releases"
        );
    }

    #[tokio::test]
    async fn numeric_repository_pagination_requests_ten_releases_and_keeps_history() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let base = server.url();
        let path = "/repositories/162334160/releases";
        let link = format!("<{base}{path}?per_page=10&page=2>; rel=\"next\"");
        let first = server
            .mock("GET", path)
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("per_page".into(), "10".into()),
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
            ]))
            .with_header("link", &link)
            .with_body(PBS_FIXTURE)
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", path)
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("per_page".into(), "10".into()),
                mockito::Matcher::UrlEncoded("page".into(), "2".into()),
            ]))
            .with_body(old_pbs().to_string())
            .expect(1)
            .create_async()
            .await;
        let endpoints = CatalogEndpoints::new(
            &format!("{base}{path}"),
            &format!("{base}/pbs/downloads/"),
            &format!("{base}/pypy/versions.json"),
            &format!("{base}/pypy/checksums.html"),
        )
        .unwrap();
        let catalog = RuntimeCatalog::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            root.path(),
            endpoints,
            NetworkPolicy::Allowed,
            Duration::ZERO,
            CatalogLimits::default(),
        )
        .unwrap();
        let releases = catalog
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .unwrap();
        assert_eq!(releases.len(), 2);
        assert!(
            releases
                .iter()
                .any(|release| release.request_version() == "3.10.14")
        );
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn pbs_follows_page_two_after_a_stable_release_without_history_truncation() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let link = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            server.url()
        );
        let first = page(&mut server, 1, PBS_FIXTURE.into(), Some(&link)).await;
        let second = page(&mut server, 2, old_pbs().to_string(), None).await;
        let releases = online(root.path(), &server.url())
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .unwrap();
        assert_eq!(releases.len(), 2);
        assert!(
            releases
                .iter()
                .any(|release| release.key().python_version == "3.15.0")
        );
        assert!(
            releases
                .iter()
                .any(|release| release.key().python_version == "3.10.14")
        );
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn only_a_matching_missing_api_hash_fetches_its_release_manifest() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut raw = old_pbs();
        raw[0]["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        let mut unrelated = old_pbs()[0].clone();
        unrelated["tag_name"] = json!("20240908");
        unrelated["assets"][0]["name"] =
            json!("cpython-3.10.14+20240908-x86_64-unknown-linux-gnu-install_only.tar.gz");
        unrelated["assets"][0]["browser_download_url"] = json!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20240908/cpython-3.10.14%2B20240908-x86_64-unknown-linux-gnu-install_only.tar.gz"
        );
        unrelated["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        raw.as_array_mut().unwrap().push(unrelated);
        let listing = page(&mut server, 1, raw.to_string(), None).await;
        let checksum = server
            .mock("GET", "/pbs/downloads/20240909/SHA256SUMS")
            .with_body(format!(
                "{}  cpython-3.10.14+20240909-aarch64-apple-darwin-install_only.tar.gz\n",
                "a".repeat(64)
            ))
            .expect(1)
            .create_async()
            .await;
        let unrelated = server
            .mock("GET", "/pbs/downloads/20240908/SHA256SUMS")
            .expect(0)
            .create_async()
            .await;
        let releases = online(root.path(), &server.url())
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].digest().hex(), "a".repeat(64));
        listing.assert_async().await;
        checksum.assert_async().await;
        unrelated.assert_async().await;
    }

    #[tokio::test]
    async fn pypy_fetches_both_official_inputs_and_keeps_the_full_language_patch() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let versions = server
            .mock("GET", "/pypy/versions.json")
            .with_body(PYPY_FIXTURE)
            .expect(1)
            .create_async()
            .await;
        let checksums = server
            .mock("GET", "/pypy/checksums.html")
            .with_body(PYPY_CHECKSUMS)
            .expect(1)
            .create_async()
            .await;
        let releases = online(root.path(), &server.url())
            .pypy_releases(&target())
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].key().python_version, "3.12.15");
        assert_eq!(releases[0].key().implementation_version, "8.0.1");
        versions.assert_async().await;
        checksums.assert_async().await;
    }

    #[tokio::test]
    async fn online_fresh_cache_and_offline_expired_cache_make_no_further_requests() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let listing = page(&mut server, 1, PBS_FIXTURE.into(), None).await;
        let current = online(root.path(), &server.url());
        assert_eq!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        let path = current
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        expire(&path);
        let offline = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Offline,
            Duration::ZERO,
            CatalogLimits::default(),
        );
        assert_eq!(
            offline
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            offline
                .pbs_releases(&target(), RuntimeFlavor::InstallOnlyStripped)
                .await
                .is_err()
        );
        assert!(
            offline
                .pbs_releases(
                    &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::LinuxGnu).unwrap(),
                    RuntimeFlavor::InstallOnly,
                )
                .await
                .is_err()
        );
        listing.assert_async().await;
    }

    #[tokio::test]
    async fn offline_missing_and_corrupt_items_fail_without_any_http() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let requests = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let offline = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Offline,
            Duration::ZERO,
            CatalogLimits::default(),
        );
        let error = offline
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("cache"), "{error}");
        let path = offline
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not JSON").unwrap();
        let error = offline
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("cache"), "{error}");
        assert!(offline.pypy_releases(&target()).await.is_err());
        requests.assert_async().await;
    }

    #[test]
    fn cache_identity_separates_endpoints_distribution_architecture_platform_and_flavor() {
        let root = tempfile::tempdir().unwrap();
        let first = online(root.path(), "http://127.0.0.1:30101");
        let second = online(root.path(), "http://127.0.0.1:30102");
        let mut changed_downloads = online(root.path(), "http://127.0.0.1:30101");
        changed_downloads.endpoints.pbs_downloads_base =
            "http://127.0.0.1:30101/other-downloads/".into();
        let path = first
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        for other in [
            changed_downloads
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
            second
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
            first
                .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
                .unwrap(),
            first
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnlyStripped,
                )
                .unwrap(),
            first
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &RuntimeArchiveTarget::new("x86_64", RuntimePlatform::Macos).unwrap(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
            first
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::LinuxGnu).unwrap(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
        ] {
            assert_ne!(path, other);
        }
        let pypy = first
            .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
            .unwrap();
        let mut changed_checksums = online(root.path(), "http://127.0.0.1:30101");
        changed_checksums.endpoints.pypy_checksums =
            "http://127.0.0.1:30101/other-checksums.html".into();
        assert_ne!(
            pypy,
            changed_checksums
                .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn failed_later_page_preserves_the_entire_previous_cache_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let listing = page(&mut server, 1, PBS_FIXTURE.into(), None).await;
        let current = online(root.path(), &server.url());
        assert_eq!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        listing.assert_async().await;
        listing.remove_async().await;
        let path = current
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        expire(&path);
        let before = std::fs::read(&path).unwrap();
        let link = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            server.url()
        );
        let first = page(&mut server, 1, old_pbs().to_string(), Some(&link)).await;
        let second = server
            .mock("GET", "/pbs/releases")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(503)
            .with_body("secret catalog response")
            .expect(1)
            .create_async()
            .await;
        assert!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn malformed_json_and_http_failures_do_not_publish_or_leak_response_data() {
        for status in [200, 403] {
            let root = tempfile::tempdir().unwrap();
            let mut server = mockito::Server::new_async().await;
            let request = server
                .mock("GET", "/pypy/versions.json")
                .with_status(status)
                .with_body("not JSON https://user:password@host.invalid/?token=secret")
                .expect(1)
                .create_async()
                .await;
            let current = online(root.path(), &server.url());
            let error = current.pypy_releases(&target()).await.err().unwrap();
            let message = format!("{error:?} {error}");
            for secret in ["password", "token=secret", "host.invalid", "not JSON"] {
                assert!(!message.contains(secret), "{message}");
            }
            assert!(
                !current
                    .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
                    .unwrap()
                    .exists()
            );
            request.assert_async().await;
        }
    }

    #[tokio::test]
    async fn missing_required_checksum_never_publishes_a_partial_catalog() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut raw = old_pbs();
        raw[0]["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        let listing = page(&mut server, 1, raw.to_string(), None).await;
        let checksum = server
            .mock("GET", "/pbs/downloads/20240909/SHA256SUMS")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;
        let current = online(root.path(), &server.url());
        assert!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .is_err()
        );
        assert!(
            !current
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly
                )
                .unwrap()
                .exists()
        );
        listing.assert_async().await;
        checksum.assert_async().await;
    }

    #[tokio::test]
    async fn page_limit_is_an_error_before_an_unavailable_page_is_requested() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let link = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            server.url()
        );
        let first = page(&mut server, 1, PBS_FIXTURE.into(), Some(&link)).await;
        let second = server
            .mock("GET", "/pbs/releases")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .expect(0)
            .create_async()
            .await;
        let current = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Allowed,
            Duration::ZERO,
            CatalogLimits {
                max_pages: 1,
                ..CatalogLimits::default()
            },
        );
        let error = current
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(
            !current
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly
                )
                .unwrap()
                .exists()
        );
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn response_and_aggregate_byte_limits_fail_instead_of_truncating_success() {
        for limits in [
            CatalogLimits {
                max_response_bytes: 32,
                ..CatalogLimits::default()
            },
            CatalogLimits {
                max_total_bytes: 32,
                ..CatalogLimits::default()
            },
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut server = mockito::Server::new_async().await;
            let request = server
                .mock("GET", "/pypy/versions.json")
                .with_body(PYPY_FIXTURE)
                .expect(1)
                .create_async()
                .await;
            let current = catalog(
                root.path(),
                &server.url(),
                NetworkPolicy::Allowed,
                Duration::ZERO,
                limits,
            );
            let error = current.pypy_releases(&target()).await.err().unwrap();
            assert!(error.to_string().contains("limit"), "{error}");
            assert!(
                !current
                    .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
                    .unwrap()
                    .exists()
            );
            request.assert_async().await;
        }
    }

    #[tokio::test]
    async fn aggregate_limit_applies_across_versions_and_checksum_responses() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let versions = server
            .mock("GET", "/pypy/versions.json")
            .with_body(PYPY_FIXTURE)
            .expect(1)
            .create_async()
            .await;
        let checksums = server
            .mock("GET", "/pypy/checksums.html")
            .with_body(PYPY_CHECKSUMS)
            .expect(1)
            .create_async()
            .await;
        let current = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Allowed,
            Duration::ZERO,
            CatalogLimits {
                max_total_bytes: PYPY_FIXTURE.len() + PYPY_CHECKSUMS.len() - 1,
                ..CatalogLimits::default()
            },
        );
        let error = current.pypy_releases(&target()).await.err().unwrap();
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(
            !current
                .cache_path(RuntimeDistribution::PyPy, &target(), RuntimeFlavor::PyPy)
                .unwrap()
                .exists()
        );
        versions.assert_async().await;
        checksums.assert_async().await;
    }

    #[tokio::test]
    async fn cache_write_limit_never_publishes_a_truncated_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let listing = page(&mut server, 1, PBS_FIXTURE.into(), None).await;
        let current = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Allowed,
            Duration::ZERO,
            CatalogLimits {
                max_cache_bytes: 32,
                ..CatalogLimits::default()
            },
        );
        let error = current
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("cache") && error.to_string().contains("limit"),
            "{error}"
        );
        assert!(
            !current
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly
                )
                .unwrap()
                .exists()
        );
        listing.assert_async().await;
    }

    #[tokio::test]
    async fn offline_cannot_accept_another_flavors_snapshot_copied_to_its_path() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let listing = page(&mut server, 1, PBS_FIXTURE.into(), None).await;
        let current = online(root.path(), &server.url());
        assert_eq!(
            current
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        let original = current
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        let other = current
            .cache_path(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnlyStripped,
            )
            .unwrap();
        std::fs::copy(original, other).unwrap();
        let offline = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Offline,
            Duration::ZERO,
            CatalogLimits::default(),
        );
        let error = offline
            .pbs_releases(&target(), RuntimeFlavor::InstallOnlyStripped)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("identity"), "{error}");
        listing.assert_async().await;
    }

    #[tokio::test]
    async fn malformed_api_hash_does_not_trigger_a_manifest_retry() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut raw = old_pbs();
        raw[0]["assets"][0]["digest"] = json!("sha256:bad");
        let listing = page(&mut server, 1, raw.to_string(), None).await;
        let manifest = server
            .mock("GET", "/pbs/downloads/20240909/SHA256SUMS")
            .expect(0)
            .create_async()
            .await;
        let current = online(root.path(), &server.url());
        let error = current
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("SHA256"), "{error}");
        assert!(
            !current
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly
                )
                .unwrap()
                .exists()
        );
        listing.assert_async().await;
        manifest.assert_async().await;
    }

    #[tokio::test]
    async fn upstream_rate_limit_is_explicit_and_does_not_expose_its_body() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/pypy/versions.json")
            .with_status(403)
            .with_header("x-ratelimit-remaining", "0")
            .with_body("secret response token=hidden")
            .expect(1)
            .create_async()
            .await;
        let error = online(root.path(), &server.url())
            .pypy_releases(&target())
            .await
            .err()
            .unwrap();
        let message = format!("{error:?} {error}");
        assert!(message.contains("rate limit"), "{message}");
        assert!(
            !message.contains("secret response") && !message.contains("hidden"),
            "{message}"
        );
        request.assert_async().await;
    }

    #[tokio::test]
    async fn pagination_cannot_change_origin_path_or_repeat_a_page() {
        for destination in ["other_origin", "other_path", "repeat", "bad_page"] {
            let root = tempfile::tempdir().unwrap();
            let mut server = mockito::Server::new_async().await;
            let mut outside = mockito::Server::new_async().await;
            let outside_calls = outside
                .mock("GET", mockito::Matcher::Any)
                .expect(0)
                .create_async()
                .await;
            let next = match destination {
                "other_origin" => format!("{}/pbs/releases?per_page=10&page=2", outside.url()),
                "other_path" => format!("{}/unrelated?per_page=10&page=2", server.url()),
                "repeat" => format!("{}/pbs/releases?per_page=10&page=1", server.url()),
                _ => format!("{}/pbs/releases?per_page=10&page=oops", server.url()),
            };
            let link = format!("<{next}>; rel=\"next\"");
            let first = page(&mut server, 1, PBS_FIXTURE.into(), Some(&link)).await;
            let current = online(root.path(), &server.url());
            assert!(
                current
                    .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                    .await
                    .is_err(),
                "{destination}"
            );
            assert!(
                !current
                    .cache_path(
                        RuntimeDistribution::Pbs,
                        &target(),
                        RuntimeFlavor::InstallOnly
                    )
                    .unwrap()
                    .exists()
            );
            first.assert_async().await;
            outside_calls.assert_async().await;
        }
    }

    #[test]
    fn endpoint_and_zero_limits_are_rejected_without_exposing_secrets() {
        for endpoint in [
            "https://user:password@host.invalid/releases",
            "https://host.invalid/releases?token=secret",
            "https://host.invalid/releases#fragment",
            "file:///tmp/catalog.json",
        ] {
            let error = CatalogEndpoints::new(
                endpoint,
                "https://host.invalid/download/",
                "https://host.invalid/versions.json",
                "https://host.invalid/checksums.html",
            )
            .err()
            .unwrap();
            let message = format!("{error:?} {error}");
            assert!(
                !message.contains("password") && !message.contains("token=secret"),
                "{message}"
            );
        }
        for limits in [
            CatalogLimits {
                max_pages: 0,
                ..CatalogLimits::default()
            },
            CatalogLimits {
                max_response_bytes: 0,
                ..CatalogLimits::default()
            },
            CatalogLimits {
                max_total_bytes: 0,
                ..CatalogLimits::default()
            },
            CatalogLimits {
                max_cache_bytes: 0,
                ..CatalogLimits::default()
            },
        ] {
            assert!(
                RuntimeCatalog::new(
                    reqwest::Client::new(),
                    "/unused",
                    CatalogEndpoints::official().unwrap(),
                    NetworkPolicy::Offline,
                    Duration::ZERO,
                    limits
                )
                .is_err()
            );
        }
    }

    fn mapped_catalog(
        root: &Path,
        canonical: &str,
        policy: NetworkPolicy,
        routes: Option<&indexmap::IndexMap<String, String>>,
    ) -> RuntimeCatalog {
        RuntimeCatalog::new(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            root,
            endpoints(canonical),
            policy,
            Duration::ZERO,
            CatalogLimits::default(),
        )
        .unwrap()
        .with_url_replacements(routes)
    }

    #[tokio::test]
    async fn explicit_routes_fetch_mirror_pages_while_pagination_and_source_identity_stay_canonical()
     {
        let root = tempfile::tempdir().unwrap();
        let mut canonical = mockito::Server::new_async().await;
        let mut mirror = mockito::Server::new_async().await;
        let no_canonical = canonical
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let routes = indexmap::IndexMap::from([(canonical.url(), mirror.url())]);
        let link = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            canonical.url()
        );
        let first = page(&mut mirror, 1, PBS_FIXTURE.into(), Some(&link)).await;
        let second = page(&mut mirror, 2, old_pbs().to_string(), None).await;
        let catalog = mapped_catalog(
            root.path(),
            &canonical.url(),
            NetworkPolicy::Allowed,
            Some(&routes),
        );
        let releases = catalog
            .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
            .await
            .unwrap();
        assert_eq!(releases.len(), 2);
        assert!(
            releases
                .iter()
                .all(|release| release.source().url().host_str() == Some("github.com"))
        );
        let cached = std::fs::read_to_string(
            catalog
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
        )
        .unwrap();
        assert!(!cached.contains(&mirror.url()));
        for mock in [no_canonical, first, second] {
            mock.assert_async().await;
        }
    }

    #[test]
    fn route_fingerprint_separates_cache_identity_without_serializing_secrets_or_losing_rule_order()
    {
        let root = tempfile::tempdir().unwrap();
        let routes = indexmap::IndexMap::from([
            (
                "first".to_string(),
                "https://mirror.invalid/a?token=hidden".to_string(),
            ),
            (
                "second".to_string(),
                "https://alice:password@private.invalid/b".to_string(),
            ),
        ]);
        let reversed = routes
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let plain = mapped_catalog(
            root.path(),
            "https://canonical.invalid",
            NetworkPolicy::Offline,
            None,
        );
        let routed = mapped_catalog(
            root.path(),
            "https://canonical.invalid",
            NetworkPolicy::Offline,
            Some(&routes),
        );
        let reverse = mapped_catalog(
            root.path(),
            "https://canonical.invalid",
            NetworkPolicy::Offline,
            Some(&reversed),
        );
        let path = |catalog: &RuntimeCatalog| {
            catalog
                .cache_path(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap()
        };
        assert_ne!(path(&plain), path(&routed));
        assert_ne!(path(&routed), path(&reverse));
        let encoded = serde_json::to_string(
            &routed
                .identity(
                    RuntimeDistribution::Pbs,
                    &target(),
                    RuntimeFlavor::InstallOnly,
                )
                .unwrap(),
        )
        .unwrap();
        for secret in [
            "hidden",
            "alice",
            "password",
            "mirror.invalid",
            "private.invalid",
        ] {
            assert!(!encoded.contains(secret), "{encoded}");
        }
    }

    #[tokio::test]
    async fn offline_route_replay_uses_only_the_matching_mapping_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut canonical = mockito::Server::new_async().await;
        let mut mirror = mockito::Server::new_async().await;
        let no_canonical = canonical
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let routes = indexmap::IndexMap::from([(canonical.url(), mirror.url())]);
        let listing = page(&mut mirror, 1, PBS_FIXTURE.into(), None).await;
        let online = mapped_catalog(
            root.path(),
            &canonical.url(),
            NetworkPolicy::Allowed,
            Some(&routes),
        );
        assert_eq!(
            online
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        let offline = mapped_catalog(
            root.path(),
            &canonical.url(),
            NetworkPolicy::Offline,
            Some(&routes),
        );
        assert_eq!(
            offline
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .unwrap()
                .len(),
            1
        );
        let other = mapped_catalog(root.path(), &canonical.url(), NetworkPolicy::Offline, None);
        assert!(
            other
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .is_err()
        );
        for mock in [no_canonical, listing] {
            mock.assert_async().await;
        }
    }
    #[tokio::test]
    async fn f2_exact_catalog_pin_fetches_only_its_official_tag_and_checksum() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut release = old_pbs()[0].clone();
        release["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        let tag = server
            .mock("GET", "/pbs/releases/tags/20240909")
            .with_body(release.to_string())
            .expect(1)
            .create_async()
            .await;
        let all = server
            .mock("GET", "/pbs/releases")
            .expect(0)
            .create_async()
            .await;
        let checksum = server
            .mock("GET", "/pbs/downloads/20240909/SHA256SUMS")
            .with_body(format!(
                "{}  cpython-3.10.14+20240909-aarch64-apple-darwin-install_only.tar.gz\n",
                "a".repeat(64)
            ))
            .expect(1)
            .create_async()
            .await;
        let unrelated = server
            .mock("GET", "/pbs/downloads/20240908/SHA256SUMS")
            .expect(0)
            .create_async()
            .await;
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.10.14+20240909")
                .unwrap();
        let releases = online(root.path(), &server.url())
            .pbs_releases_for_exact(&target(), RuntimeFlavor::InstallOnly, &request)
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].key().distribution_version, "20240909");
        assert_eq!(releases[0].digest().hex(), "a".repeat(64));
        let offline = catalog(
            root.path(),
            &server.url(),
            NetworkPolicy::Offline,
            Duration::ZERO,
            CatalogLimits::default(),
        );
        assert_eq!(
            offline
                .pbs_releases_for_exact(&target(), RuntimeFlavor::InstallOnly, &request)
                .await
                .unwrap()[0]
                .digest(),
            releases[0].digest()
        );
        // The partial snapshot cannot answer a full-list or bare request.
        assert!(
            offline
                .pbs_releases(&target(), RuntimeFlavor::InstallOnly)
                .await
                .is_err()
        );
        let bare = crate::programs::shared::exact_version::exact_python_request("3.10.14").unwrap();
        assert!(
            offline
                .pbs_releases_for_exact(&target(), RuntimeFlavor::InstallOnly, &bare)
                .await
                .is_err()
        );
        let full_identity = offline
            .identity(
                RuntimeDistribution::Pbs,
                &target(),
                RuntimeFlavor::InstallOnly,
            )
            .unwrap();
        assert!(
            serde_json::to_value(full_identity)
                .unwrap()
                .get("exact_pbs_request")
                .is_none(),
            "the original complete cache identity bytes must remain unchanged"
        );
        for mock in [tag, all, checksum, unrelated] {
            mock.assert_async().await;
        }
    }

    #[tokio::test]
    async fn f2_exact_catalog_bare_request_refuses_two_real_builds_before_checksums() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut first = old_pbs();
        first[0]["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        let mut second = first.clone();
        second[0]["tag_name"] = json!("20240910");
        second[0]["published_at"] = json!("2024-09-10T00:00:00Z");
        second[0]["assets"][0]["name"] =
            json!("cpython-3.10.14+20240910-aarch64-apple-darwin-install_only.tar.gz");
        second[0]["assets"][0]["browser_download_url"] = json!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20240910/cpython-3.10.14%2B20240910-aarch64-apple-darwin-install_only.tar.gz"
        );
        let next = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            server.url()
        );
        let more = format!(
            "<{}/pbs/releases?per_page=10&page=3>; rel=\"next\"",
            server.url()
        );
        let one = page(&mut server, 1, first.to_string(), Some(&next)).await;
        let two = page(&mut server, 2, second.to_string(), Some(&more)).await;
        let third = server
            .mock("GET", "/pbs/releases")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "3".into()))
            .expect(0)
            .create_async()
            .await;
        let manifests = server
            .mock(
                "GET",
                mockito::Matcher::Regex("^/pbs/downloads/.*/SHA256SUMS$".into()),
            )
            .expect(0)
            .create_async()
            .await;
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.10.14").unwrap();
        let error = online(root.path(), &server.url())
            .pbs_releases_for_exact(&target(), RuntimeFlavor::InstallOnly, &request)
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("pin one explicitly")
                && message.contains("20240909")
                && message.contains("20240910"),
            "{message}"
        );
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "an ambiguous partial catalog must not become a complete cache"
        );
        for mock in [one, two, third, manifests] {
            mock.assert_async().await;
        }
    }

    #[tokio::test]
    async fn f2_exact_catalog_single_build_proves_final_page_and_skips_unrelated_checksum() {
        let root = tempfile::tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mut raw = old_pbs();
        raw[0]["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        let mut unrelated = raw[0].clone();
        unrelated["tag_name"] = json!("20240910");
        unrelated["published_at"] = json!("2024-09-10T00:00:00Z");
        unrelated["assets"][0]["name"] =
            json!("cpython-3.11.0+20240910-aarch64-apple-darwin-install_only.tar.gz");
        unrelated["assets"][0]["browser_download_url"] = json!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20240910/cpython-3.11.0%2B20240910-aarch64-apple-darwin-install_only.tar.gz"
        );
        raw.as_array_mut().unwrap().push(unrelated);
        let next = format!(
            "<{}/pbs/releases?per_page=10&page=2>; rel=\"next\"",
            server.url()
        );
        let first = page(&mut server, 1, raw.to_string(), Some(&next)).await;
        let final_page = page(&mut server, 2, "[]".into(), None).await;
        let checksum = server
            .mock("GET", "/pbs/downloads/20240909/SHA256SUMS")
            .with_body(format!(
                "{}  cpython-3.10.14+20240909-aarch64-apple-darwin-install_only.tar.gz\n",
                "a".repeat(64)
            ))
            .expect(1)
            .create_async()
            .await;
        let other_checksum = server
            .mock("GET", "/pbs/downloads/20240910/SHA256SUMS")
            .expect(0)
            .create_async()
            .await;
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.10.14").unwrap();
        let releases = online(root.path(), &server.url())
            .pbs_releases_for_exact(&target(), RuntimeFlavor::InstallOnly, &request)
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].key().distribution_version, "20240909");
        for mock in [first, final_page, checksum, other_checksum] {
            mock.assert_async().await;
        }
    }
}
