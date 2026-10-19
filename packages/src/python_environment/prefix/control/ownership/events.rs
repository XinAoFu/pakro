//! Typed ownership metadata. Pure values never grant filesystem authority.

use super::*;

#[path = "events/codec.rs"]
mod codec;
#[path = "events/path.rs"]
mod path;
#[path = "events/state.rs"]
mod state;

#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnershipEventLimits {
    pub prefix: PrefixLimits,
    pub final_prefix_bytes: usize,
    pub max_event_bytes: usize,
    pub max_events: usize,
    pub max_total_path_bytes: usize,
}
impl Default for OwnershipEventLimits {
    fn default() -> Self {
        Self {
            prefix: PrefixLimits::default(),
            final_prefix_bytes: 128,
            max_event_bytes: 1024 * 1024,
            max_events: 1_048_576,
            max_total_path_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipEntryKind {
    Directory,
    File,
}

/// Prefix-internal borrowed emission prevents copying an uncharged path.
#[derive(Clone, Copy)]
pub(in super::super::super) enum OwnershipEventRef<'a> {
    CreateIntent {
        op: u64,
        kind: OwnershipEntryKind,
        relative: &'a [u8],
        parent: [u64; 2],
    },
    Created {
        op: u64,
        identity: [u64; 2],
    },
    FileFinished {
        op: u64,
        size: u64,
        mode: u32,
    },
    CaptureIntent {
        op: u64,
        quarantine: &'a [u8],
        root: [u64; 2],
    },
    Captured {
        op: u64,
    },
    RemoveIntent {
        op: u64,
        kind: OwnershipEntryKind,
        relative: &'a [u8],
        identity: [u64; 2],
    },
    Removed {
        op: u64,
    },
    RootRemoved {
        capture_op: u64,
    },
    Complete {
        root_removed_sequence: u64,
    },
}
impl<'a> From<&'a OwnershipEvent> for OwnershipEventRef<'a> {
    fn from(event: &'a OwnershipEvent) -> Self {
        match event {
            OwnershipEvent::CreateIntent {
                op,
                kind,
                relative,
                parent,
            } => Self::CreateIntent {
                op: *op,
                kind: *kind,
                relative,
                parent: *parent,
            },
            OwnershipEvent::Created { op, identity } => Self::Created {
                op: *op,
                identity: *identity,
            },
            OwnershipEvent::FileFinished { op, size, mode } => Self::FileFinished {
                op: *op,
                size: *size,
                mode: *mode,
            },
            OwnershipEvent::CaptureIntent {
                op,
                quarantine,
                root,
            } => Self::CaptureIntent {
                op: *op,
                quarantine,
                root: *root,
            },
            OwnershipEvent::Captured { op } => Self::Captured { op: *op },
            OwnershipEvent::RemoveIntent {
                op,
                kind,
                relative,
                identity,
            } => Self::RemoveIntent {
                op: *op,
                kind: *kind,
                relative,
                identity: *identity,
            },
            OwnershipEvent::Removed { op } => Self::Removed { op: *op },
            OwnershipEvent::RootRemoved { capture_op } => Self::RootRemoved {
                capture_op: *capture_op,
            },
            OwnershipEvent::Complete {
                root_removed_sequence,
            } => Self::Complete {
                root_removed_sequence: *root_removed_sequence,
            },
        }
    }
}

#[derive(Debug)]
pub(in super::super::super) struct OwnershipEventEncoder {
    limits: OwnershipEventLimits,
    budget: codec::Budget,
    events: usize,
}
impl OwnershipEventEncoder {
    pub(in super::super::super) fn new(limits: OwnershipEventLimits) -> Result<Self> {
        path::limits(limits)?;
        Ok(Self {
            limits,
            budget: codec::Budget::default(),
            events: 0,
        })
    }
    pub(in super::super::super) fn encode(
        &mut self,
        event: OwnershipEventRef<'_>,
    ) -> Result<Vec<u8>> {
        ensure!(
            self.events < self.limits.max_events,
            "ownership event count limit exceeded"
        );
        let bytes = codec::encode_ref(event, self.limits, &mut self.budget)?;
        self.events += 1;
        Ok(bytes)
    }

