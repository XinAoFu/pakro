//! Explicit Simple HTTP/session inputs and bounded HTTP/charset observation.
//!
//! Registry pages and independent file sessions share one checked transport.
//! The client does not select another registry, resolve dependency versions or
//! authorize a new pin. File-body receipts confer no installation authority.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use reqwest::Url;
use reqwest::header::HeaderValue;
use thiserror::Error;

use crate::artifact::NetworkPolicy;
use crate::python_source::{
    AuthScopeKey, CredentialRef, NormalizedPythonName, PythonRegistry, TransportRulesFingerprint,
};

use super::{SimpleError, SimpleHtmlLimits, SimpleLimits, SimpleProject};

mod cache;
mod decode;
mod file;
pub use file::{CompletedSourceTransfer, SourceFileLimits, SourceFileSession};
pub(super) mod http;

pub type SimpleClientResult<T> = Result<T, SimpleClientError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleClientLimitKind {
    WireBytes,
    DecodedBytes,
    HeaderBytes,
    HeaderCount,
    CacheBytes,
    Redirects,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SimpleClientError {
    #[error("invalid Simple client configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("Simple client limits must be positive")]
    InvalidLimits,
    #[error("Simple metadata is absent from its source cache and network access is disabled")]
    OfflineMissing,
    #[error("Simple HTTP status is not successful: {0}")]
    HttpStatus(u16),
    #[error("unsupported or missing Simple response media type")]
    UnsupportedMediaType,
    #[error("unsupported or inconsistent Simple response charset")]
    UnsupportedEncoding,
    #[error("Simple response contains invalid encoded text")]
    InvalidEncoding,
    #[error("Simple response has an unsupported content encoding")]
    UnsupportedContentEncoding,
    #[error("Simple client limit exceeded: {0:?}")]
    LimitExceeded(SimpleClientLimitKind),
    #[error("Simple request deadline expired")]
    Timeout,
    #[error("Simple HTTP transport failed")]
    Transport,
    #[error("invalid Simple request or redirect locator")]
    InvalidLocator,
    #[error("Simple canonical and physical redirect locations are ambiguous")]
    AmbiguousRedirect,
    #[error("Simple request origin has no matching credential authorization")]
    Authentication,
    #[error("invalid Simple metadata cache: {0}")]
    InvalidCache(&'static str),
    #[error("Simple metadata cache I/O failed")]
    Io,
    #[error("Simple response facts are invalid: {0}")]
    Parse(SimpleError),
    #[error("Simple client stage is not implemented")]
    NotImplemented,
}

/// Transport settings are explicit values, never a client/builder with unknown
/// default headers. The application may opt into the standard proxy snapshot
/// taken when this client is built; it is disabled for isolated source callers.
#[derive(Clone)]
pub struct SimpleHttpConfig {
    pub connect_timeout: Duration,
    pub read_timeout: Option<Duration>,
    pub request_timeout: Duration,
    pub max_redirects: usize,
    /// Resolve environment/system proxies once during client construction.
    /// An explicit proxy list cannot be combined with this option.
    pub use_system_proxy: bool,
    pub proxies: Vec<reqwest::Proxy>,
    pub root_certificates: Vec<reqwest::Certificate>,
}
impl Default for SimpleHttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: None,
            request_timeout: Duration::from_secs(30),
            max_redirects: 10,
            use_system_proxy: false,
            proxies: Vec::new(),
            root_certificates: Vec::new(),
        }
    }
}
impl fmt::Debug for SimpleHttpConfig {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SimpleHttpConfig")
            .field("connect_timeout", &self.connect_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_redirects", &self.max_redirects)
            .field("use_system_proxy", &self.use_system_proxy)
            .field("proxy_count", &self.proxies.len())
            .field("root_certificate_count", &self.root_certificates.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SimpleClientLimits {
    pub max_wire_bytes: u64,
    pub max_cache_bytes: u64,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub max_decoded_bytes: u64,
    pub parser: SimpleLimits,
    pub html: SimpleHtmlLimits,
}
impl Default for SimpleClientLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 64 * 1024 * 1024,
            max_cache_bytes: 66 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_headers: 64,
            max_decoded_bytes: 64 * 1024 * 1024,
            parser: SimpleLimits::default(),
            html: SimpleHtmlLimits::default(),
        }
    }
}
impl SimpleClientLimits {
    /// Validate explicit limits even when no project lookup is needed.
    pub fn validate(self) -> SimpleClientResult<()> {
        http::validate_limits(self)
    }
}

/// Exact effective HTTP origin. No URL/path/query or secret is displayed.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpOrigin(Url);
impl HttpOrigin {
    pub fn new(url: &Url) -> SimpleClientResult<Self> {
        http::locator(url)?;
        let origin = Url::parse(&url.origin().ascii_serialization())
            .map_err(|_| SimpleClientError::InvalidLocator)?;
        Ok(Self(origin))
    }
}
impl fmt::Debug for HttpOrigin {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("HttpOrigin(<origin>)")
    }
}

/// A finite, already-resolved credential for exactly one granted HTTP origin.
/// No arbitrary client default header injection or credential refresh is exposed.
pub struct OriginAuthorization {
    origin: HttpOrigin,
    value: HeaderValue,
}
impl OriginAuthorization {
    pub fn new(origin: HttpOrigin, mut value: HeaderValue) -> SimpleClientResult<Self> {
        if value
            .as_bytes()
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t'))
        {
            return Err(SimpleClientError::Authentication);
        }
        value.set_sensitive(true);
        Ok(Self { origin, value })
    }
}
impl fmt::Debug for OriginAuthorization {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("OriginAuthorization(<sensitive header>)")
    }
}

