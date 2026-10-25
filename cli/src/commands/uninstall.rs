use crate::args::ToolArg;
use crate::exit;
use eyre::{Result, bail, eyre};
/// Remove one exact unused language or package instance from the shared library.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Uninstall {
    /// Tool(s) to remove
    #[usage(value_name = "INSTALLED_TOOL@VERSION", required_unless = "package")]
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

impl Uninstall {
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
            let removed = crate::programs::library::uninstall_package(
                &requests[0],
                self.instance.as_deref(),
                self.is_dry_run(),
            )?;
            pakroprintln!(
                "{}{}",
                if self.is_dry_run() {
                    "would remove "
                } else {
                    "removed "
                },
                removed
            );
            if self.dry_run_code {
                return Err(exit::request(1));
            }
            return Ok(true);
        }
        if python.is_empty() {
            eyre::ensure!(
                self.instance.is_none(),
                "--instance requires an exact Python or --package target"
            );
            eyre::bail!("uninstall requires one exact shared Python or --package target");
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
        let removed = crate::programs::library::uninstall_python(
            &version,
            self.instance.as_deref(),
            self.is_dry_run(),
        )?;
        pakroprintln!(
            "{}{}",
            if self.is_dry_run() {
                "would remove "
            } else {
                "removed "
            },
            removed
        );
        if self.dry_run_code {
            return Err(exit::request(1));
        }
        Ok(true)
    }
}
