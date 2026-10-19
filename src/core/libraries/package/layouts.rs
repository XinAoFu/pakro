//! Shared package layouts: the install-compatible projection of a closed
//! package graph onto one shared language instance.
//!
//! A layout key never contains a project identity: identical language +
//! graph + target + recipe combinations share one layout. The layout records
//! its final prefix before position-sensitive files are generated.

use eyre::Result;

use crate::programs::OperationContext;

/// A sealed, verified layout ready for publication.
pub(crate) struct PreparedLayout {
    language: [u8; 32],
    prefix: std::path::PathBuf,
}

impl PreparedLayout {
    pub(crate) fn language(&self) -> &[u8; 32] {
        &self.language
    }
    pub(crate) fn prefix(&self) -> &std::path::Path {
        &self.prefix
    }
}

/// Reserves (or reuses) the shared layout slot for one exact key. The final
/// prefix is fixed before any position-sensitive entry is generated; a
/// failed layout never becomes Ready.
pub(crate) fn reserve_layout(
    context: &OperationContext,
    language: [u8; 32],
    graph: [u8; 32],
) -> Result<PreparedLayout> {
    let _ = (context, graph);
    eyre::bail!("f3 shared layout reservation is not implemented")
}

#[cfg(test)]
mod tests;
