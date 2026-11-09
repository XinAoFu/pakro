use crate::exit;
use crate::programs::library::{ReclaimReport, ReclaimStatus};
use eyre::{Context, Result};

/// Reclaim unreferenced managed contents. After interruption, run pakro prune again.
#[derive(Debug, Default, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example("pakro prune --dry-run"),
    example("pakro prune")
)]
pub(crate) struct Prune {
    /// Preview without changing contents, references or deletion progress.
    #[usage(long, short = 'n')]
    dry_run: bool,
}

impl Prune {
    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        self.dry_run |= dry_run;
    }
    pub(super) fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub(crate) async fn run(self) -> Result<()> {
        let report = crate::programs::library::prune(self.dry_run)?;
        finish_report(&report)
    }
}

fn finish_report(report: &ReclaimReport) -> Result<()> {
    print_report(report)?;
    if report.complete {
        Ok(())
    } else {
        Err(exit::request(1))
    }
}

pub(super) fn print_report(report: &ReclaimReport) -> Result<()> {
    write_report(report).wrap_err_with(|| {
        let removed = report.entries.iter().filter(|entry| entry.status == ReclaimStatus::Removed)
            .map(|entry| entry.label.as_str()).collect::<Vec<_>>().join(", ");
        format!("reclamation result: dry_run={}, complete={}, observed removed=[{removed}]; writing result output failed", report.dry_run, report.complete)
    })
}

fn write_report(report: &ReclaimReport) -> Result<()> {
    if report.entries.is_empty() {
        pakroprintln!("No managed content needs reclamation");
    }
    for entry in &report.entries {
        let status = match entry.status {
            ReclaimStatus::Eligible => {
                if report.dry_run {
                    "would remove"
                } else {
                    "not removed"
                }
            }
            ReclaimStatus::Protected => "protected",
            ReclaimStatus::Busy => "busy",
            ReclaimStatus::PendingRecovery => "pending recovery",
            ReclaimStatus::Damaged => "damaged",
            ReclaimStatus::Unknown => "unknown",
            ReclaimStatus::Removed => "removed",
            ReclaimStatus::Retained => "retained",
            ReclaimStatus::Capacity => "capacity limit",
        };
        pakroprintln!("{status}: {}", entry.label);
        for reason in &entry.reasons {
            pakroprintln!("  {reason}");
        }
    }
    if !report.complete {
        pakroprintln!("Reclamation did not complete; the rows above show the actual result");
        pakroprintln!("After interruption, run pakro prune again to clear unavailable contents");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p4_partial_reclamation_reports_progress_and_returns_nonzero() {
        use crate::programs::library::{ReclaimEntry, ReclaimId};
        let report = ReclaimReport {
            dry_run: false,
            complete: false,
            entries: vec![
                ReclaimEntry {
                    object: Some(ReclaimId::Package([1; 32])),
                    label: "removed package".into(),
                    status: ReclaimStatus::Removed,
                    reasons: vec![],
                },
                ReclaimEntry {
                    object: Some(ReclaimId::Package([2; 32])),
                    label: "busy package".into(),
                    status: ReclaimStatus::Busy,
                    reasons: vec!["active use protection".into()],
                },
            ],
        };
        let error = finish_report(&report).unwrap_err();
        assert_eq!(crate::exit::requested_exit_code(&error), Some(1));
        assert_eq!(report.entries[0].status, ReclaimStatus::Removed);
        assert_eq!(report.entries[1].status, ReclaimStatus::Busy);
        let protected = ReclaimReport {
            dry_run: false,
            complete: true,
            entries: vec![ReclaimEntry {
                object: Some(ReclaimId::Package([3; 32])),
                label: "used package".into(),
                status: ReclaimStatus::Protected,
                reasons: vec!["project still needs this object".into()],
            }],
        };
        assert!(
            finish_report(&protected).is_ok(),
            "protected content is an expected prune retention"
        );
    }
}
