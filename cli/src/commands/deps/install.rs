use crate::config::Config;
use eyre::{Result, ensure};
/// Install the exact Python and package versions declared by the current project.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct DepsInstall {
    /// Optional explicit ecosystem selector: python.
    provider: Option<String>,
    #[usage(long, short = 'n')]
    dry_run: bool,
}

pub(super) struct PreparedDepsInstall {
    project: crate::programs::python::PythonProjectInstall,
    config: std::sync::Arc<Config>,
    dry_run: bool,
}

impl DepsInstall {
    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        self.dry_run |= dry_run;
    }

    pub(super) async fn preflight(self) -> Result<PreparedDepsInstall> {
        ensure!(
            self.provider
                .as_deref()
                .is_none_or(|provider| provider == "python"),
            "dependency installation currently supports Python"
        );
        let config = super::super::read_config_for_admission().await?;
        let admission = crate::programs::python::capture_install_admission()?;
        let project = admission
            .project(&[], false)?
            .ok_or_else(|| eyre::eyre!("the project must declare an exact Python version"))?;
        ensure!(
            project.only_python_tools(),
            "dependency installation currently supports Python-only declarations"
        );
        Ok(PreparedDepsInstall {
            project,
            config,
            dry_run: self.dry_run,
        })
    }
}

impl PreparedDepsInstall {
    pub(super) async fn run(self) -> Result<()> {
        if self.dry_run {
            self.project.recheck_admission()?;
            pakroprintln!(
                "Would install the declared Python {} and its exact project packages",
                self.project.version()
            );
            return Ok(());
        }
        self.project.install(self.config).await
    }
}
