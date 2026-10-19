//! Bounded projection of one verified closed package graph into the same
//! authoritative project catalog used by language-only selections.
//!
//! The projection records exact roots, package nodes and real dependency
//! edges (a graph digest alone is not an edge list). Persistent commit goes
//! through the same single-catalog path; there is no separate writable
//! reference authority here.

use eyre::Result;

use crate::programs::OperationContext;

/// One package node of a committed graph projection.
pub(crate) struct GraphNode {
    name: String,
    version: String,
}

impl GraphNode {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// One real dependency edge of a committed graph projection.
pub(crate) struct GraphEdge {
    from: usize,
    to: usize,
}

impl GraphEdge {
    pub(crate) fn from(&self) -> usize {
        self.from
    }
    pub(crate) fn to(&self) -> usize {
        self.to
    }
}

/// Reads the committed graph projection of one selection, if any.
pub(crate) fn read_graph(
    context: &OperationContext,
    project: uuid::Uuid,
) -> Result<Option<CommittedGraph>> {
    let _ = (context, project);
    eyre::bail!("f3 package graph projection is not implemented")
}

/// The committed projection: roots, nodes and real edges.
pub(crate) struct CommittedGraph {
    roots: Vec<usize>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

impl CommittedGraph {
    pub(crate) fn roots(&self) -> &[usize] {
        &self.roots
    }
    pub(crate) fn nodes(&self) -> &[GraphNode] {
        &self.nodes
    }
    pub(crate) fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }
}
