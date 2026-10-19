//! Bounded pyproject declaration data, never a current-configuration proof.
//!
//! This projection neither evaluates markers nor authorizes source acquisition,
//! worker execution, graph publication or environment installation.

use pakro_index::python_requirements::{PythonRequirementLimits, PythonRequirementSyntax};
use pakro_index::python_source::NormalizedPythonName;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

pub(crate) type PythonDeclarationResult<T> = Result<T, PythonDeclarationError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonDeclarationLimit {
    DocumentBytes,
    Depth,
    Values,
    FieldBytes,
    Utf8Bytes,
    Requirements,
    Groups,
    Extras,
    GroupItems,
    BackendPaths,
    DynamicFields,
    ConfigEntries,
    ConfigValues,
    ExpandedRequirements,
    ExpansionWork,
    RequirementSyntax,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonDeclarationField {
    Project,
    Name,
    Version,
    RequiresPython,
    Dependencies,
    OptionalDependencies,
    DependencyGroups,
    Dynamic,
    BuildSystem,
    BuildRequires,
    Backend,
    BackendPath,
    ConfigSettings,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonDeclarationNamespace {
    Extra,
    Group,
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum PythonDeclarationError {
    #[error("Python declaration limits must be positive")]
    InvalidLimits,
    #[error("Python declaration budget exceeded: {0:?}")]
    Limit(PythonDeclarationLimit),
    #[error("invalid pyproject TOML document")]
    InvalidToml,
    #[error("invalid Python declaration field: {0:?}")]
    InvalidField(PythonDeclarationField),
    #[error("Python declaration names collide after normalization: {0:?}")]
    DuplicateName(PythonDeclarationNamespace),
    #[error("selected Python declaration name is absent: {0:?}")]
    MissingSelection(PythonDeclarationNamespace),
    #[error("Python declaration selection repeats a normalized name")]
    DuplicateSelection,
    #[error("Python dependency group contains an unsupported item")]
    InvalidGroupItem,
    #[error("Python dependency group includes a cycle")]
    GroupCycle,
    #[error("Python dependency declaration syntax is invalid")]
    InvalidRequirement,
    #[error("Python dynamic metadata conflicts with static metadata")]
    DynamicConflict,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PythonProjectLimits {
    pub max_document_bytes: usize,
    pub max_depth: usize,
    pub max_values: usize,
    pub max_field_bytes: usize,
    pub max_total_utf8_bytes: usize,
    pub max_requirements: usize,
    pub max_groups: usize,
    pub max_extras: usize,
    pub max_group_items: usize,
    pub max_backend_paths: usize,
    pub max_dynamic_fields: usize,
    pub max_config_entries: usize,
    pub max_config_values: usize,
    pub requirements: PythonRequirementLimits,
}
impl Default for PythonProjectLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 4 * 1024 * 1024,
            max_depth: 64,
            max_values: 262144,
            max_field_bytes: 128 * 1024,
            max_total_utf8_bytes: 8 * 1024 * 1024,
            max_requirements: 65536,
            max_groups: 4096,
            max_extras: 4096,
            max_group_items: 131072,
            max_backend_paths: 256,
            max_dynamic_fields: 64,
            max_config_entries: 4096,
            max_config_values: 16384,
            requirements: Default::default(),
        }
    }
}
impl PythonProjectLimits {
    pub(crate) fn validate(self) -> PythonDeclarationResult<()> {
        decode::validate(self)
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct PythonExpansionLimits {
    pub max_depth: usize,
    pub max_work: usize,
    pub max_expanded_requirements: usize,
    pub max_total_utf8_bytes: usize,
    pub max_selected_extras: usize,
    pub max_selected_groups: usize,
}
impl Default for PythonExpansionLimits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            max_work: 1048576,
            max_expanded_requirements: 131072,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            max_selected_extras: 4096,
            max_selected_groups: 4096,
        }
    }
}
pub(crate) struct PythonProjectSelection<'a> {
    pub include_project_dependencies: bool,
    pub extras: &'a [&'a str],
    pub groups: &'a [&'a str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PythonDeclarationOrigin {
    Project { index: usize },
    Extra { name: String, index: usize },
    Group { name: String, index: usize },
}
pub(crate) struct PythonDeclaredRequirement {
    syntax: Arc<PythonRequirementSyntax>,
    origin: PythonDeclarationOrigin,
}
impl PythonDeclaredRequirement {
    pub(crate) fn syntax(&self) -> &PythonRequirementSyntax {
        &self.syntax
    }
    pub(crate) fn origin(&self) -> &PythonDeclarationOrigin {
        &self.origin
    }
}
impl fmt::Debug for PythonDeclaredRequirement {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("PythonDeclaredRequirement")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}
pub(crate) struct PythonProjectMetadata {
    original_name: String,
    name: NormalizedPythonName,
    version: Option<String>,
    requires_python: Option<String>,
}
impl PythonProjectMetadata {
    pub(crate) fn original_name(&self) -> &str {
        &self.original_name
    }
    pub(crate) fn name(&self) -> &NormalizedPythonName {
        &self.name
    }
    pub(crate) fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
    pub(crate) fn requires_python(&self) -> Option<&str> {
        self.requires_python.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonBuildSystemOrigin {
    Declared,
    DefaultAbsentTable,
}
pub(crate) enum PythonBuildBackend {
    LegacyUnspecified,
    Explicit {
        module: String,
        object: Option<String>,
    },
}
impl PythonBuildBackend {
    pub(crate) fn module(&self) -> Option<&str> {
        match self {
            Self::LegacyUnspecified => None,
            Self::Explicit { module, .. } => Some(module),
        }
    }
    pub(crate) fn object(&self) -> Option<&str> {
        match self {
            Self::LegacyUnspecified => None,
            Self::Explicit { object, .. } => object.as_deref(),
        }
    }
}
impl fmt::Debug for PythonBuildBackend {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::LegacyUnspecified => "PythonBuildBackend::LegacyUnspecified",
            Self::Explicit { .. } => "PythonBuildBackend::Explicit(<data>)",
        })
    }
}
pub(crate) struct PythonBackendPath(String);
impl PythonBackendPath {
    /// An unresolved source-relative token. This is not containment evidence.
    pub(crate) fn raw_for_source_binding(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for PythonBackendPath {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("PythonBackendPath(<unresolved token>)")
    }
}
pub(crate) struct PythonBuildSystem {
    origin: PythonBuildSystemOrigin,
    requires: Vec<Arc<PythonRequirementSyntax>>,
    backend: PythonBuildBackend,
    backend_paths: Vec<PythonBackendPath>,
}
impl PythonBuildSystem {
    pub(crate) fn origin(&self) -> PythonBuildSystemOrigin {
        self.origin
    }
    pub(crate) fn requires(&self) -> impl Iterator<Item = &PythonRequirementSyntax> {
        self.requires.iter().map(Arc::as_ref)
    }
    pub(crate) fn backend(&self) -> &PythonBuildBackend {
        &self.backend
    }
    pub(crate) fn backend_paths(&self) -> &[PythonBackendPath] {
        &self.backend_paths
    }
}
struct OptionalExtra {
    original: String,
    requirements: Vec<Arc<PythonRequirementSyntax>>,
}
enum RawGroup {
    Items(Vec<RawGroupItem>),
    Invalid,
}
enum RawGroupItem {
    Requirement(String),
    Include(String),
    Invalid,
}
struct DependencyGroup {
    original: String,
    contents: RawGroup,
}

pub(crate) struct ParsedPythonProject {
    project: Option<PythonProjectMetadata>,
    dependencies: Vec<Arc<PythonRequirementSyntax>>,
    extras: BTreeMap<String, OptionalExtra>,
    groups: BTreeMap<String, DependencyGroup>,
    dynamic: Vec<String>,
    build: PythonBuildSystem,
    requirement_limits: PythonRequirementLimits,
}
impl ParsedPythonProject {
    pub(crate) fn project(&self) -> Option<&PythonProjectMetadata> {
        self.project.as_ref()
    }
    pub(crate) fn dynamic_fields(&self) -> &[String] {
        &self.dynamic
    }
    pub(crate) fn build_system(&self) -> &PythonBuildSystem {
        &self.build
    }
    pub(crate) fn optional_extras(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extras
            .iter()
            .map(|(normalized, value)| (value.original.as_str(), normalized.as_str()))
    }
    pub(crate) fn dependency_groups(&self) -> impl Iterator<Item = (&str, &str)> {
        self.groups
            .iter()
            .map(|(normalized, value)| (value.original.as_str(), normalized.as_str()))
    }
    pub(crate) fn select(
        &self,
        selection: PythonProjectSelection<'_>,
        limits: PythonExpansionLimits,
    ) -> PythonDeclarationResult<PythonDeclarationSelection> {
        selection::select(self, selection, limits)
    }
}
impl fmt::Debug for ParsedPythonProject {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ParsedPythonProject")
            .field("has_project", &self.project.is_some())
            .field("dependencies", &self.dependencies.len())
            .field("extras", &self.extras.len())
            .field("groups", &self.groups.len())
            .finish_non_exhaustive()
    }
}
#[path = "python_declarations/config_data.rs"]
mod config_data;
#[path = "python_declarations/decode.rs"]
mod decode;
#[path = "python_declarations/selection.rs"]
mod selection;
pub(crate) fn parse_pyproject(
    raw: &str,
    limits: PythonProjectLimits,
) -> PythonDeclarationResult<ParsedPythonProject> {
    decode::parse(raw, limits)
}

