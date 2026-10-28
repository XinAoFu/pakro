//! Descriptive reclaim views and the single controlled deletion authority.
use crate::programs::OperationContext;
use eyre::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod cli_tests;
mod completion;
mod engine;
mod journal;
mod object;
mod preparing;
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
    pub operation_id: Option<Uuid>,
    pub dry_run: bool,
    pub resumed: bool,
    pub complete: bool,
    pub entries: Vec<ReclaimEntry>,
}
#[derive(Debug, Clone)]
pub(crate) struct PendingReclaim {
    pub operation_id: Uuid,
    pub objects: Vec<ReclaimId>,
    pub members: Vec<(ReclaimId, Vec<ReclaimId>)>,
    pub directories: Vec<(ReclaimId, String)>,
    pub journals: Vec<(String, String)>,
    pub complete: bool,
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
pub(crate) fn resume(
    context: &OperationContext,
    operation: Uuid,
    dry_run: bool,
) -> Result<ReclaimReport> {
    engine::resume(context, operation, dry_run)
}
pub(crate) fn pending(context: &OperationContext) -> Result<Vec<PendingReclaim>> {
    let mut values = journal::pending(context)?;
    for preparing in preparing::pending(context)? {
        if let Some(existing) = values
            .iter_mut()
            .find(|p| p.operation_id == preparing.operation_id)
        {
            existing.journals.extend(preparing.journals);
        } else {
            values.push(preparing);
        }
    }
    Ok(values)
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
mod uuid_serde {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        id: &Uuid,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&id.to_string())
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Uuid, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let id = Uuid::parse_str(&raw).map_err(serde::de::Error::custom)?;
        if id.is_nil() || id.to_string() != raw {
            return Err(serde::de::Error::custom("reclaim UUID is not canonical"));
        }
        Ok(id)
    }
}
