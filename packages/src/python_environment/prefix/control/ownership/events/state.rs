//! Ordered metadata transitions. No paths are opened and no capability is minted.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

struct Machine {
    state: OwnershipRecoveryState,
    by_path: BTreeMap<Vec<u8>, usize>,
    identities: BTreeSet<[u64; 2]>,
    children: BTreeMap<Vec<u8>, usize>,
    remaining: usize,
    root: [u64; 2],
    limits: OwnershipEventLimits,
}
impl Machine {
    fn new(log: &OwnershipLog, limits: OwnershipEventLimits) -> Self {
        let root = log.identity().generation;
        Self {
            state: OwnershipRecoveryState {
                phase: OwnershipRecoveryPhase::Building,
                entries: Vec::new(),
                pending_create: None,
                capture: None,
                pending_remove: None,
                root_removed_sequence: None,
                last_frame_sha256: log.header.sha256,
                events: 0,
            },
            by_path: BTreeMap::new(),
            identities: BTreeSet::from([root]),
            children: BTreeMap::from([(Vec::new(), 0)]),
            remaining: 0,
            root,
            limits,
        }
    }
    fn parent(&self, canonical: &[u8]) -> Result<[u64; 2]> {
        let parent = parent_key(canonical);
        if parent.is_empty() {
            return Ok(self.root);
        }
        let index = self
            .by_path
            .get(parent)
            .ok_or_else(|| eyre::eyre!("ownership creation parent was not recorded"))?;
        let entry = &self.state.entries[*index];
        ensure!(
            entry.kind == OwnershipEntryKind::Directory && !entry.removed,
            "ownership creation parent is not an owned directory"
        );
        Ok(entry.identity)
    }
    fn apply(&mut self, event: OwnershipEvent, sequence: u64, frame: [u8; 32]) -> Result<()> {
        ensure!(
            self.state.phase != OwnershipRecoveryPhase::Complete,
            "ownership event follows completion"
        );
        match event {
            OwnershipEvent::CreateIntent {
                op,
                kind,
                relative,
                parent,
            } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Building
                        && self.state.pending_create.is_none(),
                    "ownership creation overlaps an unfinished operation or capture"
                );
                ensure!(
                    op == sequence,
                    "ownership creation operation disagrees with frame sequence"
                );
                let canonical = path::canonical(&relative, self.limits)?;
                ensure!(
                    !self.by_path.contains_key(&canonical),
                    "ownership path already has an owner"
                );
                ensure!(
                    parent == self.parent(&canonical)?,
                    "ownership creation parent identity disagrees"
                );
                self.state.pending_create = Some(OwnershipPendingCreate {
                    op,
                    relative,
                    kind,
                    parent,
                    created: None,
                });
            }
            OwnershipEvent::Created { op, identity } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Building,
                    "ownership creation completed after capture"
                );
                let mut pending = self
                    .state
                    .pending_create
                    .take()
                    .ok_or_else(|| eyre::eyre!("ownership created event has no intent"))?;
                ensure!(
                    pending.op == op && pending.created.is_none(),
                    "ownership created operation disagrees"
                );
                ensure!(
                    !self.identities.contains(&identity),
                    "ownership inode already has an owner"
                );
                let canonical = path::canonical(&pending.relative, self.limits)?;
                ensure!(
                    !self.by_path.contains_key(&canonical),
                    "ownership path already has an owner"
                );
                let parent_count = self
                    .children
                    .get_mut(parent_key(&canonical))
                    .ok_or_else(|| eyre::eyre!("ownership parent directory is missing"))?;
                *parent_count = parent_count
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("ownership child count limit overflow"))?;
                if pending.kind == OwnershipEntryKind::Directory {
                    self.children.insert(canonical.clone(), 0);
                }
                self.state
                    .entries
                    .try_reserve(1)
                    .map_err(|_| eyre::eyre!("ownership entry allocation limit failed"))?;
                self.by_path.insert(canonical, self.state.entries.len());
                self.identities.insert(identity);
                self.state.entries.push(OwnershipEntryFacts {
                    relative: pending.relative.clone(),
                    kind: pending.kind,
                    parent: pending.parent,
                    identity,
                    create_op: op,
                    created_frame_sha256: frame,
                    finished: None,
                    removed: false,
                });
                self.remaining = self
                    .remaining
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("ownership entry count limit overflow"))?;
                pending.created = Some(identity);
                if pending.kind == OwnershipEntryKind::File {
                    self.state.pending_create = Some(pending);
                }
            }
            OwnershipEvent::FileFinished { op, size, mode } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Building,
                    "ownership file finished after capture"
                );
                let pending = self
                    .state
                    .pending_create
                    .take()
                    .ok_or_else(|| eyre::eyre!("ownership finish has no open creation"))?;
                ensure!(
                    pending.op == op
                        && pending.kind == OwnershipEntryKind::File
                        && pending.created.is_some(),
                    "ownership finish is not the pending created file"
                );
                let canonical = path::canonical(&pending.relative, self.limits)?;
                let index = *self
                    .by_path
                    .get(&canonical)
                    .ok_or_else(|| eyre::eyre!("ownership finished file was not recorded"))?;
                ensure!(
                    self.state.entries[index].finished.is_none(),
                    "ownership file was already finished"
                );
                self.state.entries[index].finished = Some(OwnershipFinishedFacts {
                    size,
                    mode,
                    frame_sha256: frame,
                });
            }
            OwnershipEvent::CaptureIntent {
                op,
                quarantine,
                root,
            } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Building
                        && self.state.capture.is_none(),
                    "ownership capture was already started"
                );
                ensure!(
                    op == sequence && root == self.root,
                    "ownership capture operation or root disagrees"
                );
                self.state.capture = Some(OwnershipCaptureFacts {
                    op,
                    quarantine,
                    root,
                    captured: false,
                });
                self.state.phase = OwnershipRecoveryPhase::CapturePending;
            }
            OwnershipEvent::Captured { op } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::CapturePending,
                    "ownership captured event has no pending capture"
                );
                let capture = self
                    .state
                    .capture
                    .as_mut()
                    .ok_or_else(|| eyre::eyre!("ownership capture facts are missing"))?;
                ensure!(
                    capture.op == op && !capture.captured,
                    "ownership captured operation disagrees"
                );
                capture.captured = true;
                self.state.phase = OwnershipRecoveryPhase::Captured;
            }
            OwnershipEvent::RemoveIntent {
                op,
                kind,
                relative,
                identity,
            } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Captured
                        && self.state.pending_remove.is_none()
                        && !self.state.requires_retention(),
                    "ownership removal lacks a certain captured state"
                );
                ensure!(
                    op == sequence,
                    "ownership remove operation disagrees with frame sequence"
                );
                let canonical = path::canonical(&relative, self.limits)?;
                let index = *self
                    .by_path
                    .get(&canonical)
                    .ok_or_else(|| eyre::eyre!("ownership removal path was not created"))?;
                let entry = &self.state.entries[index];
                ensure!(
                    !entry.removed && entry.kind == kind && entry.identity == identity,
                    "ownership removal identity or kind disagrees"
                );
                if kind == OwnershipEntryKind::Directory {
                    ensure!(
                        self.children.get(&canonical) == Some(&0),
                        "ownership directory still has owned children"
                    );
                }
                self.state.pending_remove = Some(OwnershipPendingRemove {
                    op,
                    relative,
                    kind,
                    identity,
                });
            }
            OwnershipEvent::Removed { op } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Captured,
                    "ownership removed event precedes capture"
                );
                let pending = self
                    .state
                    .pending_remove
                    .take()
                    .ok_or_else(|| eyre::eyre!("ownership removed event has no intent"))?;
                ensure!(pending.op == op, "ownership removed operation disagrees");
                let canonical = path::canonical(&pending.relative, self.limits)?;
                let index = *self
                    .by_path
                    .get(&canonical)
                    .ok_or_else(|| eyre::eyre!("ownership removed path is missing"))?;
                let entry = &mut self.state.entries[index];
                ensure!(!entry.removed, "ownership entry was already removed");
                entry.removed = true;
                if self
                    .state
                    .pending_create
                    .as_ref()
                    .is_some_and(|pending| pending.op == entry.create_op)
                {
                    self.state.pending_create = None;
                }
                let count = self
                    .children
                    .get_mut(parent_key(&canonical))
                    .ok_or_else(|| eyre::eyre!("ownership removal parent is missing"))?;
                *count = count
                    .checked_sub(1)
                    .ok_or_else(|| eyre::eyre!("ownership remaining child count disagrees"))?;
                self.remaining = self
                    .remaining
                    .checked_sub(1)
                    .ok_or_else(|| eyre::eyre!("ownership remaining entry count disagrees"))?;
            }
            OwnershipEvent::RootRemoved { capture_op } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::Captured
                        && self
                            .state
                            .capture
                            .as_ref()
                            .is_some_and(|capture| capture.op == capture_op && capture.captured)
                        && self.remaining == 0
                        && self.state.pending_create.is_none()
                        && self.state.pending_remove.is_none(),
                    "ownership root removal precedes complete known cleanup"
                );
                self.state.phase = OwnershipRecoveryPhase::RootRemoved;
                self.state.root_removed_sequence = Some(sequence);
            }
            OwnershipEvent::Complete {
                root_removed_sequence,
            } => {
                ensure!(
                    self.state.phase == OwnershipRecoveryPhase::RootRemoved
                        && self.state.root_removed_sequence == Some(root_removed_sequence),
                    "ownership completion has no matching root removal"
                );
                self.state.phase = OwnershipRecoveryPhase::Complete;
            }
        }
        self.state.events = self
            .state
            .events
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("ownership event count limit overflow"))?;
        self.state.last_frame_sha256 = frame;
        Ok(())
    }
}
fn parent_key(canonical: &[u8]) -> &[u8] {
    canonical
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(b"", |index| &canonical[..index])
}

pub(super) fn validate(
    log: &OwnershipLog,
    limits: OwnershipEventLimits,
) -> Result<OwnershipRecoveryState> {
    path::limits(limits)?;
    let mut budget = codec::Budget::default();
    let mut machine = Machine::new(log, limits);
    for (index, record) in log.records().iter().enumerate() {
        ensure!(
            index < limits.max_events,
            "ownership event count limit exceeded"
        );
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| eyre::eyre!("ownership event sequence limit overflow"))?;
        ensure!(
            record.sequence() == sequence,
            "ownership raw frame sequence disagrees"
        );
        let event = codec::decode(record.payload(), limits, &mut budget)?;
        machine.apply(event, sequence, *record.chain_sha256())?;
    }
    Ok(machine.state)
}
