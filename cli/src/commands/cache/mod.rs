use eyre::Result;

use crate::env;

mod clear;
mod path;
mod prune;
mod task;

/// Manage the pakro cache
///
/// Run `pakro cache` with no args to view the current cache directory.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Cache {
    #[usage(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Clear(clear::CacheClear),
    Path(path::CachePath),
    Prune(prune::CachePrune),
    Task(task::CacheTask),
}

impl Commands {
    pub(crate) async fn run(self) -> Result<()> {
        match self {
            Self::Clear(cmd) => cmd.run().await,
            Self::Path(cmd) => cmd.run(),
            Self::Prune(cmd) => cmd.run(),
            Self::Task(cmd) => cmd.run().await,
        }
    }
}

impl Cache {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            Some(cmd) => cmd.run().await,
            None => {
                // just show the cache dir
                pakroprintln!("{}", env::PAKRO_CACHE_DIR.display());
                Ok(())
            }
        }
    }
}
