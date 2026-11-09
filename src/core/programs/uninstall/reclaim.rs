//! Acquires actual object protection before rechecking and removing content.
use crate::libraries::reclaim::{ReclaimId, ReclaimMode, ReclaimReport, ReclaimStatus};
use crate::programs::OperationContext;
use eyre::{Result, ensure};

fn exact(context: &OperationContext, id: ReclaimId, dry_run: bool) -> Result<ReclaimReport> {
    let groups = super::prune::plan_exact_target(context, id)?;
    crate::libraries::reclaim::execute(context, ReclaimMode::Exact, &groups, dry_run)
}

fn completed_label(report: ReclaimReport, id: ReclaimId) -> Result<String> {
    let expected = if report.dry_run {
        ReclaimStatus::Eligible
    } else {
        ReclaimStatus::Removed
    };
    let target_complete = report
        .entries
        .iter()
        .any(|entry| entry.object == Some(id) && entry.status == expected);
    let reasons = report
        .entries
        .iter()
        .flat_map(|entry| entry.reasons.iter().cloned())
        .collect::<Vec<_>>()
        .join("; ");
    let removed = report
        .entries
        .iter()
        .filter(|entry| entry.status == ReclaimStatus::Removed)
        .map(|entry| entry.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let resume = report
        .operation_id
        .map(|operation| format!("; continue with pakro prune --resume {operation}"))
        .unwrap_or_default();
    ensure!(
        report.complete && target_complete,
        "object removal did not complete; observed removed=[{removed}]: {reasons}{resume}"
    );
    Ok(id.label())
}

pub(crate) fn instance_report(
    context: &OperationContext,
    instance: &str,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let id = super::select::instance(context, instance)?;
    exact(context, id, dry_run)
}

pub(crate) fn language_report(
    context: &OperationContext,
    version: &str,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let target = super::select::language(context, version, instance)?;
    exact(
        context,
        ReclaimId::Language(target.key().fingerprint()),
        dry_run,
    )
}

pub(crate) fn package_report(
    context: &OperationContext,
    request: &crate::packages::PackageInstallRequest,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let target = super::select::package(context, request, instance)?;
    exact(context, ReclaimId::Package(target.key()), dry_run)
}

pub(crate) fn language(
    context: &OperationContext,
    version: &str,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    let target = super::select::language(context, version, instance)?;
    let id = ReclaimId::Language(target.key().fingerprint());
    completed_label(exact(context, id, dry_run)?, id)
}

pub(crate) fn package(
    context: &OperationContext,
    request: &crate::packages::PackageInstallRequest,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    let target = super::select::package(context, request, instance)?;
    let id = ReclaimId::Package(target.key());
    completed_label(exact(context, id, dry_run)?, id)
}
