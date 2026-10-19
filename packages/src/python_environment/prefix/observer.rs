//! Prefix ownership attachment. Later phases instrument writes and cleanup.

use super::control::ownership::events::{
    OwnershipEntryKind, OwnershipEventEncoder, OwnershipEventLimits, OwnershipEventRef,
};
use super::control::ownership::{OwnershipStream, OwnershipStreamLimits};
use super::control::{ControlLimits, StoreControl};
use super::*;
use crate::packages::python_environment::journal::read_journal_metadata;
use crate::packages::python_environment::journal::{JournalLimits, JournalRecord};

#[path = "observer/cleanup.rs"]
pub(super) mod cleanup;

#[path = "observer/recovery.rs"]
pub(super) mod recovery;

#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnershipObserverLimits {
    pub stream: OwnershipStreamLimits,
    pub max_event_bytes: usize,
    pub max_events: usize,
    pub max_total_path_bytes: usize,
}
impl Default for OwnershipObserverLimits {
    fn default() -> Self {
        Self {
            stream: OwnershipStreamLimits::default(),
            max_event_bytes: 1024 * 1024,
            max_events: 1_048_576,
            max_total_path_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub(super) enum OwnershipRegistration {
    Unbound,
    Attached(Box<Observer>),
    Retained { nonce: uuid::Uuid },
}

pub(super) enum CreationCheckpoint {
    Intent,
    CreatedActual,
}

#[derive(Debug)]
pub(super) struct Observer {
    stream: OwnershipStream,
    journal: JournalRecord,
    journal_limits: JournalLimits,
    limits: OwnershipObserverLimits,
    control_limits: ControlLimits,
    encoder: OwnershipEventEncoder,
}

impl StoreControl<'_> {
    pub(crate) fn attach_prefix_ownership(
        &mut self,
        prefix: &mut UnpublishedPrefix,
        journal: &JournalRecord,
        journal_limits: JournalLimits,
        limits: OwnershipObserverLimits,
    ) -> Result<()> {
        ensure!(
            limits.max_event_bytes != 0
                && limits.max_events != 0
                && limits.max_total_path_bytes != 0,
            "ownership observer limits must be nonzero"
        );
        ensure!(
            matches!(prefix.ownership, OwnershipRegistration::Unbound),
            "prefix ownership was already registered"
        );
        prefix.check_binding()?;
        ensure!(
            prefix.entries.is_empty() && prefix.written_bytes == 0,
            "ownership observer must attach before any output"
        );
        let facts = prefix.generation_identity()?.facts()?;
        let (device, inode) = self.store_identity()?;
        ensure!(
            facts.store == [device, inode]
                && journal
                    .generation()
                    .is_some_and(|generation| generation.identity() == &facts),
            "ownership observer prefix and journal identities differ"
        );
        ensure!(
            read_journal_metadata(self, journal_limits)?
                .iter()
                .any(|current| current == journal),
            "ownership observer requires the exact durable journal"
        );
        let encoder = OwnershipEventEncoder::new(OwnershipEventLimits {
            prefix: prefix.limits,
            final_prefix_bytes: prefix.final_path.as_os_str().as_encoded_bytes().len(),
            max_event_bytes: limits.max_event_bytes,
            max_events: limits.max_events,
            max_total_path_bytes: limits.max_total_path_bytes,
        })?;
        prefix.ownership = OwnershipRegistration::Retained {
            nonce: journal.nonce(),
        };
        let stream =
            match self.create_observer_stream(journal, prefix, journal_limits, limits.stream) {
                Ok(stream) => stream,
                Err(error) => {
                    prefix.tainted = true;
                    return Err(error);
                }
            };
        prefix.ownership = OwnershipRegistration::Attached(Box::new(Observer {
            stream: stream.0,
            journal: journal.clone(),
            journal_limits,
            limits,
            control_limits: stream.1,
            encoder,
        }));
        Ok(())
    }
}

impl UnpublishedPrefix {
    pub(super) fn retain_uncertain_creation(&mut self, operation: Option<u64>) {
        if operation.is_none() {
            return;
        }
        self.tainted = true;
        if let OwnershipRegistration::Attached(observer) = &self.ownership {
            let nonce = observer.journal.nonce();
            self.ownership = OwnershipRegistration::Retained { nonce };
        }
    }

    fn emit_ownership<'a>(
        &mut self,
        event: impl FnOnce(u64) -> OwnershipEventRef<'a>,
    ) -> Result<Option<u64>> {
        let nonce = match &self.ownership {
            OwnershipRegistration::Unbound => return Ok(None),
            OwnershipRegistration::Retained { .. } => {
                eyre::bail!("ownership observer is retained after failure")
            }
            OwnershipRegistration::Attached(observer) => observer.journal.nonce(),
        };
        let registration = std::mem::replace(
            &mut self.ownership,
            OwnershipRegistration::Retained { nonce },
        );
        let OwnershipRegistration::Attached(mut observer) = registration else {
            unreachable!("checked attached observer")
        };
        let result = (|| {
            let sequence = observer.stream.next_sequence()?;
            let bytes = observer.encoder.encode(event(sequence))?;
            let receipt = observer.stream.append(self, &bytes)?;
            ensure!(
                receipt.sequence() == sequence,
                "ownership emitted frame sequence changed"
            );
            Ok(Some(sequence))
        })();
        if result.is_ok() {
            self.ownership = OwnershipRegistration::Attached(observer);
        } else {
            // Retained is already installed before the attempt. A failed
            // encoder/append must never restore write or unlogged cleanup
            // authority, even when the namespace is subsequently repaired.
            self.tainted = true;
        }
        result
    }
    pub(super) fn observe_create_intent(
        &mut self,
        kind: EntryKind,
        relative: &Path,
        parent: &File,
    ) -> Result<Option<u64>> {
        let parent = parent.metadata()?;
        self.emit_ownership(|op| OwnershipEventRef::CreateIntent {
            op,
            kind: match kind {
                EntryKind::Directory => OwnershipEntryKind::Directory,
                EntryKind::File => OwnershipEntryKind::File,
            },
            relative: relative.as_os_str().as_encoded_bytes(),
            parent: [parent.dev(), parent.ino()],
        })
    }
    pub(super) fn observe_created(&mut self, operation: Option<u64>, file: &File) -> Result<()> {
        if let Some(op) = operation {
            let identity = file.metadata()?;
            self.emit_ownership(|_| OwnershipEventRef::Created {
                op,
                identity: [identity.dev(), identity.ino()],
            })?;
        }
        Ok(())
    }
    pub(super) fn observe_finished(
        &mut self,
        operation: Option<u64>,
        size: u64,
        mode: u32,
    ) -> Result<()> {
        if let Some(op) = operation {
            self.emit_ownership(|_| OwnershipEventRef::FileFinished { op, size, mode })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "observer/write_tests.rs"]
mod write_tests;

#[cfg(test)]
#[path = "observer/cleanup_tests.rs"]
mod cleanup_tests;

#[cfg(test)]
#[path = "observer/metadata_tests.rs"]
mod metadata_tests;

#[cfg(test)]
#[path = "observer/late_tests.rs"]
mod late_tests;
