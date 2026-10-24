//! Borrowed observations from a genuine closed attempt; output is portable data.
use super::*;
use crate::packages::python_resolution::lock_model::{
    EdgeInput, GroupKey, LockGraphInput, LockMarkerSemantics, LockModelError, LockModelLimits,
    NodeInput, PythonLockModel, RootInput, RuntimePin,
};

#[derive(Debug)]
pub(crate) enum RetainedProjectionError {
    Deadline,
    Model(LockModelError),
}
impl From<LockModelError> for RetainedProjectionError {
    fn from(value: LockModelError) -> Self {
        Self::Model(value)
    }
}
fn deadline(value: tokio::time::Instant) -> Result<(), RetainedProjectionError> {
    if tokio::time::Instant::now() >= value {
        return Err(RetainedProjectionError::Deadline);
    }
    Ok(())
}
impl RetainedRegistryOwner<'_> {
    fn facts(&self) -> &PythonDependencyMetadataFacts {
        match self {
            Self::Fresh(owner) => owner.facts(),
            Self::Locked(owner) => owner.facts(),
            Self::Built(owner) => owner.facts(),
        }
    }
}
impl RetainedGraphEvidence<'_> {
    pub(crate) fn project_model(
        &self,
        key: &GroupKey,
        runtime: RuntimePin,
        limits: LockModelLimits,
        end: tokio::time::Instant,
    ) -> Result<PythonLockModel, RetainedProjectionError> {
        deadline(end)?;
        limits.check_group_shape(
            self.plan.selected.len(),
            self.roots.len(),
            self.checked.roots.len(),
            self.plan.selected.len(),
            self.checked.edges.len(),
        )?;
        let mut selected = BTreeMap::new();
        for node in &self.plan.selected {
            deadline(end)?;
            if selected.insert(node.key, node).is_some() {
                return Err(LockModelError::IdentityMismatch.into());
            }
        }
        let mut raw = Vec::new();
        raw.try_reserve_exact(self.roots.len()).map_err(|_| {
            LockModelError::Limit(crate::packages::python_resolution::lock_model::LockLimit::Roots)
        })?;
        for root in &self.roots {
            deadline(end)?;
            raw.push(root.raw_for_semantic_resolution());
        }
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(self.checked.roots.len())
            .map_err(|_| {
                LockModelError::Limit(
                    crate::packages::python_resolution::lock_model::LockLimit::Roots,
                )
            })?;
        for root in &self.checked.roots {
            deadline(end)?;
            let node = selected
                .get(&root.to)
                .ok_or(LockModelError::IdentityMismatch)?;
            roots.push(RootInput {
                declaration_index: root.declaration,
                node: &node.name,
            });
        }
        let mut nodes = Vec::new();
        let mut pins = Vec::new();
        nodes
            .try_reserve_exact(self.plan.selected.len())
            .map_err(|_| {
                LockModelError::Limit(
                    crate::packages::python_resolution::lock_model::LockLimit::Nodes,
                )
            })?;
        pins.try_reserve_exact(self.plan.selected.len())
            .map_err(|_| {
                LockModelError::Limit(
                    crate::packages::python_resolution::lock_model::LockLimit::Pins,
                )
            })?;
        for node in &self.plan.selected {
            deadline(end)?;
            let pin = self.pin(node.key).ok_or(LockModelError::IdentityMismatch)?;
            nodes.push(NodeInput {
                name: &node.name,
                version: &node.version,
                source_pin: pin.id(),
            });
            pins.push(pin);
        }
        let mut edges = Vec::new();
        edges
            .try_reserve_exact(self.checked.edges.len())
            .map_err(|_| {
                LockModelError::Limit(
                    crate::packages::python_resolution::lock_model::LockLimit::Edges,
                )
            })?;
        for edge in &self.checked.edges {
            deadline(end)?;
            let from = selected
                .get(&edge.from)
                .ok_or(LockModelError::IdentityMismatch)?;
            let to = selected
                .get(&edge.to)
                .ok_or(LockModelError::IdentityMismatch)?;
            let facts = self
                .owners
                .get(&edge.from)
                .ok_or(LockModelError::IdentityMismatch)?
                .facts();
            let declaration = facts
                .requirements()
                .get(edge.declaration)
                .ok_or(LockModelError::IdentityMismatch)?;
            edges.push(EdgeInput {
                from: &from.name,
                to: &to.name,
                declaration: declaration.raw_for_semantic_resolution(),
            });
        }
        let graph = LockGraphInput {
            key,
            semantics: match self.semantics {
                MarkerSemantics::Current2026 => LockMarkerSemantics::Current2026,
            },
            runtime,
            root_declarations: &raw,
            roots: &roots,
            nodes: &nodes,
            edges: &edges,
        };
        let empty;
        let old = match self.root.model() {
            Some(model) => model,
            None => {
                empty = PythonLockModel::from_parts(&[], &[], limits)?;
                &empty
            }
        };
        deadline(end)?;
        let model = old.replace_group_references(&pins, &graph, limits)?;
        deadline(end)?;
        Ok(model)
    }
}
