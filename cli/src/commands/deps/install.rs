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
impl DepsInstall {
    pub(super) async fn preflight(&self) -> Result<()> {
        ensure!(
            self.provider
                .as_deref()
                .is_none_or(|provider| provider == "python"),
            "dependency installation currently supports Python"
        );
        let project = crate::programs::python::prepare_current_project(&[])?
            .ok_or_else(|| eyre::eyre!("the project must declare an exact Python version"))?;
        ensure!(
            project.only_python_tools(),
            "dependency installation currently supports Python-only declarations"
        );
        Ok(())
    }
    pub(super) async fn run(self) -> Result<()> {
        self.preflight().await?;
        let project = crate::programs::python::prepare_current_project(&[])?
            .ok_or_else(|| eyre::eyre!("the project no longer declares Python"))?;
        if self.dry_run {
            pakroprintln!(
                "Would install the declared Python {} and its exact project packages",
                project.version()
            );
            return Ok(());
        }
        project.install(Config::get().await?).await
    }
}
