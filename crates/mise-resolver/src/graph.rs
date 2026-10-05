use std::collections::HashMap;

use petgraph::algo::toposort;
use petgraph::graph::Graph;
use serde::{Deserialize, Serialize};

use crate::error::ResolverError;

/// One edge of an unsolved constraint: what a package needs from another
/// package. The constraint string is interpreted by the owning ecosystem's
/// version rules; this crate never compares versions itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRequest {
    pub eco: String,
    pub name: String,
    pub constraint: String,
}

/// A package with its solved exact version, its artifact digest, and the
/// constraints it places on others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedNode {
    pub eco: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub deps: Vec<DependencyRequest>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedGraph {
    pub nodes: Vec<ResolvedNode>,
}

fn key(eco: &str, name: &str) -> String {
    format!("{eco}:{name}")
}

impl ResolvedGraph {
    /// Canonical ordering by (eco, name, version). Sorting through this makes
    /// any two solves of the same input byte-identical, which the lockfile
    /// relies on.
    pub fn sorted(&self) -> ResolvedGraph {
        let mut nodes = self.nodes.clone();
        nodes.sort_by(|a, b| (&a.eco, &a.name, &a.version).cmp(&(&b.eco, &b.name, &b.version)));
        ResolvedGraph { nodes }
    }

    /// Node keys ordered so every dependency precedes its dependents, ready
    /// to feed a parallel installer. Errors on cycles, dangling dependency
    /// references, and duplicate names.
    pub fn install_order(&self) -> Result<Vec<String>, ResolverError> {
        let mut graph = Graph::<String, ()>::new();
        let mut indexes = HashMap::new();
        for node in &self.nodes {
            let node_key = key(&node.eco, &node.name);
            if indexes.contains_key(&node_key) {
                return Err(ResolverError::DuplicateNode(node_key));
            }
            let index = graph.add_node(node_key.clone());
            indexes.insert(node_key, index);
        }
        for node in &self.nodes {
            let dependent = indexes[&key(&node.eco, &node.name)];
            for dep in &node.deps {
                let dep_key = key(&dep.eco, &dep.name);
                let dependency = indexes
                    .get(&dep_key)
                    .ok_or_else(|| ResolverError::MissingNode(dep_key.clone()))?;
                graph.add_edge(*dependency, dependent, ());
            }
        }
        toposort(&graph, None)
            .map_err(|cycle| ResolverError::Cycle(graph[cycle.node_id()].clone()))
            .map(|order| {
                order
                    .into_iter()
                    .map(|index| graph[index].clone())
                    .collect()
            })
    }
}
