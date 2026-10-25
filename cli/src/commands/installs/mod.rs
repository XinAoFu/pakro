use eyre::Result;
mod ls;
/// Inspect published shared language and package instances and their users.
#[derive(Debug, usage_rs::Args)]
pub(crate) struct Installs {
    #[usage(subcommand)]
    command: Option<Commands>,
}
#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Ls(ls::InstallsLs),
}
impl Installs {
    pub(crate) async fn run(self) -> Result<()> {
        match self
            .command
            .unwrap_or(Commands::Ls(ls::InstallsLs::default()))
        {
            Commands::Ls(command) => command.run().await,
        }
    }
}
