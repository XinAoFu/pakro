//! Original-text arbitrary equality beside the shared ordinary PEP 440 matcher.
//!
//! This module consumes specifiers only, never a complete requirement or a
//! source reference. It grants no resolver, source, or installation authority.

use std::{fmt, ops::Range, str::FromStr};

use pep440_rs::{Version, VersionSpecifier, VersionSpecifiers};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct RawVersionConstraintLimits {
    pub max_specifier_bytes: usize,
    pub max_clauses: usize,
    pub max_literal_bytes: usize,
    pub max_candidate_bytes: usize,
}

impl Default for RawVersionConstraintLimits {
    fn default() -> Self {
        Self {
            max_specifier_bytes: 16 * 1024,
            max_clauses: 1024,
            max_literal_bytes: 16 * 1024,
            max_candidate_bytes: 16 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawVersionConstraintLimit {
    SpecifierBytes,
    Clauses,
    LiteralBytes,
    CandidateBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RawVersionConstraintError {
    #[error("version constraint limits must be positive")]
    InvalidLimits,
    #[error("version constraint limit exceeded: {0:?}")]
    Limit(RawVersionConstraintLimit),
    #[error("invalid version constraint syntax")]
    Syntax,
}

#[derive(Clone)]
pub struct RawVersionConstraint {
    raw: String,
    ordinary: VersionSpecifiers,
    arbitrary: Vec<Range<usize>>,
    clauses: usize,
    max_candidate_bytes: usize,
}

impl fmt::Debug for RawVersionConstraint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawVersionConstraint")
            .field("clauses", &self.clauses)
            .field("arbitrary_clauses", &self.arbitrary.len())
            .finish_non_exhaustive()
    }
}

impl RawVersionConstraint {
    pub fn parse(
        raw: &str,
        limits: RawVersionConstraintLimits,
    ) -> Result<Self, RawVersionConstraintError> {
        use RawVersionConstraintError::{InvalidLimits, Limit, Syntax};
        use RawVersionConstraintLimit::{Clauses, LiteralBytes, SpecifierBytes};
        if [
            limits.max_specifier_bytes,
            limits.max_clauses,
            limits.max_literal_bytes,
            limits.max_candidate_bytes,
        ]
        .contains(&0)
        {
            return Err(InvalidLimits);
        }
        if raw.len() > limits.max_specifier_bytes {
            return Err(Limit(SpecifierBytes));
        }
        let mut body = raw.trim_ascii();
        if let Some(inner) = body.strip_prefix('(') {
            body = inner.strip_suffix(')').ok_or(Syntax)?.trim_ascii();
            if body.is_empty() {
                return Err(Syntax);
            }
        }
        let mut ordinary = Vec::new();
        let mut arbitrary = Vec::new();
        let mut clauses = 0usize;
        if !body.is_empty() {
            // A single trailing comma is allowed by the requirement grammar.
            // Removing only one preserves rejection of repeated empty clauses.
            let body = body.strip_suffix(',').unwrap_or(body);
            for clause in body.split(',') {
                let clause = clause.trim_ascii();
                clauses = clauses
                    .checked_add(1)
                    .filter(|count| *count <= limits.max_clauses)
                    .ok_or(Limit(Clauses))?;
                if clause.is_empty() || clause.contains(['(', ')', ';']) {
                    return Err(Syntax);
                }
                if let Some(literal) = clause.strip_prefix("===") {
                    let literal = literal.trim_ascii();
                    if literal.is_empty()
                        || literal
                            .chars()
                            .any(|value| value.is_whitespace() || value.is_control())
                    {
                        return Err(Syntax);
                    }
                    if literal.len() > limits.max_literal_bytes {
                        return Err(Limit(LiteralBytes));
                    }
                    let offset = literal.as_ptr() as usize - raw.as_ptr() as usize;
                    arbitrary.try_reserve(1).map_err(|_| Limit(Clauses))?;
                    arbitrary.push(offset..offset + literal.len());
                } else {
                    let specifier = VersionSpecifier::from_str(clause).map_err(|_| Syntax)?;
                    ordinary.try_reserve(1).map_err(|_| Limit(Clauses))?;
                    ordinary.push(specifier);
                }
            }
        }
        let mut owned_raw = String::new();
        owned_raw
            .try_reserve_exact(raw.len())
            .map_err(|_| Limit(SpecifierBytes))?;
        owned_raw.push_str(raw);
        Ok(Self {
            raw: owned_raw,
            ordinary: ordinary.into_iter().collect(),
            arbitrary,
            clauses,
            max_candidate_bytes: limits.max_candidate_bytes,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn clause_count(&self) -> usize {
        self.clauses
    }

    /// Ordinary AST only: arbitrary literals deliberately never enter it.
    pub fn ordinary(&self) -> &VersionSpecifiers {
        &self.ordinary
    }

    pub fn has_arbitrary_equality(&self) -> bool {
        !self.arbitrary.is_empty()
    }

    /// Original literal RHS values, for policy facts such as an explicit
    /// prerelease request. Matching still belongs to `matches`.
    pub fn arbitrary_literals(&self) -> impl Iterator<Item = &str> {
        self.arbitrary.iter().map(|range| &self.raw[range.clone()])
    }

    pub fn matches(&self, candidate: &str) -> Result<bool, RawVersionConstraintError> {
        if candidate.len() > self.max_candidate_bytes {
            return Err(RawVersionConstraintError::Limit(
                RawVersionConstraintLimit::CandidateBytes,
            ));
        }
        if !self
            .arbitrary
            .iter()
            .all(|range| candidate.eq_ignore_ascii_case(&self.raw[range.clone()]))
        {
            return Ok(false);
        }
        if self.ordinary.is_empty() && !self.arbitrary.is_empty() {
            return Ok(true);
        }
        let Ok(version) = Version::from_str(candidate) else {
            return Ok(false);
        };
        Ok(super::specifiers_contain(&self.ordinary, &version))
    }
}

#[cfg(test)]
#[path = "raw_specifiers/tests.rs"]
mod tests;
