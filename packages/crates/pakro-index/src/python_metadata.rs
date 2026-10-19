//! Unfiltered Python dependency declarations projected from parsed Core Metadata.
//!
//! These facts neither select a source nor grant artifact, resolver or write
//! authority. Declarations retain exact marker syntax; effective dependencies
//! still require an explicit context and versioned marker evaluation.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use pep508_rs::MarkerWarningKind;
use thiserror::Error;

pub use crate::python_requirements::UnresolvedDependencyReference;
use crate::python_requirements::{PythonRequirementSyntax, parse_python_requirement};
use crate::python_source::NormalizedPythonName;

pub type PythonMetadataResult<T> = Result<T, PythonMetadataError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMetadataField {
    Name,
    Version,
    MetadataVersion,
    RequiresPython,
    RequiresDist,
    ProvidesExtra,
    Dynamic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMetadataLimitKind {
    TotalBytes,
    FieldBytes,
    Requirements,
    ProvidedExtras,
    RequestedExtras,
    DynamicFields,
    MarkerDepth,
    MarkerTerms,
    SyntaxTokens,
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PythonMetadataError {
    #[error("Python metadata limits must be positive")]
    InvalidLimits,
    #[error("Python metadata limit exceeded: {0:?}")]
    LimitExceeded(PythonMetadataLimitKind),
    #[error("invalid Python metadata field {field:?} at item {index:?}")]
    InvalidField {
        field: PythonMetadataField,
        index: Option<usize>,
    },
    #[error("Python metadata identity differs from the selected file")]
    IdentityMismatch,
    #[error("unsupported Python metadata major version: {0}")]
    UnsupportedMajor(u32),
    #[error("Python metadata projection is not implemented")]
    NotImplemented,
}

/// Borrowed values from the existing bounded header parser, not a second
/// RFC822 parser. Its Debug implementation never includes raw declarations.
#[derive(Clone, Copy)]
pub struct PythonMetadataFields<'a> {
    pub metadata_version: (u32, u32),
    pub name: &'a str,
    pub version: &'a str,
    pub requires_python: Option<&'a str>,
    pub requires_dist: &'a [String],
    pub provides_extra: &'a [String],
    pub dynamic: &'a [String],
}
impl fmt::Debug for PythonMetadataFields<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonMetadataFields")
            .field("metadata_version", &self.metadata_version)
            .field("requirements", &self.requires_dist.len())
            .field("extras", &self.provides_extra.len())
            .field("dynamic_fields", &self.dynamic.len())
            .finish_non_exhaustive()
    }
}
pub struct PythonMetadataExpectation<'a> {
    pub name: &'a NormalizedPythonName,
    pub version: &'a str,
}
#[derive(Debug, Clone, Copy)]
pub struct PythonMetadataLimits {
    pub max_total_bytes: usize,
    pub max_field_bytes: usize,
    pub max_requirements: usize,
    pub max_provided_extras: usize,
    pub max_requested_extras: usize,
    pub max_dynamic_fields: usize,
    pub max_marker_depth: usize,
    pub max_marker_terms: usize,
    pub max_syntax_tokens: usize,
}
impl Default for PythonMetadataLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 2 * 1024 * 1024,
            max_field_bytes: 16 * 1024,
            max_requirements: 1024,
            max_provided_extras: 1024,
            max_requested_extras: 128,
            max_dynamic_fields: 128,
            max_marker_depth: 32,
            max_marker_terms: 8,
            max_syntax_tokens: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMarkerSemantics {
    Unconditional,
    NeedsExactMarkerSemantics,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdistDynamicPolicy {
    LegacyUnspecified,
    ReplaceMarkedFields,
    AppendMarkedMultipleUseFields,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonMetadataWarning {
    NewerMetadataMinor {
        major: u32,
        minor: u32,
    },
    LegacyExtraNormalized {
        index: usize,
    },
    DynamicBeforeMetadata22 {
        index: usize,
    },
    MarkerSyntax {
        index: usize,
        kind: MarkerWarningKind,
    },
}

pub struct PythonRequirementDeclaration {
    syntax: PythonRequirementSyntax,
    semantics: PythonMarkerSemantics,
}
impl PythonRequirementDeclaration {
    pub fn name(&self) -> &str {
        self.syntax.name()
    }
    pub fn requested_extras(&self) -> impl Iterator<Item = &str> {
        self.syntax.requested_extras()
    }
    pub fn unresolved_reference(&self) -> Option<&UnresolvedDependencyReference> {
        self.syntax.unresolved_reference()
    }
    pub fn version_specifiers(&self) -> Option<&pep440_rs::VersionSpecifiers> {
        self.syntax.version_specifiers()
    }
    pub fn raw_version_specifiers(&self) -> Option<&str> {
        self.syntax.raw_version_specifiers()
    }
    pub fn marker_semantics(&self) -> PythonMarkerSemantics {
        self.semantics
    }
    pub fn exact_marker(&self) -> Option<&crate::python_markers::PythonMarkerExpression> {
        self.syntax.marker()
    }
    /// Original declaration for the explicit exact-semantics adapter. Neither
    /// Display nor a normalized library tree may substitute for this input.
    pub fn raw_for_semantic_resolution(&self) -> &str {
        self.syntax.raw_for_semantic_resolution()
    }
}
impl fmt::Debug for PythonRequirementDeclaration {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonRequirementDeclaration")
            .field("name", &self.name())
            .field("requested_extras", &self.syntax.requested_extras().count())
            .field("direct_reference", &self.unresolved_reference().is_some())
            .field("marker_semantics", &self.semantics)
            .finish_non_exhaustive()
    }
}

pub struct PythonDependencyMetadataFacts {
    name: NormalizedPythonName,
    version: String,
    metadata_version: (u32, u32),
    requires_python: Option<String>,
    requirements: Vec<PythonRequirementDeclaration>,
    extras: Vec<String>,
    dynamic: Vec<String>,
    dynamic_policy: SdistDynamicPolicy,
    warnings: Vec<PythonMetadataWarning>,
}
impl PythonDependencyMetadataFacts {
    pub fn name(&self) -> &NormalizedPythonName {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn metadata_version(&self) -> (u32, u32) {
        self.metadata_version
    }
    pub fn requires_python(&self) -> Option<&str> {
        self.requires_python.as_deref()
    }
    pub fn requirements(&self) -> &[PythonRequirementDeclaration] {
        &self.requirements
    }
    pub fn provided_extras(&self) -> &[String] {
        &self.extras
    }
    pub fn dynamic_fields(&self) -> &[String] {
        &self.dynamic
    }
    /// Only the later sdist-to-wheel transition validator applies this rule.
    /// It never makes any already-observed wheel field ignorable.
    pub fn sdist_dynamic_policy(&self) -> SdistDynamicPolicy {
        self.dynamic_policy
    }
    pub fn warnings(&self) -> &[PythonMetadataWarning] {
        &self.warnings
    }
}
impl fmt::Debug for PythonDependencyMetadataFacts {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonDependencyMetadataFacts")
            .field("name", &self.name)
            .field("metadata_version", &self.metadata_version)
            .field("requirements", &self.requirements.len())
            .field("extras", &self.extras.len())
            .field("dynamic_fields", &self.dynamic.len())
            .field("warning_count", &self.warnings.len())
            .finish_non_exhaustive()
    }
}

mod preflight;

fn invalid_field(field: PythonMetadataField, index: Option<usize>) -> PythonMetadataError {
    PythonMetadataError::InvalidField { field, index }
}

pub fn project_python_metadata(
    fields: PythonMetadataFields<'_>,
    expected: PythonMetadataExpectation<'_>,
    limits: PythonMetadataLimits,
) -> PythonMetadataResult<PythonDependencyMetadataFacts> {
    preflight::inputs(fields, &expected, limits)?;
    let (major, minor) = fields.metadata_version;
    if !matches!(major, 1 | 2) {
        return Err(PythonMetadataError::UnsupportedMajor(major));
    }
    if (major == 1 && minor > 2) || (major == 2 && minor == 0) {
        return Err(invalid_field(PythonMetadataField::MetadataVersion, None));
    }
    let mut warnings = Vec::new();
    if major == 2 && minor > 6 {
        warnings.push(PythonMetadataWarning::NewerMetadataMinor { major, minor });
    }
    let package = pep508_rs::PackageName::from_str(fields.name)
        .map_err(|_| invalid_field(PythonMetadataField::Name, None))?;
    let name = NormalizedPythonName::new(package.as_ref())
        .map_err(|_| invalid_field(PythonMetadataField::Name, None))?;
    let version = pep440_rs::Version::from_str(fields.version)
        .map_err(|_| invalid_field(PythonMetadataField::Version, None))?;
    let expected_version = pep440_rs::Version::from_str(expected.version)
        .map_err(|_| invalid_field(PythonMetadataField::Version, None))?;
    if &name != expected.name || version != expected_version {
        return Err(PythonMetadataError::IdentityMismatch);
    }
    if let Some(value) = fields.requires_python {
        if value.is_empty() || pep440_rs::VersionSpecifiers::from_str(value).is_err() {
            return Err(invalid_field(PythonMetadataField::RequiresPython, None));
        }
    }

    // Run every raw requirement budget before parsing any declaration into
    // third-party syntax/BDD nodes or cloning collected facts.
    for (index, raw) in fields.requires_dist.iter().enumerate() {
        preflight::requirement(raw, index, limits)?;
    }
    let mut extras = Vec::with_capacity(fields.provides_extra.len());
    let mut seen = BTreeSet::new();
    for (index, raw) in fields.provides_extra.iter().enumerate() {
        let extra = pep508_rs::ExtraName::from_str(raw)
            .map_err(|_| invalid_field(PythonMetadataField::ProvidesExtra, Some(index)))?;
        let extra = extra.to_string();
        if raw != &extra {
            if fields.metadata_version >= (2, 3) {
                return Err(invalid_field(
                    PythonMetadataField::ProvidesExtra,
                    Some(index),
                ));
            }
            warnings.push(PythonMetadataWarning::LegacyExtraNormalized { index });
        }
        if !seen.insert(extra.clone()) {
            return Err(invalid_field(
                PythonMetadataField::ProvidesExtra,
                Some(index),
            ));
        }
        extras.push(extra);
    }
    let mut dynamic = Vec::with_capacity(fields.dynamic.len());
    let mut seen = BTreeSet::new();
    for (index, raw) in fields.dynamic.iter().enumerate() {
        if raw.is_empty()
            || !raw
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'-')
        {
            return Err(invalid_field(PythonMetadataField::Dynamic, Some(index)));
        }
        let field = raw.to_ascii_lowercase();
        if matches!(field.as_str(), "name" | "version" | "metadata-version")
            || !seen.insert(field.clone())
        {
            return Err(invalid_field(PythonMetadataField::Dynamic, Some(index)));
        }
        if fields.metadata_version < (2, 2) {
            warnings.push(PythonMetadataWarning::DynamicBeforeMetadata22 { index });
        }
        dynamic.push(field);
    }
    let mut requirements = Vec::with_capacity(fields.requires_dist.len());
    for (index, raw) in fields.requires_dist.iter().enumerate() {
        let syntax = parse_python_requirement(raw, preflight::declaration_limits(limits)?)
            .map_err(|error| preflight::declaration_error(error, index))?;
        let semantics = if syntax.marker().is_some() {
            // Compatibility status: exact syntax is retained, while its
            // effective dependencies still need explicit context evaluation.
            PythonMarkerSemantics::NeedsExactMarkerSemantics
        } else {
            PythonMarkerSemantics::Unconditional
        };
        requirements.push(PythonRequirementDeclaration { syntax, semantics });
    }
    let dynamic_policy = if fields.metadata_version < (2, 2) {
        SdistDynamicPolicy::LegacyUnspecified
    } else if fields.metadata_version < (2, 6) {
        SdistDynamicPolicy::ReplaceMarkedFields
    } else {
        SdistDynamicPolicy::AppendMarkedMultipleUseFields
    };
    Ok(PythonDependencyMetadataFacts {
        name,
        version: version.to_string(),
        metadata_version: fields.metadata_version,
        requires_python: fields.requires_python.map(str::to_owned),
        requirements,
        extras,
        dynamic,
        dynamic_policy,
        warnings,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "python_requirements/metadata_tests.rs"]
mod exact_requirement_tests;
