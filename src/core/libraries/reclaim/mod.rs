//! Descriptive reclaim views and the single controlled deletion authority.
use crate::programs::OperationContext;
use eyre::Result;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod cli_tests;
mod engine;
mod object;
#[cfg(test)]
mod tests;
mod tree;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReclaimId {
    Language([u8; 32]),
    Package([u8; 32]),
    Layout([u8; 32]),
}
impl ReclaimId {
    pub fn key(self) -> [u8; 32] {
        match self {
            Self::Language(key) | Self::Package(key) | Self::Layout(key) => key,
        }
    }
    pub fn label(self) -> String {
        format!(
            "{}-{}",
            match self {
                Self::Language(_) => "language",
                Self::Package(_) => "package",
                Self::Layout(_) => "layout",
            },
            hex::encode(self.key())
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ReclaimMode {
    Exact,
    Prune,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReclaimStatus {
    Eligible,
    Protected,
    Busy,
    PendingRecovery,
    Damaged,
    Unknown,
    Removed,
    Retained,
    Capacity,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReclaimEntry {
    pub object: Option<ReclaimId>,
    pub label: String,
    pub status: ReclaimStatus,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ReclaimReport {
    pub dry_run: bool,
    pub complete: bool,
    pub entries: Vec<ReclaimEntry>,
}
#[derive(Debug, Clone)]
pub(crate) struct PendingReclaim {
    pub objects: Vec<ReclaimId>,
    pub members: Vec<(ReclaimId, Vec<ReclaimId>)>,
    pub directories: Vec<(ReclaimId, String)>,
    pub journals: Vec<(String, String)>,
}
pub(crate) const MAX_GROUP_OBJECTS: usize = 16;
pub(crate) fn execute(
    context: &OperationContext,
    mode: ReclaimMode,
    groups: &[Vec<ReclaimId>],
    dry_run: bool,
) -> Result<ReclaimReport> {
    engine::execute(context, mode, groups, dry_run)
}
pub(crate) fn pending(context: &OperationContext) -> Result<Vec<PendingReclaim>> {
    object::pending(context)
}
pub(crate) fn probe(context: &OperationContext, id: ReclaimId) -> Result<ReclaimEntry> {
    object::probe(context, id)
}
fn entry(id: ReclaimId, status: ReclaimStatus, reason: impl Into<String>) -> ReclaimEntry {
    ReclaimEntry {
        object: Some(id),
        label: id.label(),
        status,
        reasons: vec![reason.into()],
    }
}
