// Bounded pure-data group replacement. This grants no source or lock rights.
use std::collections::{BTreeMap, BTreeSet};

use crate::packages::python_resolution::lock_model::{
    ContentPin, EdgeInput, LockGraphInput, LockLimit, LockModelError, LockModelLimits,
    LockModelResult, LockReference, LockedGraph, NodeInput, PythonLockModel, RootInput,
};

impl PythonLockModel {
    pub(crate) fn replace_group(
        &self,
        new_pins: &[ContentPin],
        graph: &LockGraphInput<'_>,
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        self.replace_group_iter(new_pins.iter(), graph, limits)
    }
    pub(crate) fn replace_group_references(
        &self,
        new_pins: &[&ContentPin],
        graph: &LockGraphInput<'_>,
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        self.replace_group_iter(new_pins.iter().copied(), graph, limits)
    }
    fn replace_group_iter<'a>(
        &self,
        new_pins: impl ExactSizeIterator<Item = &'a ContentPin> + Clone,
        graph: &LockGraphInput<'_>,
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        if !self.conda().is_empty() {
            return Err(LockModelError::UnsupportedSourceKind);
        }
        // Bound both supplied inputs through the existing kernels before any
        // temporary projection, then validate the entire combined output once.
        self.check_limits(limits)?;
        super::preflight_borrowed_parts(new_pins.clone(), std::slice::from_ref(graph), limits)?;
        let count = self
            .graphs()
            .iter()
            .filter(|old| old.key() != graph.key)
            .count()
            .checked_add(1)
            .filter(|count| *count <= limits.max_groups)
            .ok_or(LockModelError::Limit(LockLimit::Groups))?;
        let mut views = Vec::new();
        views
            .try_reserve_exact(count - 1)
            .map_err(|_| LockModelError::Limit(LockLimit::Groups))?;
        for old in self.graphs().iter().filter(|old| old.key() != graph.key) {
            views.push(BorrowedGraph::new(old)?);
        }
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(count)
            .map_err(|_| LockModelError::Limit(LockLimit::Groups))?;
        inputs.extend(views.iter().map(BorrowedGraph::input));
        inputs.push(LockGraphInput {
            key: graph.key,
            semantics: graph.semantics,
            runtime: graph.runtime,
            root_declarations: graph.root_declarations,
            roots: graph.roots,
            nodes: graph.nodes,
            edges: graph.edges,
        });
        let mut used = BTreeSet::new();
        for input in &inputs {
            for node in input.nodes {
                used.insert(node.source_pin);
                if used.len() > limits.max_pins {
                    return Err(LockModelError::Limit(LockLimit::Pins));
                }
            }
        }
        let old_pins = self
            .pins()
            .iter()
            .map(|pin| (pin.id(), pin))
            .collect::<BTreeMap<_, _>>();
        let mut pins = old_pins
            .iter()
            .filter(|(id, _)| used.contains(id))
            .map(|(id, pin)| (*id, *pin))
            .collect::<BTreeMap<_, _>>();
        for pin in new_pins {
            if old_pins.get(&pin.id()).is_some_and(|old| *old != pin)
                || pins.get(&pin.id()).is_some_and(|old| *old != pin)
            {
                return Err(LockModelError::IdentityMismatch);
            }
            if !used.contains(&pin.id()) {
                return Err(LockModelError::Reference(LockReference::UnusedPin));
            }
            pins.insert(pin.id(), pin);
        }
        // Borrowed pin references avoid cloning the old table before the
        // combined count/UTF-8 budget has passed the same constructor preflight.
        let mut model = Self::from_borrowed_parts(pins.values().copied(), &inputs, limits)?;
        if let Some(old) = &self.locators {
            let locators = model.pins().iter().map(|pin| {
                let id = hex::encode(pin.id().as_bytes());
                old.get(&id).cloned().map(|url| (id, url))
                    .ok_or(LockModelError::Reference(LockReference::NodePin))
            }).collect::<LockModelResult<BTreeMap<_, _>>>()?;
            model = model.with_locators(locators, limits)?;
        }
        Ok(model)
    }
}

fn collect<T>(iter: impl ExactSizeIterator<Item = T>, kind: LockLimit) -> LockModelResult<Vec<T>> {
    let mut output = Vec::new();
    let count = iter.len();
    output
        .try_reserve_exact(count)
        .map_err(|_| LockModelError::Limit(kind))?;
    output.extend(iter);
    Ok(output)
}
struct BorrowedGraph<'a> {
    graph: &'a LockedGraph,
    raw: Vec<&'a str>,
    roots: Vec<RootInput<'a>>,
    nodes: Vec<NodeInput<'a>>,
    edges: Vec<EdgeInput<'a>>,
}
impl<'a> BorrowedGraph<'a> {
    fn new(graph: &'a LockedGraph) -> LockModelResult<Self> {
        Ok(Self {
            graph,
            raw: collect(
                graph.root_declarations().iter().map(String::as_str),
                LockLimit::Roots,
            )?,
            roots: collect(
                graph.roots().iter().map(|root| RootInput {
                    declaration_index: root.declaration_index(),
                    node: root.node(),
                }),
                LockLimit::Roots,
            )?,
            nodes: collect(
                graph.nodes().iter().map(|node| NodeInput {
                    name: node.name(),
                    version: node.version(),
                    source_pin: node.source_pin(),
                }),
                LockLimit::Nodes,
            )?,
            edges: collect(
                graph.edges().iter().map(|edge| EdgeInput {
                    from: edge.from(),
                    to: edge.to(),
                    declaration: edge.declaration(),
                }),
                LockLimit::Edges,
            )?,
        })
    }
    fn input(&self) -> LockGraphInput<'_> {
        LockGraphInput {
            key: self.graph.key(),
            semantics: self.graph.semantics(),
            runtime: self.graph.runtime(),
            root_declarations: &self.raw,
            roots: &self.roots,
            nodes: &self.nodes,
            edges: &self.edges,
        }
    }
}

#[cfg(test)]
#[path = "replace_group/tests.rs"]
mod tests;
