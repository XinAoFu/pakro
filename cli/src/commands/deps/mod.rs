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

pub(super) struct PreparedDeps {
    install: install::PreparedDepsInstall,
}

impl Deps {
    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        match &mut self.command {
            Commands::Install(command) => command.inherit_root_dry_run(dry_run),
        }
    }

    pub(super) async fn preflight(self) -> Result<PreparedDeps> {
        match self.command {
            Commands::Install(command) => Ok(PreparedDeps {
                install: command.preflight().await?,
            }),
        }
    }
}

impl PreparedDeps {
    pub(super) async fn run(self) -> Result<()> {
        self.install.run().await
    }
}
