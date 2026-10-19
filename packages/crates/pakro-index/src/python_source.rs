//! Explicit Python registry identity and project-source binding.
//!
//! Credentials remain application-owned. This module consumes only an opaque
//! local auth revision and never discovers settings, credentials or targets.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use reqwest::Url;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub type PythonSourceResult<T> = Result<T, PythonSourceError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupFailureKind {
    Authentication,
    Transport,
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PythonSourceError {
    #[error("invalid Python source configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("Python source lookup failed: {0:?}")]
    LookupFailed(LookupFailureKind),
    #[error("Python project is missing from its configured source")]
    ProjectMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PythonRegistryId(String);

impl PythonRegistryId {
    pub fn new(value: &str) -> PythonSourceResult<Self> {
        validate_name_atom(value)?;
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new(value: &str) -> PythonSourceResult<Self> {
        validate_name_atom(value)?;
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("CredentialRef(<application reference>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedPythonName(String);

impl NormalizedPythonName {
    pub fn new(value: &str) -> PythonSourceResult<Self> {
        validate_name_atom(value)?;
        Ok(Self(crate::normalize_py_name(value)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CanonicalRegistryUrl(Url);

impl CanonicalRegistryUrl {
    pub fn new(value: &str) -> PythonSourceResult<Self> {
        let invalid = || {
            PythonSourceError::InvalidConfiguration(
                "expected a credential-free HTTP(S) registry URL",
            )
        };
        // URL parsing can trim whitespace and discard an empty userinfo. Reject
        // that input explicitly instead of guessing a new canonical source.
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(invalid());
        }
        let mut url = Url::parse(value).map_err(|_| invalid())?;
        let authority_has_userinfo = value.split_once(':').is_some_and(|(_, rest)| {
            rest.trim_start_matches(['/', '\\'])
                .split(['/', '\\', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || authority_has_userinfo
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        if !url.path().ends_with('/') {
            url.path_segments_mut().map_err(|_| invalid())?.push("");
        }
        Ok(Self(url))
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }
}

impl fmt::Debug for CanonicalRegistryUrl {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("CanonicalRegistryUrl(<canonical URI>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CanonicalProjectUrl(Url);

impl CanonicalProjectUrl {
    pub fn as_url(&self) -> &Url {
        &self.0
    }
}

impl fmt::Debug for CanonicalProjectUrl {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("CanonicalProjectUrl(<canonical URI>)")
    }
}

/// A local principal/permission revision, supplied by the application.
///
/// No secret-to-hash conversion or portable serialization is provided. The
/// application must change this revision when its effective auth scope changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuthScopeKey([u8; 32]);

impl AuthScopeKey {
    pub fn from_local_revision(revision: [u8; 32]) -> Self {
        Self(revision)
    }
}

impl fmt::Debug for AuthScopeKey {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("AuthScopeKey(<local-only revision>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceCacheKey([u8; 32]);

impl SourceCacheKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportRulesFingerprint([u8; 32]);

/// Fingerprint ordered, borrowed configuration. None and an empty slice have
/// identical semantics. Patterns/replacements are not retained or logged.
pub fn transport_rules_fingerprint(rules: Option<&[(&str, &str)]>) -> TransportRulesFingerprint {
    let mut hash = Sha256::new();
    hash_field(&mut hash, b"pakro.python-source-transport/v1");
    for (pattern, replacement) in rules.unwrap_or_default() {
        hash_field(&mut hash, pattern.as_bytes());
        hash_field(&mut hash, replacement.as_bytes());
    }
    TransportRulesFingerprint(hash.finalize().into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleRepresentation {
    JsonV1,
    HtmlV1,
    LegacyHtml,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PythonRegistry {
    id: PythonRegistryId,
    canonical_base: CanonicalRegistryUrl,
    credential_ref: Option<CredentialRef>,
}

impl PythonRegistry {
    pub fn new(
        id: &str,
        canonical_base: &str,
        credential_ref: Option<CredentialRef>,
    ) -> PythonSourceResult<Self> {
        Ok(Self {
            id: PythonRegistryId::new(id)?,
            canonical_base: CanonicalRegistryUrl::new(canonical_base)?,
            credential_ref,
        })
    }

    pub fn id(&self) -> &PythonRegistryId {
        &self.id
    }

    pub fn canonical_base(&self) -> &CanonicalRegistryUrl {
        &self.canonical_base
    }

    pub fn credential_ref(&self) -> Option<&CredentialRef> {
        self.credential_ref.as_ref()
    }

    pub fn project_url(
        &self,
        project: &NormalizedPythonName,
    ) -> PythonSourceResult<CanonicalProjectUrl> {
        self.canonical_base
            .as_url()
            .join(&format!("{}/", project.as_str()))
            .map(CanonicalProjectUrl)
            .map_err(|_| PythonSourceError::InvalidConfiguration("invalid Python project URL"))
    }

    pub fn cache_key(
        &self,
        auth_scope: &AuthScopeKey,
        transport: &TransportRulesFingerprint,
        representation: SimpleRepresentation,
    ) -> SourceCacheKey {
        let mut hash = Sha256::new();
        hash_field(&mut hash, b"pakro.python-source-cache/v1");
        hash_field(&mut hash, self.id.as_str().as_bytes());
        hash_field(&mut hash, self.canonical_base.as_url().as_str().as_bytes());
        match &self.credential_ref {
            Some(reference) => {
                hash_field(&mut hash, b"ref");
                hash_field(&mut hash, reference.as_str().as_bytes());
            }
            None => {
                hash_field(&mut hash, b"none");
                hash_field(&mut hash, b"");
            }
        }
        hash_field(&mut hash, &auth_scope.0);
        hash_field(&mut hash, &transport.0);
        hash_field(
            &mut hash,
            match representation {
                SimpleRepresentation::JsonV1 => b"json-v1",
                SimpleRepresentation::HtmlV1 => b"html-v1",
                SimpleRepresentation::LegacyHtml => b"legacy-html",
            },
        );
        SourceCacheKey(hash.finalize().into())
    }
}

impl fmt::Debug for PythonRegistry {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonRegistry")
            .field("id", &self.id)
            .field("has_credential_ref", &self.credential_ref.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonSourcePolicy {
    Explicit(PythonRegistryId),
    FirstProject(Vec<PythonRegistryId>),
}

impl Default for PythonSourcePolicy {
    fn default() -> Self {
        Self::Explicit(PythonRegistryId("pypi".to_owned()))
    }
}

/// Only an actual project-404 may be converted to Missing404. An empty project
/// page, no compatible versions/files, authentication or protocol errors cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPresence {
    Present,
    Missing404,
}

/// Validate the complete finite source configuration and return its explicit
/// lookup order. These indices are data, never project availability or binding.
pub fn ordered_python_source_indices(
    registries: &[&PythonRegistry],
    policy: &PythonSourcePolicy,
) -> PythonSourceResult<Vec<usize>> {
    let mut available = BTreeMap::new();
    for (index, registry) in registries.iter().enumerate() {
        if available.insert(registry.id.as_str(), index).is_some() {
            return Err(PythonSourceError::InvalidConfiguration(
                "duplicate registry id",
            ));
        }
    }
    let ids = match policy {
        PythonSourcePolicy::Explicit(id) => std::slice::from_ref(id),
        PythonSourcePolicy::FirstProject(ids) => ids.as_slice(),
    };
    if ids.is_empty() {
        return Err(PythonSourceError::InvalidConfiguration(
            "source policy is empty",
        ));
    }
    let mut selected = BTreeSet::new();
    let mut order = Vec::new();
    order.try_reserve_exact(ids.len()).map_err(|_| {
        PythonSourceError::InvalidConfiguration("source order exceeds allocation limits")
    })?;
    for id in ids {
        if !selected.insert(id.as_str()) {
            return Err(PythonSourceError::InvalidConfiguration(
                "source policy repeats a registry id",
            ));
        }
        order.push(
            *available
                .get(id.as_str())
                .ok_or(PythonSourceError::InvalidConfiguration(
                    "unknown registry id",
                ))?,
        );
    }
    Ok(order)
}

#[derive(Debug)]
pub struct PythonSourceBinding<'a> {
    registry: &'a PythonRegistry,
    project: NormalizedPythonName,
}

impl<'a> PythonSourceBinding<'a> {
    pub fn registry(&self) -> &'a PythonRegistry {
        self.registry
    }

    pub fn project(&self) -> &NormalizedPythonName {
        &self.project
    }
}

/// Bind one source before any version, Requires-Python or ABI filtering.
pub fn bind_python_source<'a>(
    registries: &'a [PythonRegistry],
    policy: &PythonSourcePolicy,
    project: &NormalizedPythonName,
    mut lookup: impl FnMut(
        &PythonRegistry,
        &NormalizedPythonName,
    ) -> PythonSourceResult<ProjectPresence>,
) -> PythonSourceResult<PythonSourceBinding<'a>> {
    let references: Vec<_> = registries.iter().collect();
    let order = ordered_python_source_indices(&references, policy)?;
    // The complete policy is validated before the first lookup. Only a typed
    // Missing404 advances; every error and every Present ends this search.
    for index in order {
        let registry = &registries[index];
        match lookup(registry, project)? {
            ProjectPresence::Present => {
                return Ok(PythonSourceBinding {
                    registry,
                    project: project.clone(),
                });
            }
            ProjectPresence::Missing404 => {}
        }
    }
    Err(PythonSourceError::ProjectMissing)
}

fn validate_name_atom(value: &str) -> PythonSourceResult<()> {
    if value
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
    {
        Ok(())
    } else {
        Err(PythonSourceError::InvalidConfiguration(
            "expected an ASCII identifier with alphanumeric boundaries",
        ))
    }
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    // Supported Rust targets have <=64-bit slice lengths; this conversion can
    // never truncate a field in the versioned 64-bit encoding.
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

#[cfg(test)]
#[path = "python_source/tests.rs"]
mod tests;
