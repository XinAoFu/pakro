//! Prepared transaction metadata and data-only recovery decisions.
//!
//! A recorded inode identity never reconstructs a prefix or lease capability.

use eyre::{Result, ensure};
use uuid::Uuid;

use super::model::{
    CatalogCodecLimits, CatalogModel, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use super::prefix::control::StoreControl;
use super::prefix::control::lease::{GenerationIdentity, GenerationIdentityFacts};

#[path = "journal/encode.rs"]
mod encode;
#[path = "journal/read.rs"]
mod read;
#[path = "journal/repository.rs"]
mod repository;

#[derive(Debug, Clone, Copy)]
pub(crate) struct JournalLimits {
    pub model: ModelLimits,
    pub max_document_bytes: usize,
    pub max_total_document_bytes: usize,
    pub max_documents: usize,
    pub max_total_name_bytes: usize,
    pub max_inputs_per_record: usize,
    pub max_total_inputs: usize,
}
impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            model: ModelLimits::default(),
            max_document_bytes: 16 * 1024 * 1024,
            max_total_document_bytes: 256 * 1024 * 1024,
            max_documents: 65_536,
            max_total_name_bytes: 4 * 1024 * 1024,
            max_inputs_per_record: 65_536,
            max_total_inputs: 1_048_576,
        }
    }
}
impl JournalLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            self.max_document_bytes != 0
                && self.max_total_document_bytes != 0
                && self.max_documents != 0
                && self.max_total_name_bytes != 0
                && self.max_inputs_per_record != 0
                && self.max_total_inputs != 0
                && self.model.max_generation_records != 0
                && self.model.max_selection_slots != 0
                && self.model.max_refs_per_generation != 0
                && self.model.max_total_references != 0
                && self.model.max_group_bytes != 0
                && self.model.max_transactions != 0,
            "journal limits must be nonzero"
        );
        Ok(())
    }
}