pub(crate) struct SelectedPythonDeclarations {
    requirements: Vec<PythonDeclaredRequirement>,
    extras: Vec<String>,
    groups: Vec<String>,
}
impl SelectedPythonDeclarations {
    pub(crate) fn requirements(&self) -> &[PythonDeclaredRequirement] {
        &self.requirements
    }
    pub(crate) fn extras(&self) -> &[String] {
        &self.extras
    }
    pub(crate) fn groups(&self) -> &[String] {
        &self.groups
    }
}
impl fmt::Debug for SelectedPythonDeclarations {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("SelectedPythonDeclarations")
            .field("requirements", &self.requirements.len())
            .field("extra_count", &self.extras.len())
            .field("group_count", &self.groups.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PythonMetadataWorkerReason {
    MissingProject,
    DynamicMetadata,
}
pub(crate) struct PythonMetadataWorkerNeed {
    reason: PythonMetadataWorkerReason,
    fields: Vec<String>,
}
impl PythonMetadataWorkerNeed {
    pub(crate) fn reason(&self) -> PythonMetadataWorkerReason {
        self.reason
    }
    pub(crate) fn fields(&self) -> &[String] {
        &self.fields
    }
}
pub(crate) enum PythonDeclarationSelection {
    Static(SelectedPythonDeclarations),
    NeedsMetadataWorker {
        known: SelectedPythonDeclarations,
        need: PythonMetadataWorkerNeed,
    },
}
impl PythonDeclarationSelection {
    pub(crate) fn known(&self) -> &SelectedPythonDeclarations {
        match self {
            Self::Static(value) => value,
            Self::NeedsMetadataWorker { known, .. } => known,
        }
    }
    pub(crate) fn metadata_need(&self) -> Option<&PythonMetadataWorkerNeed> {
        match self {
            Self::Static(_) => None,
            Self::NeedsMetadataWorker { need, .. } => Some(need),
        }
    }
}
impl fmt::Debug for PythonDeclarationSelection {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Static(_) => "PythonDeclarationSelection::Static",
            Self::NeedsMetadataWorker { .. } => "PythonDeclarationSelection::NeedsMetadataWorker",
        })
    }
}

