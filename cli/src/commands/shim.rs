//! A shim selects an existing project command and directly hands off to it.
use crate::programs::directory::{DirectoryCommandSelection, select_directory_command};
use crate::{env, file, request_exit};
use eyre::{Result, bail};

pub(crate) async fn handle_shim() -> Result<()> {
    let name = *env::PAKRO_BIN_NAME;
    if env::is_pakro_binary(name) || cfg!(test) {
        return Ok(());
    }
    let selected = select_directory_command(name)?;
    let shared = matches!(&selected, DirectoryCommandSelection::Available(_));
    let program = match selected {
        DirectoryCommandSelection::Available(path) => path,
        DirectoryCommandSelection::Missing => return Err(request_exit(127)),
        DirectoryCommandSelection::NotPython => {
            let own = file::canonicalize_or_self(&env::PAKRO_BIN);
            env::PATH
                .iter()
                .filter(|path| {
                    !file::is_pakro_shims_dir(path) && !file::is_command_wrapper_dir(path)
                })
                .map(|path| path.join(name))
                .find(|path| {
                    path.is_file()
                        && file::is_executable(path)
                        && file::canonicalize_or_self(path) != own
                })
                .ok_or_else(|| {
                    eyre::eyre!("no installed command {name:?}; install missing content explicitly")
                })?
        }
    };
    let args = env::ARGS.read().unwrap().clone();
    let mut command = std::process::Command::new(program);
    command.args(args.iter().skip(1));
    if shared {
        command.env("PYTHONDONTWRITEBYTECODE", "1");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        bail!("failed to execute {name:?}: {error}");
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Err(request_exit(status.code().unwrap_or(1)))
    }
}
