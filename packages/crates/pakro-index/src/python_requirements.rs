//! Complete unresolved PEP508 declarations and exact marker syntax.
//!
//! Source tokens remain facts, never expanded paths or download authority.

use std::fmt;
use std::ops::Range;
use std::path::Path;

use pep508_rs::{Pep508Url, Requirement, VersionOrUrl};
use thiserror::Error;

use crate::python_markers::{
    PythonMarkerError, PythonMarkerExpression, PythonMarkerLimits, parse_python_marker,
};

mod spans;

pub type PythonRequirementResult<T> = Result<T, PythonRequirementError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonRequirementLimitKind {
    InputBytes,
    RequestedExtras,
    SyntaxTokens,
    MarkerComparisons,
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PythonRequirementError {
    #[error("requirement limits must be positive")]
    InvalidLimits,
    #[error("requirement limit exceeded: {0:?}")]
    LimitExceeded(PythonRequirementLimitKind),
    #[error("invalid requirement syntax at byte {offset}")]
    Syntax { offset: usize },
    #[error("requirement marker is invalid: {0}")]
    Marker(PythonMarkerError),
}

/// Unresolved declaration token: never a canonical source, expanded path or
/// request authorization. Display deliberately cannot serve as identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnresolvedDependencyReference(String);
impl UnresolvedDependencyReference {
    pub fn raw_for_source_binding(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for UnresolvedDependencyReference {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("UnresolvedDependencyReference(<private token>)")
    }
}
impl fmt::Display for UnresolvedDependencyReference {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("<unresolved dependency reference>")
    }
}
impl Pep508Url for UnresolvedDependencyReference {
    type Err = PythonRequirementError;
    fn parse_url(url: &str, _working_dir: Option<&Path>) -> PythonRequirementResult<Self> {
        if url.is_empty()
            || url
                .chars()
                .any(|value| value.is_control() || value.is_whitespace())
        {
            return Err(PythonRequirementError::Syntax { offset: 0 });
        }
        Ok(Self(url.to_owned()))
    }
}
#[derive(Debug, Clone, Copy)]
pub struct PythonRequirementLimits {
    pub max_input_bytes: usize,
    pub max_requested_extras: usize,
    pub max_syntax_tokens: usize,
    pub max_marker_comparisons: usize,
    pub marker: PythonMarkerLimits,
}
impl Default for PythonRequirementLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024,
            max_requested_extras: 128,
            max_syntax_tokens: 256,
            max_marker_comparisons: 64,
            marker: PythonMarkerLimits::default(),
        }
    }
}

#[derive(Debug)]
pub struct PythonRequirementSpans {
    prefix: Range<usize>,
    uri: Option<Range<usize>>,
    marker: Option<Range<usize>>,
    versions: Option<Range<usize>>,
}
impl PythonRequirementSpans {
    pub fn prefix(&self) -> Range<usize> {
        self.prefix.clone()
    }
    pub fn uri(&self) -> Option<Range<usize>> {
        self.uri.clone()
    }
    pub fn marker(&self) -> Option<Range<usize>> {
        self.marker.clone()
    }
    pub fn version_specifiers(&self) -> Option<Range<usize>> {
        self.versions.clone()
    }
}

pub struct PythonRequirementSyntax {
    raw: String,
    prefix: Requirement<UnresolvedDependencyReference>,
    marker: Option<PythonMarkerExpression>,
    spans: PythonRequirementSpans,
}
impl PythonRequirementSyntax {
    pub fn name(&self) -> &str {
        self.prefix.name.as_ref()
    }
    pub fn requested_extras(&self) -> impl Iterator<Item = &str> {
        self.prefix.extras.iter().map(|value| value.as_ref())
    }
    pub fn unresolved_reference(&self) -> Option<&UnresolvedDependencyReference> {
        match self.prefix.version_or_url.as_ref() {
            Some(VersionOrUrl::Url(value)) => Some(value),
            _ => None,
        }
    }
    pub fn version_specifiers(&self) -> Option<&pep440_rs::VersionSpecifiers> {
        match self.prefix.version_or_url.as_ref() {
            Some(VersionOrUrl::VersionSpecifier(value)) => Some(value),
            _ => None,
        }
    }
    /// Original version-clause slice. Never use the normalized AST's Display
    /// output in place of arbitrary-equality literal text.
    pub fn raw_version_specifiers(&self) -> Option<&str> {
        self.spans
            .versions
            .as_ref()
            .map(|range| &self.raw[range.clone()])
    }
    pub fn marker(&self) -> Option<&PythonMarkerExpression> {
        self.marker.as_ref()
    }
    pub fn spans(&self) -> &PythonRequirementSpans {
        &self.spans
    }
    pub fn raw_for_semantic_resolution(&self) -> &str {
        &self.raw
    }
}
impl fmt::Debug for PythonRequirementSyntax {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PythonRequirementSyntax")
            .field("name", &self.name())
            .field("requested_extras", &self.prefix.extras.len())
            .field("direct_reference", &self.unresolved_reference().is_some())
            .field("has_marker", &self.marker.is_some())
            .finish_non_exhaustive()
    }
}

