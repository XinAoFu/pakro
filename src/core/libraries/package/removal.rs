//! Exact package removal and its derived layouts share the durable kernel.
#[cfg(test)]
use super::manifest;
use super::manifest::PackageManifest;
use crate::{
    libraries::reclaim::{self, ReclaimId, ReclaimMode, ReclaimStatus},
    programs::OperationContext,
};
use eyre::{Result, ensure};
pub(crate) fn remove(context: &OperationContext, target: &PackageManifest) -> Result<()> {
    let groups = crate::programs::uninstall::prune::plan_exact_target(
        context,
        ReclaimId::Package(target.key()),
    )?;
    let report = reclaim::execute(context, ReclaimMode::Exact, &groups, false)?;
    ensure!(
        report.complete,
        "package removal incomplete ({}): {}",
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