pub(crate) struct JournalPrepare<'a> {
    pub nonce: Uuid,
    pub key: &'a SelectionKey,
    pub expected: &'a SelectionStamp,
    pub target: JournalTarget<'a>,
}
pub(crate) enum JournalTarget<'a> {
    Generation {
        identity: &'a GenerationIdentity,
        graph: [u8; 32],
        inputs: &'a [InputReference],
    },
    ClearSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalExpectedStamp {
    revision: u64,
    generation: Option<String>,
    transaction: Uuid,
}
impl JournalExpectedStamp {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }
    pub(crate) fn transaction(&self) -> Uuid {
        self.transaction
    }
    fn matches(&self, stamp: &SelectionStamp) -> bool {
        self.revision == stamp.revision()
            && self.generation.as_deref() == stamp.generation()
            && self.transaction == stamp.transaction()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalGenerationFacts {
    identity: GenerationIdentityFacts,
    graph: [u8; 32],
    inputs: Vec<InputReference>,
}
impl JournalGenerationFacts {
    pub(crate) fn identity(&self) -> &GenerationIdentityFacts {
        &self.identity
    }
    pub(crate) fn graph(&self) -> &[u8; 32] {
        &self.graph
    }
    pub(crate) fn inputs(&self) -> &[InputReference] {
        &self.inputs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedTarget {
    Generation(JournalGenerationFacts),
    ClearSelection { store: [u64; 2] },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct JournalRecord {
    nonce: Uuid,
    key: SelectionKey,
    expected: JournalExpectedStamp,
    target: RecordedTarget,
}
impl JournalRecord {
    pub(crate) fn nonce(&self) -> Uuid {
        self.nonce
    }
    pub(crate) fn key(&self) -> &SelectionKey {
        &self.key
    }
    pub(crate) fn expected(&self) -> &JournalExpectedStamp {
        &self.expected
    }
    pub(crate) fn generation(&self) -> Option<&JournalGenerationFacts> {
        match &self.target {
            RecordedTarget::Generation(facts) => Some(facts),
            RecordedTarget::ClearSelection { .. } => None,
        }
    }
    pub(crate) fn store_identity(&self) -> [u64; 2] {
        match &self.target {
            RecordedTarget::Generation(facts) => facts.identity.store,
            RecordedTarget::ClearSelection { store } => *store,
        }
    }
}
impl std::fmt::Debug for JournalRecord {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("JournalRecord")
            .field("nonce", &self.nonce)
            .field("clear_selection", &self.generation().is_none())
            .finish_non_exhaustive()
    }
}

/// These variants describe metadata facts, never cleanup/publication authority.
#[derive(Debug)]
pub(crate) enum JournalRecoveryDecision {
    CatalogCommitted(JournalRecord),
    CleanupCandidate(JournalRecord),
    UncommittedClear(JournalRecord),
}
impl JournalRecoveryDecision {
    pub(crate) fn record(&self) -> &JournalRecord {
        match self {
            Self::CatalogCommitted(record)
            | Self::CleanupCandidate(record)
            | Self::UncommittedClear(record) => record,
        }
    }
}

pub(crate) fn prepare_journal_metadata(
    control: &mut StoreControl<'_>,
    request: &JournalPrepare<'_>,
    limits: JournalLimits,
) -> Result<JournalRecord> {
    repository::prepare(control, request, limits)
}

pub(crate) fn read_journal_metadata(
    control: &StoreControl<'_>,
    limits: JournalLimits,
) -> Result<Vec<JournalRecord>> {
    repository::load(control, limits).map(|batch| batch.records)
}

pub(crate) fn plan_journal_recovery(
    control: &StoreControl<'_>,
    limits: JournalLimits,
    catalog_limits: CatalogCodecLimits,
) -> Result<Vec<JournalRecoveryDecision>> {
    let catalog = super::catalog::read_catalog_metadata(control, catalog_limits)?;
    let records = read_journal_metadata(control, limits)?;
    records
        .into_iter()
        .map(|record| decide_recovery(record, &catalog))
        .collect()
}

fn decide_recovery(
    record: JournalRecord,
    catalog: &CatalogModel,
) -> Result<JournalRecoveryDecision> {
    if let Some((key, expected, generation)) = catalog.applied_transaction(record.nonce) {
        ensure!(
            key == &record.key
                && record.expected.matches(expected)
                && generation
                    == record
                        .generation()
                        .map(|facts| facts.identity.allocation.as_str()),
            "journal nonce conflicts with catalog transaction facts"
        );
        if let Some(facts) = record.generation()
            && let Some(current) = catalog.generation(&facts.identity.allocation)
        {
            ensure!(
                current.target() == record.key.target()
                    && current.graph() == &facts.graph
                    && current.inputs() == facts.inputs,
                "journal generation conflicts with catalog record facts"
            );
        }
        return Ok(JournalRecoveryDecision::CatalogCommitted(record));
    }
    Ok(if record.generation().is_some() {
        JournalRecoveryDecision::CleanupCandidate(record)
    } else {
        JournalRecoveryDecision::UncommittedClear(record)
    })
}

fn filename(nonce: Uuid) -> String {
    format!("{}.json", nonce.simple())
}

fn validate_allocation(allocation: &str) -> Result<()> {
    let bytes = allocation.as_bytes();
    ensure!(
        bytes.len() == 34
            && bytes.starts_with(b"g-")
            && bytes[2..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "invalid journal allocation identity"
    );
    Ok(())
}

fn validate_expected(stamp: &JournalExpectedStamp) -> Result<()> {
    if let Some(generation) = &stamp.generation {
        validate_allocation(generation)?;
    }
    ensure!(
        if stamp.revision == 0 {
            stamp.generation.is_none() && stamp.transaction.is_nil()
        } else {
            !stamp.transaction.is_nil()
        },
        "invalid journal expected selection stamp"
    );
    Ok(())
}

#[cfg(test)]
#[path = "journal/tests.rs"]
mod tests;
