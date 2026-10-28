//! Exact removal uses the same durable authority and continuation as prune.
use super::repository::PublishedLanguage;
use crate::{
    libraries::reclaim::{self, ReclaimId, ReclaimMode, ReclaimStatus},
    programs::OperationContext,
};
use eyre::{Result, ensure};
pub(crate) fn remove(context: &OperationContext, target: &PublishedLanguage) -> Result<()> {
    let id = ReclaimId::Language(target.key().fingerprint());
    let groups = crate::programs::uninstall::prune::plan_exact_target(context, id)?;
    let report = reclaim::execute(context, ReclaimMode::Exact, &groups, false)?;
    ensure!(
        report.complete,
        "language removal incomplete ({}): {}",
        if report
            .entries
            .iter()
            .any(|r| r.status == ReclaimStatus::Busy)
        {
            "in use"
        } else {
            "protected or pending"
        },
        report
            .entries
            .iter()
            .flat_map(|r| &r.reasons)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(())
}
#[cfg(test)]
mod tests;