/// Empty grants authorize only the registry's canonical origin. A mirror or
/// redirect origin must be granted explicitly before consulting its provider.
#[derive(Debug, Default)]
pub struct AuthOriginGrants {
    additional: Vec<HttpOrigin>,
}
impl AuthOriginGrants {
    pub fn with_additional(origins: Vec<HttpOrigin>) -> SimpleClientResult<Self> {
        if origins.len() > 16 {
            return Err(SimpleClientError::InvalidConfiguration(
                "too many additional authentication origins",
            ));
        }
        Ok(Self {
            additional: origins,
        })
    }
}

/// Only a finite local lookup of already-resolved values. Remote refresh is an
/// app operation supervised separately by the explicit network policy/deadline.
pub trait SourceCredentials: Send + Sync {
    fn authorization_for(
        &self,
        reference: &CredentialRef,
        origin: &HttpOrigin,
        policy: NetworkPolicy,
    ) -> SimpleClientResult<Option<OriginAuthorization>>;
}

/// Pure explicit routing. The application adapter owns existing ordered
/// literal/regex semantics; no registry discovery or network is allowed here.
pub trait SourceRequestRouter: Send + Sync {
    fn fingerprint(&self) -> TransportRulesFingerprint;
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url>;
    /// Explicit application mapping for an otherwise ambiguous physical-only
    /// redirect. The client still validates the complete resulting route.
    /// This method must remain a pure finite configuration lookup.
    fn canonical_redirect(
        &self,
        _previous_canonical: &Url,
        _previous_request: &Url,
        _location: &str,
    ) -> SimpleClientResult<Option<Url>> {
        Ok(None)
    }
}

pub struct PythonSourceSession<'a> {
    pub registry: &'a PythonRegistry,
    pub auth_scope: &'a AuthScopeKey,
    pub credentials: &'a dyn SourceCredentials,
    pub auth_origins: &'a AuthOriginGrants,
    pub transport: &'a dyn SourceRequestRouter,
}

#[derive(Debug, Clone, Copy)]
pub struct SimpleFetchPolicy {
    pub network: NetworkPolicy,
    pub fresh_for: Duration,
    pub now: SystemTime,
    pub refresh: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectLookup {
    Present(SimpleProject),
    Missing404,
}

pub struct SimpleIndexClient {
    client: reqwest::Client,
    cache_root: PathBuf,
    config: SimpleHttpConfig,
}
impl SimpleIndexClient {
    pub fn new(cache_root: impl AsRef<Path>, config: SimpleHttpConfig) -> SimpleClientResult<Self> {
        validate_config(&config)?;
        let mut builder = reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(reqwest::header::HeaderMap::new())
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .http1_max_headers(1024);
        if !config.use_system_proxy {
            builder = builder.no_proxy();
        }
        if let Some(timeout) = config.read_timeout {
            builder = builder.read_timeout(timeout);
        }
        for proxy in &config.proxies {
            builder = builder.proxy(proxy.clone());
        }
        for certificate in &config.root_certificates {
            builder = builder.add_root_certificate(certificate.clone());
        }
        let client = builder.build().map_err(|_| {
            SimpleClientError::InvalidConfiguration("HTTP transport could not be constructed")
        })?;
        Ok(Self {
            client,
            cache_root: cache_root.as_ref().to_path_buf(),
            config,
        })
    }

    pub async fn project(
        &self,
        session: PythonSourceSession<'_>,
        project: &NormalizedPythonName,
        policy: SimpleFetchPolicy,
        limits: SimpleClientLimits,
    ) -> SimpleClientResult<ProjectLookup> {
        http::project(self, session, project, policy, limits, None).await
    }

    /// Observe a page within the earlier client and caller absolute budgets.
    /// Cache, routing, per-hop credentials and parsing share that same limit.
    pub async fn project_at_deadline(
        &self,
        session: PythonSourceSession<'_>,
        project: &NormalizedPythonName,
        policy: SimpleFetchPolicy,
        limits: SimpleClientLimits,
        caller_deadline: std::time::Instant,
    ) -> SimpleClientResult<ProjectLookup> {
        http::project(
            self,
            session,
            project,
            policy,
            limits,
            Some(caller_deadline),
        )
        .await
    }
}
impl fmt::Debug for SimpleIndexClient {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SimpleIndexClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

fn validate_config(config: &SimpleHttpConfig) -> SimpleClientResult<()> {
    if config.connect_timeout.is_zero()
        || config.read_timeout.is_some_and(|timeout| timeout.is_zero())
        || config.request_timeout.is_zero()
        || config.proxies.len() > 16
        || config.root_certificates.len() > 16
    {
        return Err(SimpleClientError::InvalidConfiguration(
            "timeouts must be positive and proxy/CA lists finite",
        ));
    }
    if config.use_system_proxy && !config.proxies.is_empty() {
        return Err(SimpleClientError::InvalidConfiguration(
            "system proxies and an explicit proxy list cannot be combined",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod auth_tests;

#[cfg(test)]
mod proxy_tests;
