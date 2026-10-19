//! Simple repository file facts from explicit source/document inputs.
//!
//! These values do not resolve dependencies, filter by a target, authenticate,
//! fetch metadata sidecars or authorize a missing-hash artifact for installation.

use std::collections::BTreeMap;
use std::fmt;

use pakro_store::Digest;
use reqwest::Url;
use thiserror::Error;

use crate::WheelFile;
use crate::python_source::{
    CanonicalProjectUrl, NormalizedPythonName, PythonRegistry, PythonRegistryId,
};

mod client;
pub use client::{
    AuthOriginGrants, CompletedSourceTransfer, HttpOrigin, OriginAuthorization, ProjectLookup,
    PythonSourceSession, SimpleClientError, SimpleClientLimitKind, SimpleClientLimits,
    SimpleClientResult, SimpleFetchPolicy, SimpleHttpConfig, SimpleIndexClient, SourceCredentials,
    SourceFileLimits, SourceFileSession, SourceRequestRouter,
};

mod facts;
mod html;
pub use html::{SimpleHtmlLimits, parse_simple_html};
mod json;
pub use json::parse_simple_json;

#[cfg(test)]
mod html_tests;

pub type SimpleResult<T> = Result<T, SimpleError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleLimitKind {
    ResponseBytes,
    Files,
    Versions,
    Hashes,
    Relations,
    FieldBytes,
    ObjectFields,
    JsonDepth,
    JsonValues,
    HtmlTokens,
    HtmlTreeNodes,
    HtmlTreeDepth,
    HtmlTreeTextBytes,
    HtmlTreeOperations,
    HtmlAttributes,
    HtmlAnchors,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SimpleError {
    #[error("Simple API limits must be nonzero")]
    InvalidLimits,
    #[error("Simple API limit exceeded: {0:?}")]
    LimitExceeded(SimpleLimitKind),
    #[error("Simple API document does not match its explicit source context")]
    InvalidContext,
    #[error("invalid Simple API JSON document")]
    InvalidJson,
    #[error("invalid Simple API response: {0}")]
    InvalidResponse(&'static str),
    #[error("unsupported Simple API major version {0}")]
    UnsupportedApiMajor(u32),
    #[error("conflicting Simple API facts for one file")]
    ConflictingFile,
}

#[derive(Debug, Clone, Copy)]
pub struct SimpleLimits {
    pub max_response_bytes: u64,
    pub max_files: usize,
    pub max_versions: usize,
    pub max_hashes_per_file: usize,
    pub max_relations: usize,
    pub max_field_bytes: usize,
    pub max_object_fields: usize,
    pub max_json_depth: usize,
    pub max_json_values: usize,
}

impl Default for SimpleLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: 64 * 1024 * 1024,
            max_files: 262_144,
            max_versions: 65_536,
            max_hashes_per_file: 16,
            max_relations: 256,
            max_field_bytes: 128 * 1024,
            max_object_fields: 128,
            max_json_depth: 64,
            max_json_values: 4_000_000,
        }
    }
}

pub struct SimpleParseContext<'a> {
    pub registry: &'a PythonRegistry,
    pub project: &'a NormalizedPythonName,
    pub canonical_document_url: &'a CanonicalProjectUrl,
    pub limits: SimpleLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimpleApiVersion {
    major: u32,
    minor: u32,
}

impl SimpleApiVersion {
    pub fn major(&self) -> u32 {
        self.major
    }
    pub fn minor(&self) -> u32 {
        self.minor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleWarning {
    NewerApiMinor { major: u32, minor: u32 },
    UnknownProjectStatus,
    HistoricalDistribution,
}

/// A request locator, not a promise that its query is safe for a portable lock.
/// It deliberately has no Serialize or Display implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct FileLocator {
    url: Url,
}

impl FileLocator {
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn is_ephemeral(&self) -> bool {
        self.url.query().is_some()
    }

