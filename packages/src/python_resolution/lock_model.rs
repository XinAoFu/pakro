//! Portable Python lock data. This is never resolution or source authority.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use pakro_index::python_source::{CanonicalRegistryUrl, NormalizedPythonName, PythonRegistryId};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use pakro_store::Digest;
use serde::de::DeserializeSeed;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

#[path = "lock_model/decode.rs"]
mod decode;
#[path = "lock_model/encode.rs"]
mod encode;

pub(crate) type LockModelResult<T> = Result<T, LockModelError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockField {
    Scope,
    Group,
    Registry,
    Project,
    Version,
    Filename,
    Digest,
    Size,
    PinId,
    Declaration,
    Native,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockIdentity {
    Pin,
    Group,
    Node,
    RootReference,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockReference {
    NodePin,
    Root,
    RootDeclaration,
    Edge,
    UnusedPin,
    UnreachableNode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockLimit {
    DocumentBytes,
    Pins,
    Groups,
    Nodes,
    TotalNodes,
    Edges,
    Roots,
    References,
    FieldBytes,
    DeclarationBytes,
    TotalUtf8Bytes,
    ArtifactBytes,
}
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum LockModelError {
    #[error("Python lock limits must be positive")]
    InvalidLimits,
    #[error("Python lock limit exceeded: {0:?}")]
    Limit(LockLimit),
    #[error("invalid Python lock field: {0:?}")]
    InvalidField(LockField),
    #[error("duplicate Python lock identity: {0:?}")]
    Duplicate(LockIdentity),
    #[error("Python lock reference is not closed: {0:?}")]
    Reference(LockReference),
    #[error("Python lock identities conflict")]
    IdentityMismatch,
    #[error("Python lock schema is unsupported")]
    UnsupportedSchema,
    #[error("Python lock marker semantics are unsupported")]
    UnsupportedMarkerSemantics,
    #[error("Python runtime content recipe is unsupported")]
    UnsupportedRuntimeRecipe,
    #[error("Python source kind requires a separate portable adapter")]
    UnsupportedSourceKind,
    #[error("invalid Python lock TOML projection")]
    InvalidToml,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LockModelLimits {
    pub max_document_bytes: usize,
    pub max_pins: usize,
    pub max_groups: usize,
    pub max_nodes_per_group: usize,
    pub max_total_nodes: usize,
    pub max_edges_per_group: usize,
    pub max_roots_per_group: usize,
    pub max_total_references: usize,
    pub max_field_bytes: usize,
    pub max_declaration_bytes: usize,
    pub max_total_utf8_bytes: usize,
    pub max_artifact_bytes: u64,
}
impl LockModelLimits {
    pub(crate) fn validate(self) -> LockModelResult<()> {
        valid_limits(self)
    }
}
impl Default for LockModelLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 64 * 1024 * 1024,
            max_pins: 65_536,
            max_groups: 4096,
            max_nodes_per_group: 65_536,
            max_total_nodes: 262_144,
            max_edges_per_group: 262_144,
            max_roots_per_group: 65_536,
            max_total_references: 1_048_576,
            max_field_bytes: 16 * 1024,
            max_declaration_bytes: 64 * 1024,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            max_artifact_bytes: 1024 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GroupKey {
    config_scope: String,
    group: String,
    target: [u8; 32],
}
impl GroupKey {
    pub(crate) fn new(
        scope: &str,
        group: &str,
        target: [u8; 32],
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        valid_limits(limits)?;
        let mut usage = Usage::default();
        text(scope, LockField::Scope, false, &mut usage, limits)?;
        text(group, LockField::Group, false, &mut usage, limits)?;
        valid_scope(scope)?;
        if group.is_empty() || group.chars().any(char::is_control) {
            return Err(LockModelError::InvalidField(LockField::Group));
        }
        Ok(Self {
            config_scope: scope.into(),
            group: group.into(),
            target,
        })
    }
    pub(crate) fn config_scope(&self) -> &str {
        &self.config_scope
    }
    pub(crate) fn group(&self) -> &str {
        &self.group
    }
    pub(crate) fn target(&self) -> &[u8; 32] {
        &self.target
    }
}
impl fmt::Debug for GroupKey {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("GroupKey(<portable scope>)")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockMarkerSemantics {
    Current2026,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryArtifactKind {
    Wheel,
    Sdist,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinEvidence {
    Advertised,
    ObservedDuringFreshResolution,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PinId([u8; 32]);
impl PinId {
    pub(crate) fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub(crate) fn parse_hex(value: &str) -> LockModelResult<Self> {
        let mut bytes = [0; 32];
        if value.len() != 64 || hex::decode_to_slice(value, &mut bytes).is_err() {
            return Err(LockModelError::InvalidField(LockField::PinId));
        }
        Ok(Self(bytes))
    }
}
impl fmt::Debug for PinId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("PinId(<data identity>)")
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RegistryFileRefInput<'a> {
    pub registry_id: &'a str,
    pub canonical_base_fingerprint: [u8; 32],
    pub project: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
    pub kind: RegistryArtifactKind,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RegistryFileRef {
    registry_id: String,
    canonical_base_fingerprint: [u8; 32],
    project: String,
    version: String,
    filename: String,
    kind: RegistryArtifactKind,
}
impl RegistryFileRef {
    pub(crate) fn new(
        input: RegistryFileRefInput<'_>,
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        valid_limits(limits)?;
        let mut usage = Usage::default();
        for (value, field) in [
            (input.registry_id, LockField::Registry),
            (input.project, LockField::Project),
            (input.version, LockField::Version),
            (input.filename, LockField::Filename),
        ] {
            text(value, field, false, &mut usage, limits)?;
        }
        PythonRegistryId::new(input.registry_id)
            .map_err(|_| LockModelError::InvalidField(LockField::Registry))?;
        let project = normalized(input.project)?;
        let version = exact_version(input.version)?;
        if input.filename.is_empty()
            || matches!(input.filename, "." | "..")
            || input
                .filename
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '\\'))
        {
            return Err(LockModelError::InvalidField(LockField::Filename));
        }
        let (file_name, file_version) = match input.kind {
            RegistryArtifactKind::Wheel => {
                let file = pakro_index::parse_wheel_filename(input.filename)
                    .ok_or(LockModelError::InvalidField(LockField::Filename))?;
                (normalized(&file.name)?, exact_version(&file.version)?)
            }
            RegistryArtifactKind::Sdist => {
                let stem = input
                    .filename
                    .strip_suffix(".tar.gz")
                    .or_else(|| input.filename.strip_suffix(".zip"))
                    .ok_or(LockModelError::InvalidField(LockField::Filename))?;
                let (name, raw_version) = stem
                    .rsplit_once('-')
                    .ok_or(LockModelError::InvalidField(LockField::Filename))?;
                if raw_version.contains('-') {
                    return Err(LockModelError::InvalidField(LockField::Filename));
                }
                let parsed = exact_version(raw_version)?;
                if parsed != raw_version {
                    return Err(LockModelError::InvalidField(LockField::Filename));
                }
                (normalized(name)?, parsed)
            }
        };
        if file_name != project || !same_version(&file_version, &version) {
            return Err(LockModelError::IdentityMismatch);
        }
        let mut canonical_usage = Usage::default();
        for (value, field) in [
            (input.registry_id, LockField::Registry),
            (project.as_str(), LockField::Project),
            (version.as_str(), LockField::Version),
            (input.filename, LockField::Filename),
        ] {
            text(value, field, false, &mut canonical_usage, limits)?;
        }
        Ok(Self {
            registry_id: input.registry_id.into(),
            canonical_base_fingerprint: input.canonical_base_fingerprint,
            project,
            version,
            filename: input.filename.into(),
            kind: input.kind,
        })
    }
    pub(crate) fn registry_id(&self) -> &str {
        &self.registry_id
    }
    pub(crate) fn canonical_base_fingerprint(&self) -> &[u8; 32] {
        &self.canonical_base_fingerprint
    }
    pub(crate) fn project(&self) -> &str {
        &self.project
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
    pub(crate) fn kind(&self) -> RegistryArtifactKind {
        self.kind
    }
}
impl fmt::Debug for RegistryFileRef {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("RegistryFileRef(<portable identity>)")
    }
}
pub(crate) fn canonical_registry_base_fingerprint(base: &CanonicalRegistryUrl) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"pakro.python-registry-base/v1\0");
    hash_field(&mut hash, base.as_url().as_str().as_bytes());
    hash.finalize().into()
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}
fn pin_id(file: &RegistryFileRef, digest: Digest) -> PinId {
    let mut hash = Sha256::new();
    hash.update(b"pakro.python-registry-pin/v1\0");
    for value in [
        file.registry_id.as_bytes(),
        &file.canonical_base_fingerprint,
        file.project.as_bytes(),
        file.version.as_bytes(),
        file.filename.as_bytes(),
        match file.kind {
            RegistryArtifactKind::Wheel => b"wheel",
            RegistryArtifactKind::Sdist => b"sdist",
        },
    ] {
        hash_field(&mut hash, value);
    }
    match digest {
        Digest::Sha256(bytes) => {
            hash_field(&mut hash, b"sha256");
            hash_field(&mut hash, &bytes);
        }
        Digest::Sha512(bytes) => {
            hash_field(&mut hash, b"sha512");
            hash_field(&mut hash, &bytes);
        }
    }
    PinId(hash.finalize().into())
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ContentPin {
    id: PinId,
    file: RegistryFileRef,
    digest: Digest,
    size: u64,
    evidence: PinEvidence,
}
impl ContentPin {
    pub(crate) fn new(
        file: &RegistryFileRef,
        digest: Digest,
        size: u64,
        evidence: PinEvidence,
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        valid_limits(limits)?;
        if size > limits.max_artifact_bytes {
            return Err(LockModelError::Limit(LockLimit::ArtifactBytes));
        }
        if size > i64::MAX as u64 {
            return Err(LockModelError::InvalidField(LockField::Size));
        }
        let canonical = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: &file.registry_id,
                canonical_base_fingerprint: file.canonical_base_fingerprint,
                project: &file.project,
                version: &file.version,
                filename: &file.filename,
                kind: file.kind,
            },
            limits,
        )?;
        if &canonical != file {
            return Err(LockModelError::IdentityMismatch);
        }
        Ok(Self {
            id: pin_id(file, digest),
            file: file.clone(),
            digest,
            size,
            evidence,
        })
    }
    pub(crate) fn id(&self) -> PinId {
        self.id
    }
    pub(crate) fn file(&self) -> &RegistryFileRef {
        &self.file
    }
    pub(crate) fn digest(&self) -> Digest {
        self.digest
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn evidence(&self) -> PinEvidence {
        self.evidence
    }
}
impl fmt::Debug for ContentPin {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("ContentPin(<portable expected digest>)")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePin {
    content_tree: [u8; 32],
    recipe: u32,
    target: [u8; 32],
}
impl RuntimePin {
    pub(crate) fn new(
        content_tree: [u8; 32],
        recipe: u32,
        target: [u8; 32],
    ) -> LockModelResult<Self> {
        if recipe != 1 {
            return Err(LockModelError::UnsupportedRuntimeRecipe);
        }
        Ok(Self {
            content_tree,
            recipe,
            target,
        })
    }
    pub(crate) fn content_tree(&self) -> &[u8; 32] {
        &self.content_tree
    }
    pub(crate) fn recipe(&self) -> u32 {
        self.recipe
    }
    pub(crate) fn target(&self) -> &[u8; 32] {
        &self.target
    }
}
#[derive(Clone, Copy)]
pub(crate) struct RootInput<'a> {
    pub declaration_index: usize,
    pub node: &'a str,
}
#[derive(Clone, Copy)]
pub(crate) struct NodeInput<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub source_pin: PinId,
}
#[derive(Clone, Copy)]
pub(crate) struct EdgeInput<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub declaration: &'a str,
}
pub(crate) struct LockGraphInput<'a> {
    pub key: &'a GroupKey,
    pub semantics: LockMarkerSemantics,
    pub runtime: RuntimePin,
    pub root_declarations: &'a [&'a str],
    pub roots: &'a [RootInput<'a>],
    pub nodes: &'a [NodeInput<'a>],
    pub edges: &'a [EdgeInput<'a>],
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LockedRoot {
    declaration_index: usize,
    node: String,
}
impl LockedRoot {
    pub(crate) fn declaration_index(&self) -> usize {
        self.declaration_index
    }
    pub(crate) fn node(&self) -> &str {
        &self.node
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LockedNode {
    name: String,
    version: String,
    source_pin: PinId,
}
impl LockedNode {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn source_pin(&self) -> PinId {
        self.source_pin
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LockedEdge {
    from: String,
    to: String,
    declaration: String,
}
impl LockedEdge {
    pub(crate) fn from(&self) -> &str {
        &self.from
    }
    pub(crate) fn to(&self) -> &str {
        &self.to
    }
    pub(crate) fn declaration(&self) -> &str {
        &self.declaration
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LockedGraph {
    key: GroupKey,
    semantics: LockMarkerSemantics,
    runtime: RuntimePin,
    root_declarations: Vec<String>,
    roots: Vec<LockedRoot>,
    nodes: Vec<LockedNode>,
    edges: Vec<LockedEdge>,
}
impl LockedGraph {
    pub(crate) fn key(&self) -> &GroupKey {
        &self.key
    }
    pub(crate) fn semantics(&self) -> LockMarkerSemantics {
        self.semantics
    }
    pub(crate) fn runtime(&self) -> RuntimePin {
        self.runtime
    }
    pub(crate) fn root_declarations(&self) -> &[String] {
        &self.root_declarations
    }
    pub(crate) fn roots(&self) -> &[LockedRoot] {
        &self.roots
    }
    pub(crate) fn nodes(&self) -> &[LockedNode] {
        &self.nodes
    }
    pub(crate) fn edges(&self) -> &[LockedEdge] {
        &self.edges
    }
}
impl fmt::Debug for LockedGraph {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("LockedGraph")
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PythonLockModel {
    declaration_fingerprint: Option<[u8; 32]>,
    pins: Vec<ContentPin>,
    graphs: Vec<LockedGraph>,
    builds: Vec<(
        PinId,
        crate::packages::python_environment::model::SourceBuildRecord,
    )>,
}
impl PythonLockModel {
    pub(crate) fn with_declaration_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.declaration_fingerprint = Some(fingerprint);
        self
    }
    pub(crate) fn declaration_fingerprint(&self) -> Option<&[u8; 32]> {
        self.declaration_fingerprint.as_ref()
    }
    pub(crate) fn from_parts(
        pins: &[ContentPin],
        graphs: &[LockGraphInput<'_>],
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        Self::from_borrowed_parts(pins.iter(), graphs, limits)
    }
    fn from_borrowed_parts<'a>(
        pins: impl ExactSizeIterator<Item = &'a ContentPin> + Clone,
        graphs: &[LockGraphInput<'_>],
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        preflight_borrowed_parts(pins.clone(), graphs, limits)?;
        let mut by_pin = BTreeMap::new();
        for pin in pins {
            if pin.id != pin_id(&pin.file, pin.digest) {
                return Err(LockModelError::IdentityMismatch);
            }
            let checked = ContentPin::new(&pin.file, pin.digest, pin.size, pin.evidence, limits)?;
            if by_pin.insert(pin.id, checked).is_some() {
                return Err(LockModelError::Duplicate(LockIdentity::Pin));
            }
        }
        let mut by_group = BTreeMap::new();
        let mut used = BTreeSet::new();
        for input in graphs {
            let key = GroupKey::new(
                input.key.config_scope(),
                input.key.group(),
                *input.key.target(),
                limits,
            )?;
            if by_group.contains_key(&key) {
                return Err(LockModelError::Duplicate(LockIdentity::Group));
            }
            let runtime = RuntimePin::new(
                input.runtime.content_tree,
                input.runtime.recipe,
                input.runtime.target,
            )?;
            if runtime.target != key.target {
                return Err(LockModelError::IdentityMismatch);
            }
            let mut nodes = BTreeMap::new();
            for node in input.nodes {
                let name = normalized(node.name)?;
                let version = exact_version(node.version)?;
                if nodes.contains_key(&name) {
                    return Err(LockModelError::Duplicate(LockIdentity::Node));
                }
                let pin = by_pin
                    .get(&node.source_pin)
                    .ok_or(LockModelError::Reference(LockReference::NodePin))?;
                if name != pin.file.project || !same_version(&version, &pin.file.version) {
                    return Err(LockModelError::IdentityMismatch);
                }
                used.insert(node.source_pin);
                nodes.insert(
                    name.clone(),
                    LockedNode {
                        name,
                        version,
                        source_pin: node.source_pin,
                    },
                );
            }
            let mut root_declarations = Vec::new();
            root_declarations
                .try_reserve_exact(input.root_declarations.len())
                .map_err(|_| LockModelError::Limit(LockLimit::Roots))?;
            for declaration in input.root_declarations {
                root_declarations.push((*declaration).to_owned());
            }
            let mut root_indices = BTreeSet::new();
            let mut roots = Vec::new();
            roots
                .try_reserve_exact(input.roots.len())
                .map_err(|_| LockModelError::Limit(LockLimit::Roots))?;
            for root in input.roots {
                if root.declaration_index >= root_declarations.len() {
                    return Err(LockModelError::Reference(LockReference::RootDeclaration));
                }
                if !root_indices.insert(root.declaration_index) {
                    return Err(LockModelError::Duplicate(LockIdentity::RootReference));
                }
                let name = normalized(root.node)?;
                if !nodes.contains_key(&name) {
                    return Err(LockModelError::Reference(LockReference::Root));
                }
                roots.push(LockedRoot {
                    declaration_index: root.declaration_index,
                    node: name,
                });
            }
            roots.sort_by_key(|root| root.declaration_index);
            let mut edges = Vec::new();
            edges
                .try_reserve_exact(input.edges.len())
                .map_err(|_| LockModelError::Limit(LockLimit::Edges))?;
            for edge in input.edges {
                let from = normalized(edge.from)?;
                let to = normalized(edge.to)?;
                if !nodes.contains_key(&from) || !nodes.contains_key(&to) {
                    return Err(LockModelError::Reference(LockReference::Edge));
                }
                edges.push(LockedEdge {
                    from,
                    to,
                    declaration: edge.declaration.into(),
                });
            }
            reachable(nodes.len(), &roots, &edges)?;
            let graph = LockedGraph {
                key: key.clone(),
                semantics: input.semantics,
                runtime,
                root_declarations,
                roots,
                nodes: nodes.into_values().collect(),
                edges,
            };
            by_group.insert(key, graph);
        }
        if used.len() != by_pin.len() {
            return Err(LockModelError::Reference(LockReference::UnusedPin));
        }
        Ok(Self {
            declaration_fingerprint: None,
            pins: by_pin.into_values().collect(),
            graphs: by_group.into_values().collect(),
            builds: Vec::new(),
        })
    }
    pub(crate) fn pins(&self) -> &[ContentPin] {
        &self.pins
    }
    pub(crate) fn graphs(&self) -> &[LockedGraph] {
        &self.graphs
    }
    pub(crate) fn graph(&self, key: &GroupKey) -> Option<&LockedGraph> {
        self.graphs.iter().find(|graph| graph.key() == key)
    }
    pub(crate) fn check_limits(&self, limits: LockModelLimits) -> LockModelResult<()> {
        validate_owned(self, limits)
    }
    pub(crate) fn decode_projection(raw: &str, limits: LockModelLimits) -> LockModelResult<Self> {
        decode::projection(raw, limits)
    }
    pub(crate) fn encode_projection(&self, limits: LockModelLimits) -> LockModelResult<String> {
        encode::projection(self, limits)
    }
}
impl fmt::Debug for PythonLockModel {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("PythonLockModel")
            .field("pins", &self.pins.len())
            .field("graphs", &self.graphs.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Usage {
    utf8: usize,
    nodes: usize,
    references: usize,
}
fn valid_limits(limits: LockModelLimits) -> LockModelResult<()> {
    if [
        limits.max_document_bytes,
        limits.max_pins,
        limits.max_groups,
        limits.max_nodes_per_group,
        limits.max_total_nodes,
        limits.max_edges_per_group,
        limits.max_roots_per_group,
        limits.max_total_references,
        limits.max_field_bytes,
        limits.max_declaration_bytes,
        limits.max_total_utf8_bytes,
    ]
    .contains(&0)
        || limits.max_artifact_bytes == 0
    {
        return Err(LockModelError::InvalidLimits);
    }
    Ok(())
}
fn charge(
    current: &mut usize,
    amount: usize,
    maximum: usize,
    kind: LockLimit,
) -> LockModelResult<()> {
    *current = current
        .checked_add(amount)
        .filter(|total| *total <= maximum)
        .ok_or(LockModelError::Limit(kind))?;
    Ok(())
}
fn count(amount: usize, maximum: usize, kind: LockLimit) -> LockModelResult<()> {
    if amount > maximum {
        Err(LockModelError::Limit(kind))
    } else {
        Ok(())
    }
}
fn text(
    value: &str,
    field: LockField,
    declaration: bool,
    usage: &mut Usage,
    limits: LockModelLimits,
) -> LockModelResult<()> {
    count(
        value.len(),
        if declaration {
            limits.max_declaration_bytes
        } else {
            limits.max_field_bytes
        },
        if declaration {
            LockLimit::DeclarationBytes
        } else {
            LockLimit::FieldBytes
        },
    )?;
    charge(
        &mut usage.utf8,
        value.len(),
        limits.max_total_utf8_bytes,
        LockLimit::TotalUtf8Bytes,
    )?;
    if value
        .chars()
        .any(|ch| ch.is_control() && !(declaration && ch == '\t'))
    {
        return Err(LockModelError::InvalidField(field));
    }
    if declaration {
        if value.is_empty() {
            return Err(LockModelError::InvalidField(field));
        }
        let mut quote = None;
        for ch in value.chars() {
            if let Some(open) = quote {
                if ch == open {
                    quote = None;
                }
                continue;
            }
            match ch {
                '\'' | '"' => quote = Some(ch),
                ';' => break,
                '@' => return Err(LockModelError::UnsupportedSourceKind),
                _ => {}
            }
        }
    }
    Ok(())
}
fn valid_scope(scope: &str) -> LockModelResult<()> {
    if !scope.is_empty()
        && (scope.starts_with('/')
            || scope.contains(['\\', ':'])
            || scope
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | "..")))
    {
        return Err(LockModelError::InvalidField(LockField::Scope));
    }
    Ok(())
}
fn normalized(value: &str) -> LockModelResult<String> {
    NormalizedPythonName::new(value)
        .map(|name| name.as_str().to_owned())
        .map_err(|_| LockModelError::InvalidField(LockField::Project))
}
fn exact_version(value: &str) -> LockModelResult<String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(LockModelError::InvalidField(LockField::Version));
    }
    Pep440::parse_version(value)
        .map(|version| version.to_string())
        .ok_or(LockModelError::InvalidField(LockField::Version))
}
fn same_version(left: &str, right: &str) -> bool {
    match (Pep440::parse_version(left), Pep440::parse_version(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}
fn group_shape(
    declarations: usize,
    roots: usize,
    nodes: usize,
    edges: usize,
    limits: LockModelLimits,
) -> LockModelResult<()> {
    count(nodes, limits.max_nodes_per_group, LockLimit::Nodes)?;
    count(declarations, limits.max_roots_per_group, LockLimit::Roots)?;
    count(roots, limits.max_roots_per_group, LockLimit::Roots)?;
    count(edges, limits.max_edges_per_group, LockLimit::Edges)
}

fn preflight_borrowed_parts<'a>(
    pins: impl ExactSizeIterator<Item = &'a ContentPin>,
    graphs: &[LockGraphInput<'_>],
    limits: LockModelLimits,
) -> LockModelResult<()> {
    valid_limits(limits)?;
    count(pins.len(), limits.max_pins, LockLimit::Pins)?;
    count(graphs.len(), limits.max_groups, LockLimit::Groups)?;
    let mut usage = Usage::default();
    for pin in pins {
        for (value, field) in [
            (pin.file.registry_id.as_str(), LockField::Registry),
            (pin.file.project.as_str(), LockField::Project),
            (pin.file.version.as_str(), LockField::Version),
            (pin.file.filename.as_str(), LockField::Filename),
        ] {
            text(value, field, false, &mut usage, limits)?;
        }
        if pin.size > limits.max_artifact_bytes {
            return Err(LockModelError::Limit(LockLimit::ArtifactBytes));
        }
    }
    for graph in graphs {
        group_shape(
            graph.root_declarations.len(),
            graph.roots.len(),
            graph.nodes.len(),
            graph.edges.len(),
            limits,
        )?;
        charge(
            &mut usage.nodes,
            graph.nodes.len(),
            limits.max_total_nodes,
            LockLimit::TotalNodes,
        )?;
        charge(
            &mut usage.references,
            graph.root_declarations.len(),
            limits.max_total_references,
            LockLimit::References,
        )?;
        let root_refs = graph
            .roots
            .len()
            .checked_mul(2)
            .ok_or(LockModelError::Limit(LockLimit::References))?;
        charge(
            &mut usage.references,
            root_refs,
            limits.max_total_references,
            LockLimit::References,
        )?;
        charge(
            &mut usage.references,
            graph.nodes.len(),
            limits.max_total_references,
            LockLimit::References,
        )?;
        let edge_refs = graph
            .edges
            .len()
            .checked_mul(2)
            .ok_or(LockModelError::Limit(LockLimit::References))?;
        charge(
            &mut usage.references,
            edge_refs,
            limits.max_total_references,
            LockLimit::References,
        )?;
        text(
            graph.key.config_scope(),
            LockField::Scope,
            false,
            &mut usage,
            limits,
        )?;
        text(
            graph.key.group(),
            LockField::Group,
            false,
            &mut usage,
            limits,
        )?;
        for node in graph.nodes {
            text(node.name, LockField::Project, false, &mut usage, limits)?;
            text(node.version, LockField::Version, false, &mut usage, limits)?;
            // Canonical PEP440 spelling can expand (e.g. implicit post/dev).
            // Charge that expansion before creating any owned graph records.
            let canonical = exact_version(node.version)?;
            count(
                canonical.len(),
                limits.max_field_bytes,
                LockLimit::FieldBytes,
            )?;
            if canonical.len() > node.version.len() {
                charge(
                    &mut usage.utf8,
                    canonical.len() - node.version.len(),
                    limits.max_total_utf8_bytes,
                    LockLimit::TotalUtf8Bytes,
                )?;
            }
        }
        for declaration in graph.root_declarations {
            text(
                declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
        }
        for root in graph.roots {
            text(root.node, LockField::Project, false, &mut usage, limits)?;
        }
        for edge in graph.edges {
            text(edge.from, LockField::Project, false, &mut usage, limits)?;
            text(edge.to, LockField::Project, false, &mut usage, limits)?;
            text(
                edge.declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
        }
    }
    Ok(())
}
fn reachable(nodes: usize, roots: &[LockedRoot], edges: &[LockedEdge]) -> LockModelResult<()> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        adjacency.entry(&edge.from).or_default().push(&edge.to);
    }
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    for root in roots {
        queue.push_back(root.node.as_str());
    }
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name) {
            continue;
        }
        if let Some(children) = adjacency.get(name) {
            queue.extend(children.iter().copied());
        }
    }
    if seen.len() != nodes {
        return Err(LockModelError::Reference(LockReference::UnreachableNode));
    }
    Ok(())
}

fn validate_owned(model: &PythonLockModel, limits: LockModelLimits) -> LockModelResult<()> {
    valid_limits(limits)?;
    count(model.pins.len(), limits.max_pins, LockLimit::Pins)?;
    count(model.graphs.len(), limits.max_groups, LockLimit::Groups)?;
    let mut usage = Usage::default();
    for pin in &model.pins {
        for (value, field) in [
            (pin.file.registry_id.as_str(), LockField::Registry),
            (pin.file.project.as_str(), LockField::Project),
            (pin.file.version.as_str(), LockField::Version),
            (pin.file.filename.as_str(), LockField::Filename),
        ] {
            text(value, field, false, &mut usage, limits)?;
        }
        if pin.size > limits.max_artifact_bytes {
            return Err(LockModelError::Limit(LockLimit::ArtifactBytes));
        }
    }
    for graph in &model.graphs {
        group_shape(
            graph.root_declarations.len(),
            graph.roots.len(),
            graph.nodes.len(),
            graph.edges.len(),
            limits,
        )?;
        charge(
            &mut usage.nodes,
            graph.nodes.len(),
            limits.max_total_nodes,
            LockLimit::TotalNodes,
        )?;
        charge(
            &mut usage.references,
            graph.root_declarations.len(),
            limits.max_total_references,
            LockLimit::References,
        )?;
        let root_refs = graph
            .roots
            .len()
            .checked_mul(2)
            .ok_or(LockModelError::Limit(LockLimit::References))?;
        charge(
            &mut usage.references,
            root_refs,
            limits.max_total_references,
            LockLimit::References,
        )?;
        charge(
            &mut usage.references,
            graph.nodes.len(),
            limits.max_total_references,
            LockLimit::References,
        )?;
        let references = graph
            .edges
            .len()
            .checked_mul(2)
            .ok_or(LockModelError::Limit(LockLimit::References))?;
        charge(
            &mut usage.references,
            references,
            limits.max_total_references,
            LockLimit::References,
        )?;
        text(
            &graph.key.config_scope,
            LockField::Scope,
            false,
            &mut usage,
            limits,
        )?;
        text(
            &graph.key.group,
            LockField::Group,
            false,
            &mut usage,
            limits,
        )?;
        for node in &graph.nodes {
            text(&node.name, LockField::Project, false, &mut usage, limits)?;
            text(&node.version, LockField::Version, false, &mut usage, limits)?;
        }
        for declaration in &graph.root_declarations {
            text(
                declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
        }
        for root in &graph.roots {
            text(&root.node, LockField::Project, false, &mut usage, limits)?;
        }
        for edge in &graph.edges {
            text(&edge.from, LockField::Project, false, &mut usage, limits)?;
            text(&edge.to, LockField::Project, false, &mut usage, limits)?;
            text(
                &edge.declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
        }
    }
    let mut pins = BTreeMap::new();
    for pin in &model.pins {
        let checked = ContentPin::new(&pin.file, pin.digest, pin.size, pin.evidence, limits)?;
        if pin.id != checked.id {
            return Err(LockModelError::IdentityMismatch);
        }
        if pins.insert(pin.id, pin).is_some() {
            return Err(LockModelError::Duplicate(LockIdentity::Pin));
        }
    }
    let mut groups = BTreeSet::new();
    let mut used = BTreeSet::new();
    for graph in &model.graphs {
        valid_scope(&graph.key.config_scope)?;
        if graph.key.group.is_empty() {
            return Err(LockModelError::InvalidField(LockField::Group));
        }
        if !groups.insert(&graph.key) {
            return Err(LockModelError::Duplicate(LockIdentity::Group));
        }
        if graph.runtime.recipe != 1 {
            return Err(LockModelError::UnsupportedRuntimeRecipe);
        }
        if graph.runtime.target != graph.key.target {
            return Err(LockModelError::IdentityMismatch);
        }
        let mut nodes = BTreeSet::new();
        for node in &graph.nodes {
            if normalized(&node.name)? != node.name || exact_version(&node.version)? != node.version
            {
                return Err(LockModelError::IdentityMismatch);
            }
            if !nodes.insert(node.name.as_str()) {
                return Err(LockModelError::Duplicate(LockIdentity::Node));
            }
            let pin = pins
                .get(&node.source_pin)
                .ok_or(LockModelError::Reference(LockReference::NodePin))?;
            if pin.file.project != node.name || !same_version(&pin.file.version, &node.version) {
                return Err(LockModelError::IdentityMismatch);
            }
            used.insert(node.source_pin);
        }
        let mut root_indices = BTreeSet::new();
        for root in &graph.roots {
            if root.declaration_index >= graph.root_declarations.len() {
                return Err(LockModelError::Reference(LockReference::RootDeclaration));
            }
            if !root_indices.insert(root.declaration_index) {
                return Err(LockModelError::Duplicate(LockIdentity::RootReference));
            }
            if !nodes.contains(root.node.as_str()) {
                return Err(LockModelError::Reference(LockReference::Root));
            }
        }
        for edge in &graph.edges {
            if !nodes.contains(edge.from.as_str()) || !nodes.contains(edge.to.as_str()) {
                return Err(LockModelError::Reference(LockReference::Edge));
            }
        }
        reachable(nodes.len(), &graph.roots, &graph.edges)?;
    }
    source_builds::validate(model, &mut used, &mut usage, limits)?;
    if used.len() != pins.len() {
        return Err(LockModelError::Reference(LockReference::UnusedPin));
    }
    Ok(())
}

fn close_owned(
    mut model: PythonLockModel,
    limits: LockModelLimits,
) -> LockModelResult<PythonLockModel> {
    validate_owned(&model, limits)?;
    model.pins.sort_by_key(|pin| pin.id);
    for graph in &mut model.graphs {
        graph.roots.sort_by_key(|root| root.declaration_index);
        graph
            .nodes
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    model.graphs.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(model)
}

pub(crate) struct PythonLockSeed {
    limits: LockModelLimits,
}
impl PythonLockSeed {
    pub(crate) fn new(limits: LockModelLimits) -> Self {
        Self { limits }
    }
}
impl<'de> DeserializeSeed<'de> for PythonLockSeed {
    type Value = PythonLockModel;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        decode::table(deserializer, self.limits)
    }
}

#[path = "lock_model/source_builds.rs"]
mod source_builds;

#[path = "lock_model/replace_group.rs"]
mod replace_group;

#[cfg(test)]
#[path = "lock_model/tests.rs"]
mod tests;
