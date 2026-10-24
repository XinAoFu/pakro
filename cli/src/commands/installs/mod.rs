use eyre::{Result, bail};

use crate::config::Settings;

mod ls;
mod migrate;
mod select;

/// Inspect managed library instances and legacy installation choices
///
/// `ls` reports each published language or package instance and its users.
/// `select` and `migrate` retain their legacy identity-layout responsibilities;
/// shared Python selection follows the project's exact declaration.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Installs {
    #[usage(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Ls(ls::InstallsLs),
    Migrate(migrate::InstallsMigrate),
    Select(select::InstallsSelect),
}

impl Installs {
    pub(super) fn explicitly_non_python(&self) -> bool {
        matches!(&self.command, Some(Commands::Ls(command)) if command.explicitly_non_python())
    }

    pub(super) fn is_local_observation(&self) -> bool {
        matches!(&self.command, None | Some(Commands::Ls(_)))
    }

    pub(crate) async fn run(self) -> Result<()> {
        match self
            .command
            .unwrap_or(Commands::Ls(ls::InstallsLs::default()))
        {
            Commands::Ls(cmd) => cmd.run().await,
            Commands::Migrate(cmd) => {
                Settings::get().ensure_experimental("pakro installs migrate")?;
                if !crate::install_layout::resolver::enabled() {
                    bail!("the identity install layout is off");
                }
                cmd.run().await
            }
            Commands::Select(cmd) => cmd.run(),
        }
    }
}
