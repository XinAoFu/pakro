use crate::args::ToolArg;
use crate::exit;
use eyre::{Result, bail, eyre};
/// Remove one exact unused language or package instance from the shared library.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Uninstall {
    /// Tool(s) to remove
    #[usage(value_name = "INSTALLED_TOOL@VERSION", required_unless = ["package", "instance"])]
    installed_tool: Vec<ToolArg>,

    /// Exact bound package request, e.g. python@3.13.16+idna@3.10
    #[usage(
        long,
        value_name = "RUNTIME@VERSION+PACKAGE@VERSION",
        conflicts = "installed_tool"
    )]
    package: Option<String>,

    /// Complete shared-library instance identity from pakro installs ls
    #[usage(long, value_name = "INSTANCE")]
    instance: Option<String>,

    /// Do not actually delete anything
    #[usage(long, short = 'n')]
    dry_run: bool,

    /// Like --dry-run but exits with code 1 if there are tools to uninstall
    ///
    /// This is useful for scripts to check if tools need to be uninstalled.
    #[usage(long, verbatim_doc_comment)]
    dry_run_code: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs::library::{ReclaimEntry, ReclaimId, ReclaimReport, ReclaimStatus};

    #[test]
    fn p8_instance_alone_is_accepted_by_the_cli_parser() {
        let instance = format!("package-{}", "ab".repeat(32));
        let args = ["pakro", "uninstall", "--instance", instance.as_str(), "--dry-run"]
            .into_iter()
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>();
        assert!(super::super::Cli::parse_from_argv(&args).is_ok());
    }

    #[test]
    fn p4_exact_uninstall_cannot_claim_success_from_an_auxiliary_layout_removal() {
        let command = Uninstall {
            installed_tool: vec![],
            package: None,
            instance: None,
            dry_run: false,
            dry_run_code: false,
        };
        let report = ReclaimReport {
            operation_id: None,
            dry_run: false,
            resumed: false,
            complete: true,
            entries: vec![ReclaimEntry {
                object: Some(ReclaimId::Layout([1; 32])),
                label: "layout".into(),
                status: ReclaimStatus::Removed,
                reasons: vec![],
            }],
        };
        assert!(command.finish_report(report, true).is_err());
    }
}

impl Uninstall {
    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        self.dry_run |= dry_run;
    }
    pub(super) fn is_dry_run(&self) -> bool {
        self.dry_run || self.dry_run_code
    }
    pub(crate) async fn run(self) -> Result<()> {
        self.shared_library_uninstall()?;
        Ok(())
    }
    fn shared_library_uninstall(&self) -> Result<bool> {
        let python: Vec<_> = self
            .installed_tool
            .iter()
            .filter(|tool| crate::programs::python::is_python_backend(&tool.ba))
            .collect();
        if self.package.is_some() {
            let raw = self.package.as_deref().expect("checked package");
            let parsed =
                crate::packages::request::parse_install_arg(raw).map_err(|error| eyre!(error))?;
            let crate::packages::request::InstallArg::Bound {
                tool,
                tool_version,
                packages,
            } = parsed
            else {
                bail!("--package requires a bound exact request such as python@3.13.16+idna@3.10");
            };
            let requests = crate::packages::request::requests_from_bound(
                &tool,
                tool_version.as_deref(),
                &packages,
            )
            .map_err(|error| eyre!(error))?;
            eyre::ensure!(
                requests.len() == 1,
                "remove one explicitly named package at a time"
            );
            let report = crate::programs::library::uninstall_package_report(
                &requests[0],
                self.instance.as_deref(),
                self.is_dry_run(),
            )?;
            self.finish_report(report, true)?;
            return Ok(true);
        }
        if python.is_empty() {
            if let Some(instance) = &self.instance {
                eyre::ensure!(
                    self.installed_tool.is_empty(),
                    "--instance alone cannot be combined with another tool"
                );
                let report = crate::programs::library::uninstall_instance_report(
                    instance,
                    self.is_dry_run(),
                )?;
                self.finish_report(report, instance.starts_with("package-"))?;
                return Ok(true);
            }
            eyre::bail!("uninstall requires one exact Python, --package, or --instance target");
        }
        eyre::ensure!(
            self.installed_tool.len() == 1,
            "shared Python removal requires one exact target"
        );
        let version = python[0]
            .tvr
            .as_ref()
            .map(|request| request.version())
            .unwrap_or_default();
        eyre::ensure!(
            !version.is_empty(),
            "Python removal requires a complete exact version, e.g. python@3.13.16"
        );
        let report = crate::programs::library::uninstall_python_report(
            &version,
            self.instance.as_deref(),
            self.is_dry_run(),
        )?;
        self.finish_report(report, false)?;
        Ok(true)
    }

    fn finish_report(
        &self,
        report: crate::programs::library::ReclaimReport,
        package: bool,
    ) -> Result<()> {
        use crate::programs::library::{ReclaimId, ReclaimStatus};
        super::prune::print_report(&report)?;
        let expected = if report.dry_run {
            ReclaimStatus::Eligible
        } else {
            ReclaimStatus::Removed
        };
        let target_complete = report.entries.iter().any(|entry| {
            entry.status == expected
                && match entry.object {
                    Some(ReclaimId::Package(_)) => package,
                    Some(ReclaimId::Language(_)) => !package,
                    _ => false,
                }
        });
        if !report.complete || !target_complete || self.dry_run_code {
            return Err(exit::request(1));
        }
        Ok(())
    }
}
