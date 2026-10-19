//! Pure environment catalog state. No filesystem or publication authority.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Result, ensure};
use pakro_store::Digest;
use uuid::Uuid;

#[path = "model/codec.rs"]
mod codec;
pub(crate) use codec::CatalogCodecLimits;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationRecord {
    allocation: String,
    target: [u8; 32],
    shared_layout: Option<[u8; 32]>,
    graph: [u8; 32],
    receipt: [u8; 32],
    inputs: Vec<InputReference>,
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
        if let Some(existing) = self.generations.get(record.allocation()) {
            ensure!(
                existing == &record,
                "environment allocation conflicts with registered facts"
            );
        }
        if let Some(existing) = self.generation_for_shared_layout(layout) {
            ensure!(
                existing.target == record.target
                    && existing.graph == record.graph
                    && existing.receipt == record.receipt
                    && existing.inputs == record.inputs,
                "shared layout identity conflicts with registered content facts"
            );
            return Ok(existing.allocation.clone());
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
