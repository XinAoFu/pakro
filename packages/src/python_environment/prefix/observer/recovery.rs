//! Fresh-session recovery facts; no ready prefix or publication capability.

use super::*;
use crate::packages::python_environment::catalog::read_catalog_metadata;
use crate::packages::python_environment::journal::{
    JournalRecoveryDecision, plan_journal_recovery,
};
use crate::packages::python_environment::model::CatalogCodecLimits;
use crate::packages::python_environment::prefix::control::lease::GenerationIdentityFacts;
use crate::packages::python_environment::prefix::control::ownership::events::{
    OwnershipRecoveryPhase, validate_ownership_events,
};
use crate::packages::python_environment::prefix::control::scan::NamespaceScanLimits;

#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnershipRecoveryLimits {
    pub journal: JournalLimits,
    pub catalog: CatalogCodecLimits,
    pub stream: OwnershipStreamLimits,
    pub prefix: PrefixLimits,
    pub max_event_bytes: usize,
    pub max_events: usize,
    pub max_total_path_bytes: usize,
    pub namespace: NamespaceScanLimits,
}
impl Default for OwnershipRecoveryLimits {
    fn default() -> Self {
        Self {
            journal: JournalLimits::default(),
            catalog: CatalogCodecLimits::default(),
            stream: OwnershipStreamLimits::default(),
            prefix: PrefixLimits::default(),
            max_event_bytes: 1024 * 1024,
            max_events: 1_048_576,
            max_total_path_bytes: 64 * 1024 * 1024,
            namespace: NamespaceScanLimits::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct OwnershipCleanupReceipt {
    nonce: uuid::Uuid,
    identity: GenerationIdentityFacts,
    stream_head: [u8; 32],
    stream_bytes: u64,
}
impl OwnershipCleanupReceipt {
    pub(crate) fn nonce(&self) -> uuid::Uuid {
        self.nonce
    }
    pub(crate) fn identity(&self) -> &GenerationIdentityFacts {
        &self.identity
    }
    pub(crate) fn stream_head(&self) -> &[u8; 32] {
        &self.stream_head
    }
    pub(crate) fn stream_bytes(&self) -> u64 {
        self.stream_bytes
    }
}

#[derive(Debug)]
pub(crate) enum OwnershipRecoveryOutcome {
    AlreadyCommitted,
    UncommittedClear,
    Busy,
    RetainedUncertain,
    Cleaned(OwnershipCleanupReceipt),
}

impl StoreControl<'_> {
    pub(crate) fn recover_uncommitted_prefix(
        &mut self,
        journal: &JournalRecord,
        limits: OwnershipRecoveryLimits,
    ) -> Result<OwnershipRecoveryOutcome> {
        self.recover_with_checkpoint(journal, limits, || Ok(()))
    }

    fn recover_with_checkpoint(
        &mut self,
        journal: &JournalRecord,
        limits: OwnershipRecoveryLimits,
        checkpoint: impl FnOnce() -> Result<()>,
    ) -> Result<OwnershipRecoveryOutcome> {
        self.recover_with_checkpoints(journal, limits, checkpoint, |_| Ok(()))
    }

    #[cfg(test)]
    fn recover_with_progress(
        &mut self,
        journal: &JournalRecord,
        limits: OwnershipRecoveryLimits,
        progress: impl FnMut(cleanup::CleanupCheckpoint<'_>) -> Result<()>,
    ) -> Result<OwnershipRecoveryOutcome> {
        self.recover_with_checkpoints(journal, limits, || Ok(()), progress)
    }

    fn recover_with_checkpoints(
        &mut self,
        journal: &JournalRecord,
        limits: OwnershipRecoveryLimits,
        checkpoint: impl FnOnce() -> Result<()>,
        mut progress: impl FnMut(cleanup::CleanupCheckpoint<'_>) -> Result<()>,
    ) -> Result<OwnershipRecoveryOutcome> {
        ensure!(
            limits.prefix.max_components != 0
                && limits.prefix.max_entries != 0
                && limits.prefix.max_path_bytes != 0
                && limits.prefix.max_total_bytes != 0
                && limits.prefix.max_cleanup_entries != 0
                && limits.max_event_bytes != 0
                && limits.max_events != 0
                && limits.max_total_path_bytes != 0,
            "ownership recovery limits must be nonzero"
        );
        // Acquire the existing object lock before the first catalog read. The
        // durable journal and reference checks below still decide what (if
        // anything) may be cleaned; lock acquisition alone grants no cleanup.
        let lease = journal
            .generation()
            .map(|generation| self.try_cleanup_lease(generation.identity()))
            .transpose()?
            .flatten();
        let decisions = plan_journal_recovery(self, limits.journal, limits.catalog)?;
        let decision = decisions
            .iter()
            .find(|decision| decision.record().nonce() == journal.nonce())
            .ok_or_else(|| eyre::eyre!("recovery requires the exact fresh durable journal"))?;
        ensure!(
            decision.record() == journal,
            "recovery journal immutable facts changed"
        );
        match decision {
            JournalRecoveryDecision::CatalogCommitted(_) => {
                return Ok(OwnershipRecoveryOutcome::AlreadyCommitted);
            }
            JournalRecoveryDecision::UncommittedClear(_) => {
                return Ok(OwnershipRecoveryOutcome::UncommittedClear);
            }
            JournalRecoveryDecision::CleanupCandidate(_) => {}
        }
        let generation = journal
            .generation()
            .ok_or_else(|| eyre::eyre!("recovery generation is missing"))?;
        let catalog = read_catalog_metadata(self, limits.catalog)?;
        ensure!(
            catalog
                .generation(&generation.identity().allocation)
                .is_none(),
            "catalog record still protects this environment generation"
        );
        ensure!(
            !decisions
                .iter()
                .any(|other| other.record().nonce() != journal.nonce()
                    && other
                        .record()
                        .generation()
                        .is_some_and(
                            |other| other.identity().allocation == generation.identity().allocation
                        )),
            "another pending transaction still protects this generation"
        );
        self.scan_environment_namespace(limits.namespace)?;
        let (_, store_path, _) = self.recovery_store_anchor()?;
        let event_limits = OwnershipEventLimits {
            prefix: limits.prefix,
            final_prefix_bytes: store_path
                .join(&generation.identity().allocation)
                .as_os_str()
                .as_encoded_bytes()
                .len(),
            max_event_bytes: limits.max_event_bytes,
            max_events: limits.max_events,
            max_total_path_bytes: limits.max_total_path_bytes,
        };
        let log = self
            .read_ownership_stream(journal, limits.journal, limits.stream)?
            .ok_or_else(|| eyre::eyre!("recovery ownership stream is missing"))?;
        let state = validate_ownership_events(&log, event_limits)?;
        let Some(lease) = lease else {
            return Ok(OwnershipRecoveryOutcome::Busy);
        };
        if state.requires_retention() {
            return Ok(OwnershipRecoveryOutcome::RetainedUncertain);
        }
        let mut witness = cleanup::CleanupWitness::restart(self, lease, journal, &state)?;
        let mut tree = cleanup::tree::Tree::load(
            witness.root_file(),
            &state,
            limits.prefix,
            event_limits.final_prefix_bytes,
            witness.expected_uid(),
        )?;
        checkpoint()?;
        let (stream, control_limits) =
            self.resume_cleanup_stream(journal, limits.journal, &log, limits.stream, &witness)?;
        let mut observer = Observer {
            stream,
            journal: journal.clone(),
            journal_limits: limits.journal,
            control_limits,
            limits: OwnershipObserverLimits {
                stream: limits.stream,
                max_event_bytes: limits.max_event_bytes,
                max_events: limits.max_events,
                max_total_path_bytes: limits.max_total_path_bytes,
            },
            encoder: OwnershipEventEncoder::resume(&log, event_limits)?,
        };
        cleanup::run(
            &mut observer,
            &mut witness,
            &state,
            &mut tree,
            cleanup::RunTarget {
                store_path: &store_path,
                max_cleanup_entries: limits.prefix.max_cleanup_entries,
            },
            &mut progress,
            &mut None,
        )?;
        let final_log = self
            .read_ownership_stream(journal, limits.journal, limits.stream)?
            .ok_or_else(|| eyre::eyre!("completed ownership stream disappeared"))?;
        ensure!(
            validate_ownership_events(&final_log, event_limits)?.phase()
                == OwnershipRecoveryPhase::Complete,
            "cleanup did not persist complete ownership progress"
        );
        Ok(OwnershipRecoveryOutcome::Cleaned(OwnershipCleanupReceipt {
            nonce: journal.nonce(),
            identity: generation.identity().clone(),
            stream_head: *final_log.head(),
            stream_bytes: final_log.total_bytes(),
        }))
    }
}

#[cfg(test)]
#[path = "recovery/tests.rs"]
mod tests;
