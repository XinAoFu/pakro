use eyre::Result;
mod get;
mod ls;
/// Inspect settings. Edit configuration files directly to change them.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Settings {
    #[usage(subcommand)]
    command: Option<Commands>,
    #[usage(flatten)]
    ls: ls::SettingsLs,
}
#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Get(get::SettingsGet),
    Ls(ls::SettingsLs),
}
impl Settings {
    pub(crate) async fn run(self) -> Result<()> {
        let local = self.ls.local;
        match self.command {
            Some(Commands::Get(mut command)) => {
                command.local |= local;
                command.run()
            }
            Some(Commands::Ls(mut command)) => {
                command.local |= local;
                command.run()
            }
            None => match self.ls.setting {
                Some(setting) => get::SettingsGet { setting, local }.run(),
                None => self.ls.run(),
            },
        }
    }
}
