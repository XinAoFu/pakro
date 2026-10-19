use std::fmt;

use crate::toolset::ToolSource;

/// A declared candidate, not an installed language identity or a lease.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeCandidate {
    pub runtime_name: String,
    pub runtime_version: String,
    pub source: ToolSource,
}

impl fmt::Debug for RuntimeCandidate {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("RuntimeCandidate").finish_non_exhaustive()
    }
}

/// An explicitly chosen document and field. R03 must observe and verify it;
/// this descriptor is not a current file owner or a prepared document patch.
#[derive(Clone, PartialEq, Eq)]
pub struct DeclarationTarget {
    pub source: ToolSource,
    pub field_path: Vec<String>,
}

impl fmt::Debug for DeclarationTarget {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ToolSource::Environment can contain a secret value.
        out.debug_struct("DeclarationTarget")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationOperation {
    SetSelection,
    AddDependency,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationEditIntent {
    pub target: DeclarationTarget,
    pub operation: DeclarationOperation,
}
