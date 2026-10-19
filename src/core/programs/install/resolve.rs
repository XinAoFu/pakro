//! F3 graph resolution orchestration: exact roots in, closed real graph out.
//!
//! Direct package roots must be exact; transitive ranges from package
//! metadata are resolved for real through the existing solver cores. The
//! result keeps every real dependency edge and source pin — a graph digest
//! alone is not an edge list.

use eyre::Result;

use crate::programs::OperationContext;

/// The closed graph produced by resolution, with exact roots preserved.
pub(crate) struct ClosedGraph {
    roots: Vec<PackageRoot>,
}

impl ClosedGraph {
    pub(crate) fn roots(&self) -> &[PackageRoot] {
        &self.roots
    }
}

/// One exact direct root of the resolved graph.
pub(crate) struct PackageRoot {
    name: String,
    version: String,
}

impl PackageRoot {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// Resolves the exact declared roots into a closed graph with real edges.
pub(crate) fn resolve_exact_roots(
    context: &OperationContext,
    roots: &[(&str, &str)],
) -> Result<ClosedGraph> {
    let _ = (context, roots);
    eyre::bail!("f3 exact-root graph resolution is not implemented")
}

#[cfg(test)]
mod tests;
