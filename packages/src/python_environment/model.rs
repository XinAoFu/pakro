//! Pure environment catalog state. No filesystem or publication authority.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{ContextCompat, Result, ensure};
use pakro_store::Digest;
use uuid::Uuid;

#[path = "model/codec.rs"]
mod codec;
pub(crate) use codec::CatalogCodecLimits;

#[path = "model/native_build.rs"]
mod native_build;
pub(crate) use native_build::{MAX_NATIVE_JSON_BYTES, NativeProgramRecord, NativeSdkRecord, NativeToolchainRecord};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelLimits {
    pub max_generation_records: usize,
    pub max_selection_slots: usize,
    pub max_refs_per_generation: usize,
    pub max_total_references: usize,
    pub max_group_bytes: usize,
    pub max_transactions: usize,
}
impl Default for ModelLimits {
    fn default() -> Self {
        Self {
            max_generation_records: 1_048_576,
            max_selection_slots: 65_536,
            max_refs_per_generation: 65_536,
            max_total_references: 1_048_576,
            max_group_bytes: 1024,
            max_transactions: 1_048_576,
        }
    }
}
impl ModelLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            self.max_generation_records != 0
                && self.max_selection_slots != 0
                && self.max_refs_per_generation != 0
                && self.max_total_references != 0
                && self.max_group_bytes != 0
                && self.max_transactions != 0,
            "environment model limits must be nonzero"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SelectionKey {
    project: Uuid,
    group: String,
    target: [u8; 32],
}
impl SelectionKey {
    pub(crate) fn new(
        project: Uuid,
        group: &str,
        target: [u8; 32],
        limits: ModelLimits,
    ) -> Result<Self> {
        Self::validate_values(project, group, limits)?;
        Ok(Self {
            project,
            group: group.into(),
            target,
        })
    }
    fn validate(&self, limits: ModelLimits) -> Result<()> {
        Self::validate_values(self.project, &self.group, limits)
    }
    fn validate_values(project: Uuid, group: &str, limits: ModelLimits) -> Result<()> {
        limits.validate()?;
        ensure!(!project.is_nil(), "environment project identity is nil");
        ensure!(
            !group.is_empty()
                && group.len() <= limits.max_group_bytes
                && !group.chars().any(char::is_control),
            "invalid environment group identity or byte limit"
        );
        Ok(())
    }
    pub(crate) fn project(&self) -> Uuid {
        self.project
    }
    pub(crate) fn group(&self) -> &str {
        &self.group
    }
    pub(crate) fn target(&self) -> &[u8; 32] {
        &self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InputKind {
    Runtime,
    Artifact,
    BuildInput,
    Language,
    Package,
    BuildRequest,
    BuildOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct InputReference {
    kind: InputKind,
    digest: String,
}
impl InputReference {
    pub(crate) fn new(kind: InputKind, digest: Digest) -> Self {
        Self {
            kind,
            digest: digest.to_string(),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let digest: Digest = self
            .digest
            .parse()
            .map_err(|_| eyre::eyre!("invalid environment input digest"))?;
        ensure!(
            digest.to_string() == self.digest,
            "environment input digest is not canonical"
        );
        if matches!(
            self.kind,
            InputKind::Language | InputKind::Package | InputKind::BuildRequest
        ) {
            ensure!(
                matches!(digest, Digest::Sha256(_)),
                "typed content keys require SHA256 identities"
            );
        }
        Ok(())
    }
    pub(crate) fn kind(&self) -> InputKind {
        self.kind
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

/// Portable source facts are descriptions, not authorization to fetch bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageSourcePin {
    pub registry: String,
    pub base: [u8; 32],
    pub filename: String,
    pub size: u64,
    pub advertised: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<SourceBuildRecord>,
    /// Serialized portable Conda record; absent for the existing PyPI format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conda: Option<String>,
}

/// Portable descriptions of actual fixed build inputs, never fetch/IO authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceBuildInput {
    pub name: String,
    pub version: String,
    pub source_pin: String,
    pub registry: String,
    pub base: [u8; 32],
    pub source_filename: String,
    pub source_digest: String,
    pub source_size: u64,
    pub source_is_sdist: bool,
    pub advertised: bool,
    pub output_filename: String,
    pub output_digest: String,
    pub output_size: u64,
}

/// A source pin and a built output are separate facts, including when the
/// resulting wheel has the same distribution name/version as its source.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceBuildObservation {
    pub initial_requirements: Vec<String>,
    pub initial_inputs: Vec<SourceBuildInput>,
    pub dynamic_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceBuildRecord {
    pub source_digest: String,
    pub request_id: [u8; 32],
    pub recipe: u32,
    pub backend: String,
    pub inputs: Vec<SourceBuildInput>,
    pub output_filename: String,
    pub output_digest: String,
    pub output_size: u64,
    pub metadata_sha256: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<SourceBuildObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "native_build::optional_json")]
    pub native: Option<NativeToolchainRecord>,
}
#[path = "model/source_build.rs"]
mod source_build;
/// Exact source-backed dependency facts, stored with their shared layout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageGraphNode {
    pub name: String,
    pub version: String,
    pub instance: [u8; 32],
    pub source_pin: String,
    pub artifact: String,
    pub pin: PackageSourcePin,
}
impl PackageGraphNode {
    pub(crate) fn from_conda_record(
        record: &rattler_conda_types::RepoDataRecord,
        instance: [u8; 32],
    ) -> Result<Self> {
        let digest = record.package_record.sha256.context("Conda package SHA256 is missing")?;
        let size = record.package_record.size.context("Conda package size is missing")?;
        Ok(Self {
            name: record.package_record.name.as_normalized().to_owned(),
            version: record.package_record.version.to_string(),
            instance,
            source_pin: crate::libraries::package::conda::record_source(record)?,
            artifact: Digest::Sha256(digest.into()).to_string(),
            pin: PackageSourcePin {
                registry: record.channel.clone().context("Conda package channel is missing")?,
                base: [0; 32],
                filename: record.identifier.to_string(),
                size,
                advertised: true,
                build: None,
                conda: Some(serde_json::to_string(record)?),
            },
        })
    }

