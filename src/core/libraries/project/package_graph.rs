//! Readonly dependency projection of a selection in the single catalog.

use eyre::{Result, ensure};
use pakro_store::Digest;

use crate::libraries::project::repository::ProjectRepository;
use crate::packages::python_environment::model::{GenerationRecord, InputKind, PackageGraphRecord};
use crate::programs::OperationContext;

pub(crate) use crate::packages::python_environment::model::{
    PackageGraphEdge as GraphEdge, PackageGraphNode as GraphNode,
};
impl GraphNode {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn instance(&self) -> &[u8; 32] {
        &self.instance
    }
    pub(crate) fn source_pin(&self) -> &str {
        &self.source_pin
    }
    pub(crate) fn artifact(&self) -> &str {
        &self.artifact
    }
}
impl GraphEdge {
    pub(crate) fn from(&self) -> usize {
        self.from
    }
    pub(crate) fn to(&self) -> usize {
        self.to
    }
    pub(crate) fn declaration(&self) -> &str {
        &self.declaration
    }
}

/// A valid cached graph can be for a different request. Only these two
/// mismatches may be filtered as candidates; damaged facts remain errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GraphPinMismatch {
    #[error("shared package {package} belongs to another source scope")]
    SourceScope { package: String },
    #[error("shared package {package} differs from the project's known content pin")]
    ContentPin { package: String },
}
fn check_scope(
    node: &GraphNode,
    pin: &crate::packages::python_resolution::lock_model::ContentPin,
    allowed: &[(&str, [u8; 32])],
) -> Result<()> {
    if !allowed.iter().any(|(registry, base)| {
        *registry == pin.file().registry_id() && base == pin.file().canonical_base_fingerprint()
    }) {
        return Err(GraphPinMismatch::SourceScope {
            package: format!("{}=={}", node.name, node.version),
        }
        .into());
    }
    Ok(())
}
fn check_content(
    node: &GraphNode,
    pin: &crate::packages::python_resolution::lock_model::ContentPin,
    expected: &crate::packages::python_resolution::lock_model::ContentPin,
) -> Result<()> {
    if expected.id() != pin.id()
        || expected.digest() != pin.digest()
        || expected.size() != pin.size()
    {
        return Err(GraphPinMismatch::ContentPin {
            package: format!("{}=={}", node.name, node.version),
        }
        .into());
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct CommittedGraph {
    graph: PackageGraphRecord,
    fingerprint: [u8; 32],
    layout: [u8; 32],
    language: [u8; 32],
    allocation: String,
}
impl CommittedGraph {
    /// The catalog model verified this digest against every graph fact.
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    pub(crate) fn roots(&self) -> &[usize] {
        &self.graph.roots
    }
    pub(crate) fn nodes(&self) -> &[GraphNode] {
        &self.graph.nodes
    }
    pub(crate) fn edges(&self) -> &[GraphEdge] {
        &self.graph.edges
    }
    pub(crate) fn layout(&self) -> &[u8; 32] {
        &self.layout
    }
    pub(crate) fn language(&self) -> &[u8; 32] {
        &self.language
    }
    pub(crate) fn allocation(&self) -> &str {
        &self.allocation
    }
    /// Checks current local project pins and source scope without fetching.
    /// The actual lock owner must remain alive until selection commit.
    pub(crate) fn validate_pins(
        &self,
        root: &crate::packages::python_resolution::lock_session::BoundPythonLock,
        allowed: &[(&str, [u8; 32])],
    ) -> Result<()> {
        use crate::packages::python_resolution::lock_session::{FilePinLookup, ResolutionIntent};
        root.recheck()?;
        for node in self.nodes() {
            let pin = node.content_pin()?;
            check_scope(node, &pin, allowed)?;
            match root.lookup(pin.file(), ResolutionIntent::Resolve)? {
                FilePinLookup::Existing(slot) => {
                    slot.recheck_for(root)?;
                    check_content(node, &pin, slot.pin())?;
                }
                FilePinLookup::Fresh(slot) => slot.recheck_for(root)?,
                FilePinLookup::Missing => eyre::bail!("package source pin is unavailable"),
            }
        }
        root.recheck()
    }
    /// Readonly counterpart: consumes only the lock bytes already bound by
    /// the project observation, and never opens a writer or creates a lock.
    pub(crate) fn validate_observed_pins(
        &self,
        observed: &crate::libraries::project::snapshot::ObservedProjectInputs,
        allowed: &[(&str, [u8; 32])],
    ) -> Result<()> {
        use crate::packages::python_resolution::lock_session::{decode_root, expected_pin};
        observed.recheck()?;
        // Decode every applicable observed lock before filtering a cached
        // source candidate, so damaged lock bytes can never become "missing".
        let mut models = Vec::new();
        for path in observed
            .paths()
            .filter(|path| path.extension() == Some(std::ffi::OsStr::new("lock")))
        {
            let bytes = observed
                .document(path)
                .ok_or_else(|| eyre::eyre!("observed lock document is missing"))?;
            if let Some(model) = decode_root(bytes, Default::default())? {
                models.push(model);
            }
        }
        for node in self.nodes() {
            let pin = node.content_pin()?;
            check_scope(node, &pin, allowed)?;
            for model in &models {
                if let Some(expected) = expected_pin(Some(model), pin.file())? {
                    check_content(node, &pin, expected)?;
                }
            }
        }
        Ok(observed.recheck()?)
    }
    pub(super) fn from_record(record: &GenerationRecord) -> Result<Option<Self>> {
        let Some(graph) = record.package_graph() else {
            return Ok(None);
        };
        let layout = *record
            .shared_layout()
            .ok_or_else(|| eyre::eyre!("package graph has no shared layout"))?;
        let mut languages = record
            .inputs()
            .iter()
            .filter(|input| input.kind() == InputKind::Language);
        let language: Digest = languages
            .next()
            .ok_or_else(|| eyre::eyre!("graph language is missing"))?
            .digest()
            .parse()?;
        ensure!(
            languages.next().is_none(),
            "graph has multiple language instances"
        );
        let Digest::Sha256(language) = language else {
            eyre::bail!("graph language is not a typed identity")
        };
        Ok(Some(Self {
            graph: graph.clone(),
            fingerprint: *record.graph(),
            layout,
            language,
            allocation: record.allocation().into(),
        }))
    }
}
pub(crate) fn read_graph(
    context: &OperationContext,
    project: uuid::Uuid,
) -> Result<Option<CommittedGraph>> {
    let repository = ProjectRepository::open_readonly(context)?;
    repository
        .read_views()
        .package_graph(&super::identity::ProjectIdentity::from_uuid(project))
}

#[cfg(test)]
#[path = "package_graph/tests.rs"]
mod tests;
