//! Ecosystem-neutral metadata types the resolver and glue layer consume.

use crate::wheel::WheelTags;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyConstraint {
    /// PEP 503-normalized package name
    pub name: String,
    /// PEP 440 specifier set as written by the package author; empty means any
    pub constraint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    Wheel(WheelTags),
    Sdist,
}

impl ArtifactKind {
    pub fn is_wheel(&self) -> bool {
        matches!(self, ArtifactKind::Wheel(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    pub filename: String,
    pub url: String,
    /// digest string as "<algorithm>-<hex>"; sha256 for wheels, sha512 for npm tarballs
    pub digest: Option<String>,
    pub kind: ArtifactKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    pub requires_python: Option<String>,
    /// dependencies applicable to the requested runtime, markers already
    /// evaluated
    pub deps: Vec<DependencyConstraint>,
    pub artifacts: Vec<ArtifactDescriptor>,
}

/// PEP 503 name normalization: case-fold and collapse `_`, `-`, `.` runs to
/// `-` so `Zope.Interface` and `zope-interface` map to one package.
pub fn normalize_py_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if matches!(ch, '_' | '-' | '.') {
            if !last_dash {
                out.push('-');
            }
            last_dash = true;
        } else {
            out.push(ch);
            last_dash = false;
        }
    }
    out
}