    /// Append the sidecar suffix to the path, retaining the request query.
    pub fn metadata_locator(&self) -> SimpleResult<Self> {
        let mut url = self.url.clone();
        url.set_path(&format!("{}.metadata", self.url.path()));
        Ok(Self { url })
    }
}

impl fmt::Debug for FileLocator {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("FileLocator")
            .field("ephemeral", &self.is_ephemeral())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AdvertisedHashes {
    values: BTreeMap<String, String>,
    preferred: Option<Digest>,
}

impl AdvertisedHashes {
    /// Complete retained protocol facts, including unknown algorithms. These
    /// strings are data and must not be used as log or authority values.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
    pub fn get(&self, algorithm: &str) -> Option<&str> {
        self.values.get(algorithm).map(String::as_str)
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn preferred_digest(&self) -> Option<Digest> {
        self.preferred
    }
}

impl fmt::Debug for AdvertisedHashes {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("AdvertisedHashes")
            .field("count", &self.values.len())
            .field("has_supported_digest", &self.preferred.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Yanked {
    No,
    Yes { reason: Option<String> },
}

impl Yanked {
    pub fn is_yanked(&self) -> bool {
        matches!(self, Self::Yes { .. })
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::No => None,
            Self::Yes { reason } => reason.as_deref(),
        }
    }
}

impl fmt::Debug for Yanked {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Yanked")
            .field("yanked", &self.is_yanked())
            .field("has_reason", &self.reason().is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreMetadataAvailability {
    Unspecified,
    Absent,
    Available { hashes: AdvertisedHashes },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonDistributionKind {
    Wheel(WheelFile),
    Sdist,
    HistoricalFormat,
    Other,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SimpleFile {
    filename: String,
    locator: FileLocator,
    kind: PythonDistributionKind,
    version: Option<String>,
    hashes: AdvertisedHashes,
    requires_python: Option<String>,
    yanked: Yanked,
    core_metadata: CoreMetadataAvailability,
    size: Option<u64>,
    upload_time: Option<String>,
    provenance: Option<FileLocator>,
}

impl SimpleFile {
    pub fn filename(&self) -> &str {
        &self.filename
    }
    pub fn locator(&self) -> &FileLocator {
        &self.locator
    }
    pub fn kind(&self) -> &PythonDistributionKind {
        &self.kind
    }
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
    pub fn hashes(&self) -> &AdvertisedHashes {
        &self.hashes
    }
    pub fn requires_python(&self) -> Option<&str> {
        self.requires_python.as_deref()
    }
    pub fn yanked(&self) -> &Yanked {
        &self.yanked
    }
    pub fn core_metadata(&self) -> &CoreMetadataAvailability {
        &self.core_metadata
    }
    pub fn size(&self) -> Option<u64> {
        self.size
    }
    pub fn upload_time(&self) -> Option<&str> {
        self.upload_time.as_deref()
    }
    pub fn provenance(&self) -> Option<&FileLocator> {
        self.provenance.as_ref()
    }
}

impl fmt::Debug for SimpleFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SimpleFile")
            .field("filename", &self.filename)
            .field("kind", &self.kind)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatusKind {
    Active,
    Archived,
    Deprecated,
    Quarantined,
    Unknown,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectStatus {
    kind: ProjectStatusKind,
    raw: String,
    reason: Option<String>,
}

impl ProjectStatus {
    pub fn kind(&self) -> ProjectStatusKind {
        self.kind
    }
    pub fn raw(&self) -> &str {
        &self.raw
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

impl fmt::Debug for ProjectStatus {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ProjectStatus")
            .field("kind", &self.kind)
            .field("has_reason", &self.reason.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryRelations {
    tracks: Vec<FileLocator>,
    alternate_locations: Vec<FileLocator>,
}

impl RepositoryRelations {
    pub fn tracks(&self) -> &[FileLocator] {
        &self.tracks
    }
    pub fn alternate_locations(&self) -> &[FileLocator] {
        &self.alternate_locations
    }
}

impl fmt::Debug for RepositoryRelations {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("RepositoryRelations")
            .field("tracks_count", &self.tracks.len())
            .field("alternate_count", &self.alternate_locations.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SimpleProject {
    project: NormalizedPythonName,
    source: PythonRegistryId,
    api_version: SimpleApiVersion,
    versions: Vec<String>,
    files: Vec<SimpleFile>,
    relations: RepositoryRelations,
    status: Option<ProjectStatus>,
    warnings: Vec<SimpleWarning>,
}

impl SimpleProject {
    pub fn name(&self) -> &NormalizedPythonName {
        &self.project
    }
    pub fn source(&self) -> &PythonRegistryId {
        &self.source
    }
    pub fn api_version(&self) -> SimpleApiVersion {
        self.api_version
    }
    pub fn versions(&self) -> &[String] {
        &self.versions
    }
    pub fn files(&self) -> &[SimpleFile] {
        &self.files
    }
    pub fn relations(&self) -> &RepositoryRelations {
        &self.relations
    }
    pub fn status(&self) -> Option<&ProjectStatus> {
        self.status.as_ref()
    }
    pub fn warnings(&self) -> &[SimpleWarning] {
        &self.warnings
    }
}

impl fmt::Debug for SimpleProject {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SimpleProject")
            .field("project", &self.project)
            .field("source", &self.source)
            .field("api_version", &self.api_version)
            .field("files_count", &self.files.len())
            .field("versions_count", &self.versions.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod json_tests;