pub fn preflight_python_requirement(
    raw: &str,
    limits: PythonRequirementLimits,
) -> PythonRequirementResult<PythonRequirementSpans> {
    spans::preflight(raw, limits)
}
pub fn parse_python_requirement(
    raw: &str,
    limits: PythonRequirementLimits,
) -> PythonRequirementResult<PythonRequirementSyntax> {
    parse_inner(raw, limits, &mut |_| {})
}

fn parse_inner(
    raw: &str,
    limits: PythonRequirementLimits,
    observer: &mut dyn FnMut(&str),
) -> PythonRequirementResult<PythonRequirementSyntax> {
    let spans = preflight_python_requirement(raw, limits)?;
    let prefix = &raw[spans.prefix.clone()];
    // Every library invocation is observable with exactly its input slice.
    // No marker text enters the library's BDD parser.
    let mut library_parse = |input: &str| {
        observer(input);
        let mut reporter = |_kind, _text: String| {};
        Requirement::<UnresolvedDependencyReference>::parse_reporter(
            input,
            Path::new(""),
            &mut reporter,
        )
    };
    let prefix = match library_parse(prefix) {
        Ok(parsed) => parsed,
        Err(original) => {
            // The specification permits one trailing version comma. The
            // pinned library still requires a constraint after every comma.
            // Retry only that version-only shape, preserving byte offsets.
            let retry = spans
                .uri
                .is_none()
                .then(|| version_without_tail_comma(prefix))
                .flatten();
            match retry.as_deref().map(&mut library_parse) {
                Some(Ok(parsed))
                    if matches!(
                        parsed.version_or_url,
                        Some(VersionOrUrl::VersionSpecifier(_))
                    ) =>
                {
                    parsed
                }
                _ => {
                    return Err(PythonRequirementError::Syntax {
                        offset: original.start,
                    });
                }
            }
        }
    };
    let marker = spans
        .marker
        .as_ref()
        .map(|marker| parse_python_marker(&raw[marker.clone()], limits.marker))
        .transpose()
        .map_err(PythonRequirementError::Marker)?;
    let mut owned = String::new();
    owned.try_reserve_exact(raw.len()).map_err(|_| {
        PythonRequirementError::LimitExceeded(PythonRequirementLimitKind::InputBytes)
    })?;
    owned.push_str(raw);
    Ok(PythonRequirementSyntax {
        raw: owned,
        prefix,
        marker,
        spans,
    })
}

/// Adapt only the optional final version comma, never URI or marker text.
/// Successful library parsing must still prove a nonempty version clause.
fn version_without_tail_comma(prefix: &str) -> Option<String> {
    let end = prefix.trim_end_matches([' ', '\t']);
    let versions = end
        .strip_suffix(')')
        .unwrap_or(end)
        .trim_end_matches([' ', '\t']);
    let comma = versions.len().checked_sub(1)?;
    if versions.as_bytes()[comma] != b',' {
        return None;
    }
    let mut adapted = prefix.to_owned();
    adapted.replace_range(comma..comma + 1, " ");
    Some(adapted)
}

#[cfg(test)]
fn parse_with_prefix_observer(
    raw: &str,
    limits: PythonRequirementLimits,
    observer: &mut dyn FnMut(&str),
) -> PythonRequirementResult<PythonRequirementSyntax> {
    parse_inner(raw, limits, observer)
}

#[cfg(test)]
#[path = "python_requirements/raw_specifier_tests.rs"]
mod raw_specifier_tests;
#[cfg(test)]
#[path = "python_requirements/tests.rs"]
mod tests;