    pub(crate) fn conda_record(&self) -> Result<Option<rattler_conda_types::RepoDataRecord>> {
        ensure!(self.pin.conda.as_ref().is_none_or(|value| value.len() <= 1024 * 1024), "Conda package record exceeds the catalog limit");
        self.pin.conda.as_deref().map(serde_json::from_str).transpose().map_err(Into::into)
    }

    pub(crate) fn content_pin(
        &self,
    ) -> Result<crate::packages::python_resolution::lock_model::ContentPin> {
        use crate::packages::python_resolution::lock_model::{
            ContentPin, LockModelLimits, PinEvidence, RegistryArtifactKind, RegistryFileRef,
            RegistryFileRefInput,
        };
        let (kind, digest) = match &self.pin.build {
            Some(build) => {
                build.validate_for_node(&self.name, &self.version, &self.artifact)?;
                (RegistryArtifactKind::Sdist, build.source_digest.as_str())
            }
            None => (RegistryArtifactKind::Wheel, self.artifact.as_str()),
        };
        let file = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: &self.pin.registry,
                canonical_base_fingerprint: self.pin.base,
                project: &self.name,
                version: &self.version,
                filename: &self.pin.filename,
                kind,
            },
            LockModelLimits::default(),
        )?;
        let pin = ContentPin::new(
            &file,
            digest.parse()?,
            self.pin.size,
            if self.pin.advertised {
                PinEvidence::Advertised
            } else {
                PinEvidence::ObservedDuringFreshResolution
            },
            LockModelLimits::default(),
        )?;
        ensure!(
            hex::encode(pin.id().as_bytes()) == self.source_pin,
            "package source facts disagree with their pin identity"
        );
        Ok(pin)
    }
    pub(crate) fn source_build_input(&self) -> Result<SourceBuildInput> {
        let pin = self.content_pin()?;
        let (output_filename, output_size) = self
            .pin
            .build
            .as_ref()
            .map_or((self.pin.filename.as_str(), self.pin.size), |build| {
                (build.output_filename.as_str(), build.output_size)
            });
        Ok(SourceBuildInput {
            name: self.name.clone(),
            version: self.version.clone(),
            source_pin: self.source_pin.clone(),
            registry: self.pin.registry.clone(),
            base: self.pin.base,
            source_filename: self.pin.filename.clone(),
            source_digest: pin.digest().to_string(),
            source_size: pin.size(),
            source_is_sdist: self.pin.build.is_some(),
            advertised: self.pin.advertised,
            output_filename: output_filename.to_owned(),
            output_digest: self.artifact.clone(),
            output_size,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageGraphEdge {
    pub from: usize,
    pub to: usize,
    pub declaration: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageGraphRecord {
    pub roots: Vec<usize>,
    pub nodes: Vec<PackageGraphNode>,
    pub edges: Vec<PackageGraphEdge>,
}
impl PackageGraphRecord {
    /// One identity for the closed source graph, independent of instance
    /// placement and whether its identical bytes had an advertised digest.
    pub(crate) fn fingerprint(&self) -> Result<[u8; 32]> {
        use sha2::{Digest as _, Sha256};
        let mut projection = self.clone();
        for node in &mut projection.nodes {
            node.instance = [0; 32];
            node.pin.advertised = false;
            if let Some(build) = &mut node.pin.build {
                build.normalize();
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"pakro.closed-shared-package-graph/v1\0");
        hash.update(serde_json::to_vec(&projection)?);
        Ok(hash.finalize().into())
    }

    fn validate(&self, limits: ModelLimits) -> Result<()> {
        ensure!(
            self.roots.len() <= limits.max_refs_per_generation
                && self.nodes.len() <= limits.max_refs_per_generation
                && self.edges.len() <= limits.max_total_references,
            "package graph collection limit exceeded"
        );
        let mut names = BTreeSet::new();
        let mut instances = BTreeSet::new();
        for node in &self.nodes {
            if let Some(record) = node.conda_record()? {
                ensure!(node.name == record.package_record.name.as_normalized()
                    && node.version == record.package_record.version.to_string()
                    && node.source_pin == crate::libraries::package::conda::record_source(&record)?
                    && record.package_record.sha256.is_some_and(|sha| node.artifact == Digest::Sha256(sha.into()).to_string())
                    && node.pin.filename == record.identifier.to_string()
                    && record.package_record.size == Some(node.pin.size)
                    && record.channel.as_deref() == Some(node.pin.registry.as_str())
                    && node.pin.base == [0; 32] && node.pin.advertised
                    && node.pin.build.is_none()
                    && names.insert((true, &node.name)) && instances.insert(node.instance),
                    "invalid Conda package graph node");
                continue;
            }
            ensure!(
                !node.name.is_empty()
                    && node.name.len() <= 1024
                    && node.version.len() <= 1024
                    && !node.version.is_empty()
                    && pakro_index::python_source::NormalizedPythonName::new(&node.name).is_ok_and(|name|name.as_str()==node.name)
                    && <pakro_resolver::solve::Pep440 as pakro_resolver::solve::VersionAdapter>::parse_version(&node.version).is_some()
                    && names.insert((false, &node.name))
                    && instances.insert(node.instance),
                "invalid package graph node"
            );
            ensure!(
                node.source_pin.len() == 64
                    && node.source_pin.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid package source pin"
            );
            node.content_pin()?;
            let digest: Digest = node.artifact.parse()?;
            ensure!(
                digest.to_string() == node.artifact,
                "noncanonical package artifact digest"
            );
        }
        ensure!(
            self.roots.iter().all(|index| *index < self.nodes.len()),
            "package graph root is missing"
        );
        let mut reached = BTreeSet::new();
        let mut pending = self.roots.clone();
        let mut adjacent = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            ensure!(
                edge.from < self.nodes.len()
                    && edge.to < self.nodes.len()
                    && !edge.declaration.is_empty()
                    && edge.declaration.len() <= 16 * 1024,
                "invalid package graph edge"
            );
            adjacent[edge.from].push(edge.to);
        }
        while let Some(index) = pending.pop() {
            if reached.insert(index) {
                pending.extend(adjacent[index].iter().copied());
            }
        }
        ensure!(
            reached.len() == self.nodes.len(),
            "package graph has unreachable nodes"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationRecord {
    allocation: String,
    target: [u8; 32],
    shared_layout: Option<[u8; 32]>,
    graph: [u8; 32],
    receipt: [u8; 32],
    inputs: Vec<InputReference>,
    package_graph: Option<PackageGraphRecord>,
    source_locators: Option<BTreeMap<String, String>>,
}
impl GenerationRecord {
    pub(crate) fn new(
        allocation: &str,
        target: [u8; 32],
        graph: [u8; 32],
        receipt: [u8; 32],
        mut inputs: Vec<InputReference>,
        limits: ModelLimits,
    ) -> Result<Self> {
        limits.validate()?;
        Self::validate_allocation(allocation)?;
        // Bound the submitted sequence before canonicalizing duplicate inputs.
        ensure!(
            inputs.len() <= limits.max_refs_per_generation,
            "environment input reference limit exceeded"
        );
        inputs.sort();
        inputs.dedup();
        let value = Self {
            allocation: allocation.into(),
            target,
            shared_layout: None,
            graph,
            receipt,
            inputs,
            package_graph: None,
            source_locators: None,
        };
        value.validate(limits)?;
        Ok(value)
    }
    fn validate(&self, limits: ModelLimits) -> Result<()> {
        limits.validate()?;
        Self::validate_allocation(&self.allocation)?;
        ensure!(
            self.inputs.len() <= limits.max_refs_per_generation,
            "environment input reference limit exceeded"
        );
        for input in &self.inputs {
            input.validate()?;
        }
        if let Some(graph) = &self.package_graph {
            ensure!(
                self.shared_layout.is_some(),
                "package graph requires a shared layout"
            );
            graph.validate(limits)?;
            ensure!(
                graph.fingerprint()? == self.graph,
                "package graph facts differ from their committed graph identity"
            );
            let actual: BTreeSet<_> = self
                .inputs
                .iter()
                .filter(|input| input.kind == InputKind::Package)
                .map(|input| input.digest.as_str())
                .collect();
            let expected: BTreeSet<_> = graph
                .nodes
                .iter()
                .map(|node| Digest::Sha256(node.instance).to_string())
                .collect();
            ensure!(
                actual == expected.iter().map(String::as_str).collect(),
                "package graph nodes and typed package references differ"
            );
        }
        if let Some(locators) = &self.source_locators {
            let graph = self.package_graph.as_ref().context("source locators need a package graph")?;
            let pins = graph.nodes.iter().filter(|node| node.pin.conda.is_none())
                .map(|node| (node.source_pin.as_str(), node)).collect::<BTreeMap<_, _>>();
            ensure!(locators.len() == pins.len() && locators.len() <= limits.max_refs_per_generation,
                "source locators differ from the selected package graph");
            for (id, url) in locators {
                let node = pins.get(id.as_str()).context("source locator references an unknown package")?;
                ensure!(crate::packages::python_resolution::lock_model::valid_registry_url(url, &node.pin.filename),
                    "invalid catalog source locator");
            }
        }
        if self.shared_layout.is_some() {
            ensure!(
                self.inputs
                    .iter()
                    .filter(|input| input.kind == InputKind::Language)
                    .count()
                    == 1,
                "shared layout must retain exactly one typed language identity"
            );
            ensure!(
                self.inputs
                    .iter()
                    .all(|input| !matches!(input.kind, InputKind::Runtime | InputKind::BuildInput)),
                "shared layout cannot infer typed keys from legacy input digests"
            );
        }
        Ok(())
    }
    fn validate_allocation(allocation: &str) -> Result<()> {
        let bytes = allocation.as_bytes();
        ensure!(
            bytes.len() == 34
                && bytes.starts_with(b"g-")
                && bytes[2..]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
            "invalid environment allocation identifier"
        );
        Ok(())
    }
    pub(crate) fn allocation(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn target(&self) -> &[u8; 32] {
        &self.target
    }
    pub(crate) fn shared_layout(&self) -> Option<&[u8; 32]> {
        self.shared_layout.as_ref()
    }
    pub(crate) fn new_shared(
        allocation: &str,
        target: [u8; 32],
        graph: [u8; 32],
        receipt: [u8; 32],
        shared_layout: [u8; 32],
        inputs: Vec<InputReference>,
        limits: ModelLimits,
    ) -> Result<Self> {
        let mut record = Self::new(allocation, target, graph, receipt, inputs, limits)?;
        record.shared_layout = Some(shared_layout);
        record.validate(limits)?;
        Ok(record)
    }
    pub(crate) fn with_package_graph(
        self,
        graph: PackageGraphRecord,
        limits: ModelLimits,
    ) -> Result<Self> {
        let mut record = self;
        record.package_graph = Some(graph);
        record.validate(limits)?;
        Ok(record)
    }
    pub(crate) fn with_source_locators(mut self, locators: BTreeMap<String, String>, limits: ModelLimits) -> Result<Self> {
        self.source_locators = Some(locators);
        self.validate(limits)?;
        Ok(self)
    }
    pub(crate) fn source_locators(&self) -> Option<&BTreeMap<String, String>> {
        self.source_locators.as_ref()
    }
    pub(crate) fn package_graph(&self) -> Option<&PackageGraphRecord> {
        self.package_graph.as_ref()
    }
    pub(crate) fn graph(&self) -> &[u8; 32] {
        &self.graph
    }
    pub(crate) fn receipt(&self) -> &[u8; 32] {
        &self.receipt
    }
    pub(crate) fn inputs(&self) -> &[InputReference] {
        &self.inputs
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SelectionStamp {
    revision: u64,
    generation: Option<String>,
    /// Exact shared-language identity of a language-only selection. It is set
    /// exactly when `generation` is None and the selection is not a clear.
    language: Option<String>,
    transaction: Uuid,
}
impl SelectionStamp {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }
    pub(crate) fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
    pub(crate) fn transaction(&self) -> Uuid {
        self.transaction
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyResult {
    Applied,
    AlreadyApplied,
}

#[derive(Debug)]
pub(crate) struct CatalogModel {
    limits: ModelLimits,
    generations: BTreeMap<String, GenerationRecord>,
    // Derived index into generations, never a separate selection authority.
    shared_layouts: BTreeMap<[u8; 32], String>,
    selections: BTreeMap<SelectionKey, SelectionStamp>,
    transactions: BTreeMap<Uuid, AppliedChange>,
    // Registered project facts in the same catalog: identity, bound location
    // and the declaration observation behind the current selection.
    projects: BTreeMap<Uuid, ProjectRegistration>,
    total_references: usize,
}

/// The minimal persistent project fact of a language-only selection. The root
/// is the canonical local binding; the declaration is a SHA256 digest of the
/// declarations observed for the committed selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRegistration {
    root: String,
    declaration: String,
}
impl ProjectRegistration {
    /// Canonical shape: an absolute local root binding and a SHA256 hex
    /// digest of the observed declarations. None on any deviation.
    pub(crate) fn from_facts(root: &str, declaration: &str) -> Option<Self> {
        (!root.is_empty()
            && root.len() <= 4096
            && root.starts_with('/')
            && !root.as_bytes().contains(&0)
            && !root.chars().any(char::is_control)
            && declaration.len() == 64
            && declaration.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Self {
            root: root.to_owned(),
            declaration: declaration.to_owned(),
        })
    }
    pub(crate) fn root(&self) -> &str {
        &self.root
    }
    pub(crate) fn declaration(&self) -> &str {
        &self.declaration
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedChange {
    key: SelectionKey,
    expected: SelectionStamp,
    generation: Option<String>,
    language: Option<String>,
}
impl CatalogModel {
    pub(crate) fn new(limits: ModelLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            generations: BTreeMap::new(),
            shared_layouts: BTreeMap::new(),
            selections: BTreeMap::new(),
            transactions: BTreeMap::new(),
            projects: BTreeMap::new(),
            total_references: 0,
        })
    }
    /// Registers or refreshes the minimal project fact in this same catalog.
    /// A registered identity may not silently move to another location; a new
    /// declaration observation updates the digest with the next selection.
    pub(crate) fn register_project(
        &mut self,
        project: Uuid,
        root: &str,
        declaration: &str,
    ) -> Result<()> {
        self.limits.validate()?;
        ensure!(!project.is_nil(), "catalog project identity is nil");
        let fact = ProjectRegistration::from_facts(root, declaration)
            .ok_or_else(|| eyre::eyre!("invalid catalog project location or declaration digest"))?;
        if let Some(existing) = self.projects.get(&project) {
            ensure!(
                existing.root == fact.root,
                "catalog project location conflicts with registered facts"
            );
        } else {
            ensure!(
                self.projects.len() < self.limits.max_selection_slots,
                "catalog project registration limit exceeded"
            );
        }
        self.projects.insert(project, fact);
        Ok(())
    }
    pub(crate) fn project_registration(&self, project: Uuid) -> Option<&ProjectRegistration> {
        self.projects.get(&project)
    }
    pub(crate) fn registered_projects(&self) -> impl Iterator<Item = (Uuid, &ProjectRegistration)> {
        self.projects.iter().map(|(project, fact)| (*project, fact))
    }
    /// Removes only the registration. Cleared stamps and their complete nonce
    /// histories remain to reject preparations predating the explicit forget.
    pub(crate) fn unregister_project(&mut self, project: Uuid) -> Result<bool> {
        ensure!(!project.is_nil(), "catalog project identity is nil");
        ensure!(
            !self.selections.iter().any(|(key, stamp)| {
                key.project() == project
                    && (stamp.generation().is_some() || stamp.language().is_some())
            }),
            "cannot forget a project registration with active selections"
        );
        Ok(self.projects.remove(&project).is_some())
    }
    pub(crate) fn limits(&self) -> ModelLimits {
        self.limits
    }
    pub(crate) fn selection_slots(&self) -> impl Iterator<Item = (&SelectionKey, &SelectionStamp)> {
        self.selections.iter()
    }
    pub(crate) fn register_generation(&mut self, record: GenerationRecord) -> Result<()> {
        record.validate(self.limits)?;
        if let Some(existing) = self.generations.get(record.allocation()) {
            ensure!(
                existing == &record,
                "environment generation record conflicts with registered facts"
            );
            return Ok(());
        }
        if let Some(layout) = record.shared_layout() {
            ensure!(
                !self.shared_layouts.contains_key(layout),
                "shared layout is already registered to another allocation"
            );
        }
        ensure!(
            self.generations.len() < self.limits.max_generation_records,
            "environment generation record limit exceeded"
        );
        let total = self
            .total_references
            .checked_add(record.inputs.len())
            .filter(|total| *total <= self.limits.max_total_references)
            .ok_or_else(|| eyre::eyre!("environment total input reference limit exceeded"))?;
        if let Some(layout) = record.shared_layout() {
            self.shared_layouts
                .insert(*layout, record.allocation.clone());
        }
        self.generations.insert(record.allocation.clone(), record);
        self.total_references = total;
        Ok(())
    }
    pub(crate) fn register_shared_generation(
        &mut self,
        record: GenerationRecord,
    ) -> Result<String> {
        record.validate(self.limits)?;
        let layout = record.shared_layout().ok_or_else(|| {
            eyre::eyre!("shared registration requires an explicit shared layout identity")
        })?;
        if let Some(existing) = self.generation_for_shared_layout(layout) {
            ensure!(existing.target == record.target
                && existing.graph == record.graph
                && existing.receipt == record.receipt
                && existing.inputs == record.inputs
                && existing.package_graph == record.package_graph,
                "shared layout identity conflicts with registered content facts");
            let allocation = existing.allocation.clone();
            if existing.source_locators.is_none() && record.source_locators.is_some() {
                self.generations.get_mut(&allocation).expect("existing shared generation")
                    .source_locators = record.source_locators;
            }
            return Ok(allocation);
        }
        if let Some(existing) = self.generations.get(record.allocation()) {
            ensure!(existing == &record, "environment allocation conflicts with registered facts");
        }
        let allocation = record.allocation.clone();
        self.register_generation(record)?;
        Ok(allocation)
    }
    pub(crate) fn generation_for_shared_layout(
        &self,
        layout: &[u8; 32],
    ) -> Option<&GenerationRecord> {
        self.shared_layouts
            .get(layout)
            .and_then(|allocation| self.generations.get(allocation))
    }
    pub(crate) fn applied_stamp(&self, transaction: Uuid) -> Option<SelectionStamp> {
        let change = self.transactions.get(&transaction)?;
        Some(SelectionStamp {
            revision: change.expected.revision.checked_add(1)?,
            generation: change.generation.clone(),
            language: change.language.clone(),
            transaction,
        })
    }
    pub(crate) fn generations(&self) -> impl Iterator<Item = &GenerationRecord> {
        self.generations.values()
    }
    pub(crate) fn generation(&self, allocation: &str) -> Option<&GenerationRecord> {
        self.generations.get(allocation)
    }
    pub(crate) fn applied_transaction(
        &self,
        transaction: Uuid,
    ) -> Option<(&SelectionKey, &SelectionStamp, Option<&str>)> {
        self.transactions
            .get(&transaction)
            .map(|change| (&change.key, &change.expected, change.generation.as_deref()))
    }
    pub(crate) fn snapshot(&self, key: &SelectionKey) -> SelectionStamp {
        self.selections.get(key).cloned().unwrap_or_default()
    }
    pub(crate) fn apply_selection(
        &mut self,
        key: &SelectionKey,
        expected: &SelectionStamp,
        generation: Option<&str>,
        transaction: Uuid,
    ) -> Result<ApplyResult> {
        self.apply_stamp(key, expected, generation, None, transaction)
    }
    /// Commits a language-only selection: an exact shared-language identity
    /// with no package generation. It shares the revision, CAS and nonce
    /// semantics of generation selections and fabricates no layout facts.
    pub(crate) fn apply_language_selection(
        &mut self,
        key: &SelectionKey,
        expected: &SelectionStamp,
        language: &str,
        transaction: Uuid,
    ) -> Result<ApplyResult> {
        let digest: Digest = language
            .parse()
            .map_err(|_| eyre::eyre!("invalid language selection identity"))?;
        ensure!(
            matches!(digest, Digest::Sha256(_)) && digest.to_string() == language,
            "language selection identity is not a canonical SHA256 digest"
        );
        self.apply_stamp(key, expected, None, Some(language), transaction)
    }
    fn apply_stamp(
        &mut self,
        key: &SelectionKey,
        expected: &SelectionStamp,
        generation: Option<&str>,
        language: Option<&str>,
        transaction: Uuid,
    ) -> Result<ApplyResult> {
        key.validate(self.limits)?;
        ensure!(
            !transaction.is_nil(),
            "environment transaction identity is nil"
        );
        let change = AppliedChange {
            key: key.clone(),
            expected: expected.clone(),
            generation: generation.map(str::to_owned),
            language: language.map(str::to_owned),
        };
        if let Some(applied) = self.transactions.get(&transaction) {
            ensure!(
                applied == &change,
                "environment transaction identity was reused for another change"
            );
            return Ok(ApplyResult::AlreadyApplied);
        }
        ensure!(
            self.snapshot(key) == *expected,
            "environment selection changed since its expected revision"
        );
        if let Some(allocation) = generation {
            let record = self
                .generations
                .get(allocation)
                .ok_or_else(|| eyre::eyre!("environment generation record is missing"))?;
            ensure!(
                record.target() == key.target(),
                "environment generation target does not match selection"
            );
        }
        let revision = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("environment selection revision overflow"))?;
        ensure!(
            self.selections.contains_key(key)
                || self.selections.len() < self.limits.max_selection_slots,
            "environment selection slot limit exceeded"
        );
        ensure!(
            self.transactions.len() < self.limits.max_transactions,
            "environment transaction history limit exceeded"
        );
        // Every fallible check precedes either mutation. A deletion retains its
        // revision as a tombstone, so an old absent snapshot cannot pass CAS.
        self.selections.insert(
            key.clone(),
            SelectionStamp {
                revision,
                generation: change.generation.clone(),
                language: change.language.clone(),
                transaction,
            },
        );
        self.transactions.insert(transaction, change);
        Ok(ApplyResult::Applied)
    }
    pub(crate) fn retired_generations(&self) -> Vec<&str> {
        let selected: BTreeSet<_> = self
            .selections
            .values()
            .filter_map(SelectionStamp::generation)
            .collect();
        self.generations
            .keys()
            .map(String::as_str)
            .filter(|allocation| !selected.contains(allocation))
            .collect()
    }
    pub(crate) fn protecting_references(&self) -> BTreeSet<InputReference> {
        // A retired record still protects its inputs until actual lease-aware
        // filesystem cleanup succeeds and the catalog forgets that record.
        // This temporary protection is not a permanent root for future prune.
        self.generations
            .values()
            .flat_map(|record| record.inputs.iter().cloned())
            .collect()
    }
    pub(crate) fn forget_retired(&mut self, allocation: &str) -> Result<()> {
        ensure!(
            !self
                .selections
                .values()
                .any(|stamp| stamp.generation() == Some(allocation)),
            "active environment generation cannot be forgotten"
        );
        if let Some(record) = self.generations.get(allocation) {
            let total = self
                .total_references
                .checked_sub(record.inputs.len())
                .ok_or_else(|| eyre::eyre!("environment input reference count is inconsistent"))?;
            if let Some(layout) = record.shared_layout() {
                ensure!(
                    self.shared_layouts.get(layout).map(String::as_str) == Some(allocation),
                    "shared layout index is inconsistent"
                );
                self.shared_layouts.remove(layout);
            }
            self.generations.remove(allocation);
            self.total_references = total;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "model/tests.rs"]
mod tests;
