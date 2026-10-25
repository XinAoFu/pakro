use eyre::Result;
mod install;
/// Prepare the project's declared Python dependencies without editing declarations.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Deps {
    #[usage(subcommand)]
    command: Commands,
}
#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Install(install::DepsInstall),
}
impl Deps {
    pub(super) async fn preflight(&self) -> Result<()> {
        match &self.command {
            Commands::Install(command) => command.preflight().await,
        }
    }
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            Commands::Install(command) => command.run().await,
        }
    }
}