    pub(in super::super::super) fn resume(
        log: &OwnershipLog,
        limits: OwnershipEventLimits,
    ) -> Result<Self> {
        let mut encoder = Self::new(limits)?;
        // Restore cumulative budgets once; append never replays the prefix.
        for record in log.records() {
            ensure!(
                encoder.events < limits.max_events,
                "ownership event count limit exceeded"
            );
            codec::decode(record.payload(), limits, &mut encoder.budget)?;
            encoder.events += 1;
        }
        Ok(encoder)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum OwnershipEvent {
    CreateIntent {
        op: u64,
        kind: OwnershipEntryKind,
        relative: Vec<u8>,
        parent: [u64; 2],
    },
    Created {
        op: u64,
        identity: [u64; 2],
    },
    FileFinished {
        op: u64,
        size: u64,
        mode: u32,
    },
    CaptureIntent {
        op: u64,
        quarantine: Vec<u8>,
        root: [u64; 2],
    },
    Captured {
        op: u64,
    },
    RemoveIntent {
        op: u64,
        kind: OwnershipEntryKind,
        relative: Vec<u8>,
        identity: [u64; 2],
    },
    Removed {
        op: u64,
    },
    RootRemoved {
        capture_op: u64,
    },
    Complete {
        root_removed_sequence: u64,
    },
}
impl std::fmt::Debug for OwnershipEvent {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, op) = match self {
            Self::CreateIntent { op, .. } => ("CreateIntent", *op),
            Self::Created { op, .. } => ("Created", *op),
            Self::FileFinished { op, .. } => ("FileFinished", *op),
            Self::CaptureIntent { op, .. } => ("CaptureIntent", *op),
            Self::Captured { op } => ("Captured", *op),
            Self::RemoveIntent { op, .. } => ("RemoveIntent", *op),
            Self::Removed { op } => ("Removed", *op),
            Self::RootRemoved { capture_op } => ("RootRemoved", *capture_op),
            Self::Complete {
                root_removed_sequence,
            } => ("Complete", *root_removed_sequence),
        };
        out.debug_struct(kind)
            .field("operation", &op)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipRecoveryPhase {
    Building,
    CapturePending,
    Captured,
    RootRemoved,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnershipFinishedFacts {
    size: u64,
    mode: u32,
    frame_sha256: [u8; 32],
}
impl OwnershipFinishedFacts {
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
    pub(crate) fn frame_sha256(&self) -> &[u8; 32] {
        &self.frame_sha256
    }
}
pub(crate) struct OwnershipEntryFacts {
    relative: Vec<u8>,
    kind: OwnershipEntryKind,
    parent: [u64; 2],
    identity: [u64; 2],
    create_op: u64,
    created_frame_sha256: [u8; 32],
    finished: Option<OwnershipFinishedFacts>,
    removed: bool,
}
impl OwnershipEntryFacts {
    pub(crate) fn relative(&self) -> &[u8] {
        &self.relative
    }
    pub(crate) fn kind(&self) -> OwnershipEntryKind {
        self.kind
    }
    pub(crate) fn parent(&self) -> [u64; 2] {
        self.parent
    }
    pub(crate) fn identity(&self) -> [u64; 2] {
        self.identity
    }
    pub(crate) fn create_op(&self) -> u64 {
        self.create_op
    }
    pub(crate) fn created_frame_sha256(&self) -> &[u8; 32] {
        &self.created_frame_sha256
    }
    pub(crate) fn finished(&self) -> Option<&OwnershipFinishedFacts> {
        self.finished.as_ref()
    }
    pub(crate) fn removed(&self) -> bool {
        self.removed
    }
}
pub(crate) struct OwnershipPendingCreate {
    op: u64,
    relative: Vec<u8>,
    kind: OwnershipEntryKind,
    parent: [u64; 2],
    created: Option<[u64; 2]>,
}
impl OwnershipPendingCreate {
    pub(crate) fn operation(&self) -> u64 {
        self.op
    }
    pub(crate) fn relative(&self) -> &[u8] {
        &self.relative
    }
    pub(crate) fn kind(&self) -> OwnershipEntryKind {
        self.kind
    }
    pub(crate) fn parent(&self) -> [u64; 2] {
        self.parent
    }
    pub(crate) fn created(&self) -> Option<[u64; 2]> {
        self.created
    }
}
pub(crate) struct OwnershipCaptureFacts {
    op: u64,
    quarantine: Vec<u8>,
    root: [u64; 2],
    captured: bool,
}
impl OwnershipCaptureFacts {
    pub(crate) fn operation(&self) -> u64 {
        self.op
    }
    pub(crate) fn quarantine(&self) -> &[u8] {
        &self.quarantine
    }
    pub(crate) fn root(&self) -> [u64; 2] {
        self.root
    }
    pub(crate) fn captured(&self) -> bool {
        self.captured
    }
}
pub(crate) struct OwnershipPendingRemove {
    op: u64,
    relative: Vec<u8>,
    kind: OwnershipEntryKind,
    identity: [u64; 2],
}
impl OwnershipPendingRemove {
    pub(crate) fn operation(&self) -> u64 {
        self.op
    }
    pub(crate) fn relative(&self) -> &[u8] {
        &self.relative
    }
    pub(crate) fn kind(&self) -> OwnershipEntryKind {
        self.kind
    }
    pub(crate) fn identity(&self) -> [u64; 2] {
        self.identity
    }
}
pub(crate) struct OwnershipRecoveryState {
    phase: OwnershipRecoveryPhase,
    entries: Vec<OwnershipEntryFacts>,
    pending_create: Option<OwnershipPendingCreate>,
    capture: Option<OwnershipCaptureFacts>,
    pending_remove: Option<OwnershipPendingRemove>,
    root_removed_sequence: Option<u64>,
    last_frame_sha256: [u8; 32],
    events: usize,
}
impl OwnershipRecoveryState {
    pub(crate) fn phase(&self) -> OwnershipRecoveryPhase {
        self.phase
    }
    pub(crate) fn entries(&self) -> &[OwnershipEntryFacts] {
        &self.entries
    }
    pub(crate) fn pending_create(&self) -> Option<&OwnershipPendingCreate> {
        self.pending_create.as_ref()
    }
    pub(crate) fn capture(&self) -> Option<&OwnershipCaptureFacts> {
        self.capture.as_ref()
    }
    pub(crate) fn pending_remove(&self) -> Option<&OwnershipPendingRemove> {
        self.pending_remove.as_ref()
    }
    pub(crate) fn root_removed_sequence(&self) -> Option<u64> {
        self.root_removed_sequence
    }
    pub(crate) fn last_frame_sha256(&self) -> &[u8; 32] {
        &self.last_frame_sha256
    }
    pub(crate) fn events(&self) -> usize {
        self.events
    }
    pub(crate) fn requires_retention(&self) -> bool {
        self.pending_create
            .as_ref()
            .is_some_and(|pending| pending.created.is_none())
    }
}
impl std::fmt::Debug for OwnershipRecoveryState {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("OwnershipRecoveryState")
            .field("phase", &self.phase)
            .field("entries", &self.entries.len())
            .field("requires_retention", &self.requires_retention())
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

pub(crate) fn encode_ownership_event(
    event: &OwnershipEvent,
    limits: OwnershipEventLimits,
) -> Result<Vec<u8>> {
    codec::encode(event, limits)
}
pub(crate) fn decode_ownership_event(
    bytes: &[u8],
    limits: OwnershipEventLimits,
) -> Result<OwnershipEvent> {
    codec::decode(bytes, limits, &mut codec::Budget::default())
}
pub(crate) fn validate_ownership_events(
    log: &OwnershipLog,
    limits: OwnershipEventLimits,
) -> Result<OwnershipRecoveryState> {
    state::validate(log, limits)
}

#[cfg(test)]
#[path = "events/tests.rs"]
mod tests;
