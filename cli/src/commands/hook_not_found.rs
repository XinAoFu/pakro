use crate::request_exit;
use crate::shell::ShellType;
use eyre::Result;
/// Report a missing project command without downloading or editing configuration.
#[derive(Debug, usage_rs::Args)]
#[usage(hide = true)]
pub(crate) struct HookNotFound {
    bin: String,
    #[usage(long, short, value_enum)]
    shell: Option<ShellType>,
}
impl HookNotFound {
    pub(crate) async fn run(self) -> Result<()> {
        use crate::programs::directory::{DirectoryCommandSelection, select_directory_command};
        match select_directory_command(&self.bin) {
            Ok(DirectoryCommandSelection::Available(_)) => Ok(()),
            Ok(_) => Err(request_exit(127)),
            Err(error) => {
                warn!("pakro: {error:#}");
                Err(request_exit(127))
            }
        }
    }
}