pub(crate) enum PythonBuildConfigValue<'a> {
    String(&'a str),
    Strings(&'a [&'a str]),
}
enum OwnedBuildConfigValue {
    String(String),
    Strings(Vec<String>),
}
pub(crate) enum PythonBuildConfigValueRef<'a> {
    String(&'a str),
    Strings(&'a [String]),
}
pub(crate) struct PythonBuildConfigData {
    values: BTreeMap<String, OwnedBuildConfigValue>,
}
impl PythonBuildConfigData {
    pub(crate) fn from_entries(
        entries: &[(&str, PythonBuildConfigValue<'_>)],
        limits: PythonProjectLimits,
    ) -> PythonDeclarationResult<Self> {
        config_data::parse(entries, limits)
    }
    pub(crate) fn get(&self, key: &str) -> Option<PythonBuildConfigValueRef<'_>> {
        self.values.get(key).map(|value| match value {
            OwnedBuildConfigValue::String(value) => PythonBuildConfigValueRef::String(value),
            OwnedBuildConfigValue::Strings(values) => PythonBuildConfigValueRef::Strings(values),
        })
    }
}
impl fmt::Debug for PythonBuildConfigData {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("PythonBuildConfigData")
            .field("keys", &self.values.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "python_declarations/tests.rs"]
mod tests;
