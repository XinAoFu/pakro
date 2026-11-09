use crate::config::settings::SettingsPartial;
use crate::config::{Config, Settings, SettingsExt, config_file};
use crate::ui::{self, ctrlc};
use crate::{Result, backend, request_exit};
use crate::{hook_env as hook_env_module, logger};
use confique::Layer as _;
use eyre::{Report, bail};
use futures_util::future::LocalBoxFuture;
use std::path::PathBuf;
use usage_rs::config::{Layers, PropMeta, Registry as SettingsRegistry, Ty, Value};
const CLI_SETTINGS_REGISTRY: SettingsRegistry = SettingsRegistry::new(&[PropMeta {
    cli: &["--truncate", "--no-truncate"],
    ..PropMeta::new("truncate", Ty::Bool)
}]);
fn command_local_settings(layer: &usage_rs::config::CliLayer) -> Result<SettingsPartial> {
    let resolved = usage_rs::config::resolve(CLI_SETTINGS_REGISTRY, Layers::new().then(layer))?;
    let mut settings = SettingsPartial::empty();
    if let Some(Value::Bool(value)) = resolved.get_key("truncate") {
        settings.truncate = Some(*value);
    }
    Ok(settings)
}

pub(crate) use crate::args;
mod activate;
#[cfg(all(test, unix))]
mod admission_tests;
mod bin_paths;
mod cache;
mod command_effects;
mod completion;
mod config;
mod current;
mod deactivate;
mod deps;
mod doctor;
mod env;
mod hook_env;
mod hook_not_found;
mod implode;
mod install;
mod installs;
mod latest;
mod ls;
mod ls_remote;
mod project;
mod prune;
#[cfg(all(test, unix))]
mod reclaim_tests;
mod registry;
mod reshim;
mod search;
#[cfg_attr(not(feature = "self_update"), path = "self_update_stub.rs")]
pub(crate) mod self_update;
mod settings;
mod shim;
mod trust;
mod uninstall;
mod untrust;
mod usage;
pub(crate) mod version;
mod r#where;
mod r#which;

/// Read configuration without registration or maintenance side effects.
pub(super) async fn read_config_for_admission() -> Result<std::sync::Arc<Config>> {
    let previous = crate::config::readonly_invocation();
    crate::config::set_readonly_invocation(true);
    let result = Config::get().await;
    crate::config::set_readonly_invocation(previous);
    result
}

#[derive(usage_rs::ValueEnum, Debug, Clone, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum LevelFilter {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(usage_rs::Cli)]
#[usage(name = "pakro", about, long_about = LONG_ABOUT, settings,
    logo = include_str!("../../assets/logo.txt"), logo_style = "green",
    example("pakro install python@3.13.16", help = "Install an exact Python version"),
    example("pakro install", help = "Install the Python and packages declared by this project"),
    example("pakro current python", help = "Show the project's current Python selection"),
    arg_required_else_help = true, completion = true, unknown_flags = "error")]
pub(crate) struct Cli {
    #[usage(subcommand)]
    pub command: Option<Commands>,
    #[usage(short='C', long, global=true, value_name="DIR", value_hint=usage_rs::ValueHint::DirPath)]
    pub cd: Option<PathBuf>,
    #[usage(short = 'E', long, global = true)]
    pub env: Option<Vec<String>>,
    #[usage(long, short, global = true, env = "PAKRO_JOBS")]
    pub jobs: Option<usize>,
    #[usage(short = 'n', long, hide = true)]
    pub dry_run: bool,
    #[usage(short = 'P', long, global = true, hide = true, conflicts = "env")]
    pub profile: Option<Vec<String>>,
    #[usage(short = 'q', long, global = true, env = "PAKRO_QUIET", overrides = &["silent", "trace", "verbose", "debug", "log_level"])]
    pub quiet: bool,
    #[usage(short='v', long, global=true, count, overrides = &["quiet", "silent", "trace", "debug"])]
    pub verbose: u8,
    #[usage(long, short = 'V', hide = true)]
    pub version: bool,
    #[usage(short = 'y', long, global = true)]
    pub yes: bool,
    #[usage(long, global = true, hide = true, overrides = &["quiet", "trace", "verbose", "silent", "log_level"])]
    pub debug: bool,
    #[usage(long, global = true, hide = true, value_name = "LEVEL", value_enum, overrides = &["quiet", "trace", "verbose", "silent", "debug"])]
    pub log_level: Option<LevelFilter>,
    #[usage(long)]
    pub no_config: bool,
    #[usage(long)]
    pub no_env: bool,
    #[usage(long)]
    pub no_hooks: bool,
    #[usage(long, global = true)]
    pub raw: bool,
    /// Restore the exact project lock without changing its contents.
    #[usage(long, global = true)]
    pub locked: bool,
    #[usage(long, global = true, overrides = &["quiet", "trace", "verbose", "debug", "log_level"])]
    pub silent: bool,
    #[usage(long, global = true, hide = true, overrides = &["quiet", "silent", "verbose", "debug", "log_level"])]
    pub trace: bool,
}
fn help_style() -> usage_rs::help::Style {
    help_style_for(console::colors_enabled())
}

/// The same question for a page going to stderr, which `console` tracks separately.
fn help_style_stderr() -> usage_rs::help::Style {
    help_style_for(console::colors_enabled_stderr())
}

/// The answer, given what `console` decided. Split out so it can be tested: the test binary
/// disables colour and sets `NO_COLOR` for every test in it, which makes a rendered page plain
/// whichever policy produced it, so only the mapping itself can be pinned.
fn help_style_for(coloured: bool) -> usage_rs::help::Style {
    if coloured {
        usage_rs::help::Style::COLOURED
    } else {
        usage_rs::help::Style::PLAIN
    }
}

/// A help page as this process should print it.
///
/// `usage_rs::help::render` is the plain form, for a page going into a document. pakro
/// dispatches `Error::Help` itself rather than letting `parse()` exit, so the colour policy
/// `parse()` would have applied has to be applied here.
fn render_page(
    spec: &usage_rs::spec::Spec<'static>,
    cmd: &usage_rs::Command<'_>,
    long: bool,
) -> Option<String> {
    usage_rs::help::render_styled(spec, cmd, long, help_style())
}

#[derive(usage_rs::Subcommands, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum Commands {
    Activate(activate::Activate),
    BinPaths(bin_paths::BinPaths),
    Cache(cache::Cache),
    Completion(completion::Completion),
    Config(config::Config),
    Current(current::Current),
    Deactivate(deactivate::Deactivate),
    Doctor(doctor::Doctor),
    Env(env::Env),
    HookEnv(hook_env::HookEnv),
    HookNotFound(hook_not_found::HookNotFound),
    Implode(implode::Implode),
    Install(install::Install),
    Installs(installs::Installs),
    Latest(latest::Latest),
    Ls(ls::Ls),
    LsRemote(ls_remote::LsRemote),
    Project(project::Project),
    Prune(prune::Prune),
    Deps(deps::Deps),
    Registry(registry::Registry),
    Reshim(reshim::Reshim),
    Search(search::Search),
    SelfUpdate(self_update::SelfUpdate),
    Settings(settings::Settings),
    Trust(trust::Trust),
    Uninstall(uninstall::Uninstall),
    Untrust(untrust::Untrust),
    Usage(usage::Usage),
    Version(version::Version),
    Where(r#where::Where),
    Which(r#which::Which),
}
impl Commands {
    async fn preflight(self) -> Result<PreparedCommand> {
        match self {
            Self::Install(command) => Ok(PreparedCommand::Install(command.preflight().await?)),
            Self::Deps(command) => Ok(PreparedCommand::Deps(command.preflight().await?)),
            command => Ok(PreparedCommand::Other(command)),
        }
    }
    fn run_other(self) -> LocalBoxFuture<'static, Result<()>> {
        match self {
            Self::Activate(cmd) => Box::pin(async move { cmd.run() }),
            Self::BinPaths(cmd) => Box::pin(cmd.run()),
            Self::Cache(cmd) => Box::pin(cmd.run()),
            Self::Completion(cmd) => Box::pin(cmd.run()),
            Self::Config(cmd) => Box::pin(cmd.run()),
            Self::Current(cmd) => Box::pin(cmd.run()),
            Self::Deactivate(cmd) => Box::pin(async move { cmd.run() }),
            Self::Doctor(cmd) => Box::pin(cmd.run()),
            Self::Env(cmd) => Box::pin(cmd.run()),
            Self::HookEnv(cmd) => Box::pin(cmd.run()),
            Self::HookNotFound(cmd) => Box::pin(cmd.run()),
            Self::Implode(cmd) => Box::pin(async move { cmd.run() }),
            Self::Install(_) => {
                Box::pin(async { bail!("install execution requires its admitted request") })
            }
            Self::Installs(cmd) => Box::pin(cmd.run()),
            Self::Latest(cmd) => Box::pin(cmd.run()),
            Self::Ls(cmd) => Box::pin(cmd.run()),
            Self::LsRemote(cmd) => Box::pin(cmd.run()),
            Self::Project(cmd) => Box::pin(cmd.run()),
            Self::Prune(cmd) => Box::pin(cmd.run()),
            Self::Deps(_) => {
                Box::pin(async { bail!("dependency installation requires its admitted request") })
            }
            Self::Registry(cmd) => Box::pin(cmd.run()),
            Self::Reshim(cmd) => Box::pin(cmd.run()),
            Self::Search(cmd) => Box::pin(cmd.run()),
            Self::SelfUpdate(cmd) => Box::pin(cmd.run()),
            Self::Settings(cmd) => Box::pin(cmd.run()),
            Self::Trust(cmd) => Box::pin(cmd.run()),
            Self::Uninstall(cmd) => Box::pin(cmd.run()),
            Self::Untrust(cmd) => Box::pin(async move { cmd.run() }),
            Self::Usage(cmd) => Box::pin(async move { cmd.run() }),
            Self::Version(cmd) => Box::pin(cmd.run()),
            Self::Where(cmd) => Box::pin(cmd.run()),
            Self::Which(cmd) => Box::pin(cmd.run()),
        }
    }
}

/// Write commands carry the original admitted owners across backend loading.
/// The parsed Args themselves never cache or clone a project observation.
enum PreparedCommand {
    Install(install::PreparedInstall),
    Deps(deps::PreparedDeps),
    Other(Commands),
}

impl PreparedCommand {
    fn run(self) -> LocalBoxFuture<'static, Result<()>> {
        match self {
            Self::Install(command) => Box::pin(command.run()),
            Self::Deps(command) => Box::pin(command.run()),
            Self::Other(command) => command.run_other(),
        }
    }
}
pub(crate) fn first_non_global_arg_idx(
    cmd: &usage_rs::Command<'_>,
    args: &[String],
) -> Option<usize> {
    pakro_util::args::first_non_global_arg_idx_with(
        |name| {
            cmd.flags.iter().any(|flag| {
                flag.takes_value
                    && (flag.longs.iter().any(|long| name == format!("--{long}"))
                        || flag
                            .shorts
                            .iter()
                            .any(|short| name == format!("-{}", *short as char)))
            })
        },
        args,
    )
}
fn bytecode_recovery_command_index(args: &[String]) -> Option<usize> {
    let mut remaining = args
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, argument)| (index, std::ffi::OsStr::new(argument)))
        .collect::<Vec<_>>();
    loop {
        let argv = remaining
            .iter()
            .map(|(_, argument)| *argument)
            .collect::<Vec<_>>();
        let mut parser = usage_rs::Parser::new(Cli::command(), &argv);
        let mut install = None;
        let mut unknown = None;
        while let Some(event) = parser.next_event() {
            if parser.double_dash_seen() {
                return None;
            }
            match event {
                Ok(usage_rs::Event::Command(command)) if command.name == "install" => {
                    install = Some(remaining[parser.command_start().checked_sub(1)?].0);
                }
                Ok(usage_rs::Event::Flag { flag, .. })
                | Err(usage_rs::Error::MissingFlagValue { flag })
                    if install.is_some() && flag.longs.contains(&"restore-bytecode") =>
                {
                    return install;
                }
                // Unknown flags terminate Parser. For recognition only, remove
                // exactly that borrowed token and restart the original grammar.
                // Full parsing still uses every original argument and rejects it.
                Err(usage_rs::Error::UnknownFlag { token }) => {
                    unknown =
                        Some(argv.iter().position(|argument| {
                            std::ptr::eq(argument.as_encoded_bytes(), token)
                        })?);
                    break;
                }
                Ok(usage_rs::Event::Flag { .. }) => {}
                Ok(usage_rs::Event::Arg { .. }) if install.is_some() => {}
                _ => return None,
            }
        }
        remaining.remove(unknown?);
    }
}

/// The fast path reads no settings, so unless something loaded them there is
/// nothing to report; setting up the logger would load them.
fn finish_hook_env_fast_exit() {
    if crate::config::settings::is_loaded() {
        measure!("logger", { logger::init() });
        Settings::flush_pending_warnings_before_exit();
    }
}

pub(crate) fn register_frontend() {
    crate::frontend::register(crate::frontend::Frontend {
        subcommand_names: || {
            Cli::command()
                .subcommands
                .iter()
                .flat_map(|s| std::iter::once(s.name).chain(s.aliases.iter().copied()))
                .map(str::to_string)
                .collect()
        },
    });
}
impl Cli {
    fn settings_layer(&self, command_local: SettingsPartial) -> SettingsPartial {
        let mut s = command_local;
        if self.raw {
            s.raw = Some(true);
        }
        if self.locked {
            s.locked = Some(true);
        }
        if let Some(cd) = &self.cd {
            s.cd = Some(cd.clone());
        }
        if let Some(jobs) = self.jobs {
            s.jobs = Some(jobs);
        }
        if self.profile.is_some() {
            s.env = self.profile.clone();
        }
        if self.env.is_some() {
            s.env = self.env.clone();
        }
        if self.yes {
            s.yes = Some(true);
        }
        if self.quiet || self.silent {
            s.quiet = Some(true);
        }
        if self.silent {
            s.silent = Some(true);
        }
        if self.trace {
            s.log_level = Some("trace".to_string());
        }
        if self.debug {
            s.log_level = Some("debug".to_string());
        }
        if let Some(log_level) = &self.log_level {
            s.log_level = Some(log_level.to_string());
        }
        if self.verbose > 0 {
            s.verbose = Some(true);
        }
        if self.verbose > 1 {
            s.log_level = Some("trace".to_string());
        }
        s
    }

    pub(crate) fn exit_early_for_unchanged_hook_env(args: &[String]) -> bool {
        if args.get(1).map(String::as_str) != Some("hook-env") {
            return false;
        }
        *crate::env::ARGS.write().unwrap() = args.to_vec();
        if crate::config::pakrorc::init().is_err() {
            return false;
        }
        if !hook_env_module::should_exit_early_fast() {
            return false;
        }
        finish_hook_env_fast_exit();
        true
    }

    pub(crate) async fn run(args: &Vec<String>) -> Result<()> {
        run_with_exit_signal(Self::run_inner(args), ctrlc::exit_signal()).await
    }
    async fn run_inner(args: &Vec<String>) -> Result<()> {
        crate::env::ARGS.write().unwrap().clone_from(args);
        let completion_argv: Vec<std::ffi::OsString> =
            args.iter().skip(1).map(std::ffi::OsString::from).collect();
        if let Some(answer) = completion::usage_spec_request(&completion_argv) {
            pakroprint!("{}", answer?)?;
            return Ok(());
        }
        if let Some(answer) = completion::completion_request(&completion_argv) {
            pakroprint!("{answer}")?;
            return Ok(());
        }
        if let Some(command) = bytecode_recovery_command_index(args) {
            Settings::select_package_query_sources();
            crate::toolset::install_state::init_readonly_plugin_metadata()?;
            let mut recovery_args = args.clone();
            crate::packages::extract_bound_package_args(&mut recovery_args[command + 1..]);
            let argv = recovery_args
                .iter()
                .map(std::ffi::OsStr::new)
                .collect::<Vec<_>>();
            let mut cli =
                Cli::parse_from_argv(&argv).map_err(|err| usage_error(&argv[1..], err))?;
            validate_cd_path(&cli.cd)?;
            Settings::init_package_query(cli.settings_layer(SettingsPartial::empty()))?;
            logger::init();
            let Some(Commands::Install(mut install)) = cli.command.take() else {
                bail!("bytecode recovery requires install");
            };
            install.inherit_recovery_flags(cli.dry_run, cli.raw, cli.jobs);
            return install.preflight().await?.run().await;
        }
        // Reclaim commands use the managed libraries and operation records,
        // even when cwd is missing or unrelated project declarations are bad.
        // Select isolation before parsing ToolArg can trigger lazy settings.
        if first_non_global_arg_idx(Cli::command(), args)
            .is_some_and(|index| matches!(args[index].as_str(), "prune" | "uninstall"))
        {
            Settings::select_package_query_sources();
            crate::config::set_readonly_invocation(true);
            crate::toolset::install_state::init_readonly_plugin_metadata()?;
            let argv = args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>();
            let mut cli =
                Cli::parse_from_argv(&argv).map_err(|err| usage_error(&argv[1..], err))?;
            let readonly = match &mut cli.command {
                Some(Commands::Prune(command)) => {
                    command.inherit_root_dry_run(cli.dry_run);
                    command.is_dry_run()
                }
                Some(Commands::Uninstall(command)) => {
                    command.inherit_root_dry_run(cli.dry_run);
                    command.is_dry_run()
                }
                _ => bail!("catalog reclamation requires prune or uninstall"),
            };
            validate_cd_path(&cli.cd)?;
            Settings::init_package_query(cli.settings_layer(SettingsPartial::empty()))?;
            logger::init();
            crate::config::set_readonly_invocation(readonly);
            return cli
                .command
                .take()
                .expect("checked catalog reclamation command")
                .preflight()
                .await?
                .run()
                .await;
        }
        // Forget is keyed by the persistent catalog ID. Project discovery,
        // trust and backend loading must not depend on a directory that may
        // already have been removed, or on unrelated local declarations.
        if first_non_global_arg_idx(Cli::command(), args)
            .is_some_and(|index| args[index] == "project")
        {
            let argv = args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>();
            let mut cli =
                Cli::parse_from_argv(&argv).map_err(|err| usage_error(&argv[1..], err))?;
            if let Some(Commands::Project(command)) = &mut cli.command {
                command.inherit_root_dry_run(cli.dry_run);
            }
            if matches!(&cli.command, Some(Commands::Project(command)) if command.is_catalog_only())
            {
                validate_cd_path(&cli.cd)?;
                Settings::select_package_query_sources();
                Settings::init_package_query(cli.settings_layer(SettingsPartial::empty()))?;
                logger::init();
                let Some(Commands::Project(command)) = cli.command.take() else {
                    unreachable!("catalog project command was just checked")
                };
                crate::config::set_readonly_invocation(command.is_readonly());
                return command.run().await;
            }
        }
        crate::config::set_readonly_invocation(true);
        crate::config::pakrorc::init()?;
        if hook_env_module::should_exit_early_fast() {
            finish_hook_env_fast_exit();
            return Ok(());
        }
        logger::init();
        check_working_directory();
        shim::handle_shim().await?;
        let print_version = version::print_version_if_requested(args)?;
        crate::toolset::install_state::init_readonly_plugin_metadata()?;
        let mut processed_args = args.clone();
        if let Some(index) = first_non_global_arg_idx(Cli::command(), args)
            && matches!(args[index].as_str(), "install" | "i")
        {
            crate::packages::extract_bound_package_args(&mut processed_args[index + 1..]);
        }
        let argv = processed_args
            .iter()
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>();
        let (mut cli, cli_settings) = Cli::parse_from_argv_with_settings(&argv)
            .map_err(|err| usage_error(&argv[1..], err))?;
        if let Some(Commands::Install(install)) = &mut cli.command {
            install.inherit_root_dry_run(cli.dry_run);
        }
        if let Some(Commands::Deps(command)) = &mut cli.command {
            command.inherit_root_dry_run(cli.dry_run);
        }
        if let Some(Commands::Project(command)) = &mut cli.command {
            command.inherit_root_dry_run(cli.dry_run);
        }
        if let Some(Commands::Prune(command)) = &mut cli.command {
            command.inherit_root_dry_run(cli.dry_run);
        }
        if let Some(Commands::Uninstall(command)) = &mut cli.command {
            command.inherit_root_dry_run(cli.dry_run);
        }
        config_file::set_implicitly_trust_active_config(matches!(
            cli.command.as_ref(),
            Some(Commands::Install(_) | Commands::Deps(_))
        ));
        validate_cd_path(&cli.cd)?;
        Settings::add_cli_matches(cli.settings_layer(command_local_settings(&cli_settings)?));
        if let Err(error) = Settings::try_get() {
            if let Some(Commands::HookEnv(command)) = &cli.command {
                if let Err(withdrawal) = command.withdraw_previous_environment() {
                    warn!("could not withdraw the previous managed environment: {withdrawal:#}");
                }
            }
            return Err(error);
        }
        if print_version {
            return Err(request_exit(0));
        }
        let prepared = match cli.command.take() {
            Some(command) => Some(command.preflight().await?),
            None => None,
        };
        backend::load_tools().await?;
        match prepared {
            Some(command) => command.run().await,
            None => {
                if let Some(page) = render_page(Cli::spec(), Cli::command(), false) {
                    pakroprint!("{page}")?;
                }
                Err(request_exit(1))
            }
        }
    }
}
async fn run_with_exit_signal<T>(
    command: impl std::future::Future<Output = Result<T>>,
    exit_signal: impl std::future::Future<Output = i32>,
) -> Result<T> {
    tokio::select! {
        result = command => result,
        code = exit_signal => Err(request_exit(code)),
    }
}

fn usage_error(argv: &[&std::ffi::OsStr], err: usage_rs::Error<'_, '_>) -> Report {
    let spec = Cli::spec();
    match err {
        usage_rs::Error::Help { cmd, long } => {
            if let Some(page) = render_page(spec, cmd, long)
                && let Err(err) = pakroprint!("{page}")
            {
                return err.into();
            }
            request_exit(0)
        }
        usage_rs::Error::HelpAll { cmd } => {
            if let Some(page) = usage_rs::help::render_all_styled(spec, cmd, help_style())
                && let Err(err) = pakroprint!("{page}")
            {
                return err.into();
            }
            request_exit(0)
        }
        usage_rs::Error::MissingArgsHelp { cmd } => {
            // stderr, which `console` tracks separately from stdout.
            if let Some(page) = usage_rs::help::render_styled(spec, cmd, false, help_style_stderr())
            {
                let _ = calm_io::stderr!("{page}");
            }
            request_exit(2)
        }
        usage_rs::Error::Version { long } => {
            let version = if long {
                spec.long_version.or(spec.version)
            } else {
                spec.version
            }
            .unwrap_or_default();
            if let Err(err) = pakroprint!("{} {version}\n", spec.name) {
                return err.into();
            }
            request_exit(0)
        }
        err => {
            let _ = calm_io::stderr!("{}", usage_rs::render_failure(spec, argv, &err));
            request_exit(2)
        }
    }
}

const LONG_ABOUT: &str = "Pakro manages shared Python runtimes, packages and project selections. Maintain declarations in pakro.toml, install explicitly, and use the selected environment from the project directory.";
/// Check if the current working directory exists and warn if not
fn check_working_directory() {
    if std::env::current_dir().is_err() {
        // Try to get the directory path from PWD env var, which might still contain the old path
        let dir_path = std::env::var("PWD")
            .or_else(|_| std::env::var("OLDPWD"))
            .unwrap_or_else(|_| "(unknown)".to_string());
        warn!(
            "Current directory does not exist or is not accessible: {}",
            dir_path
        );
    }
}

/// Validate the --cd path if provided and return an error if it doesn't exist
fn validate_cd_path(cd: &Option<PathBuf>) -> Result<()> {
    if let Some(path) = cd {
        if !path.exists() {
            bail!(
                "Directory specified with --cd does not exist: {}\n\
                 Please check the path and try again.",
                ui::style::epath(path)
            );
        }
        if !path.is_dir() {
            bail!(
                "Path specified with --cd is not a directory: {}\n\
                 Please provide a valid directory path.",
                ui::style::epath(path)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parses(arguments: &[&str]) -> bool {
        let argv = arguments
            .iter()
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>();
        Cli::parse_from_argv(&argv).is_ok()
    }
    #[test]
    fn retired_commands_are_not_public_entry_points() {
        for name in [
            "install-into",
            "use",
            "unuse",
            "upgrade",
            "bootstrap",
            "dotfiles",
            "daemons",
            "exec",
            "run",
            "tasks",
            "watch",
            "shell",
            "ssh",
            "mcp",
            "generate",
        ] {
            assert!(!parses(&["pakro", name]), "retired command {name} parsed");
        }
        for arguments in [
            vec!["pakro", "deps", "add", "python:idna"],
            vec!["pakro", "project", "init"],
            vec!["pakro", "installs", "migrate"],
            vec!["pakro", "prune", "--resume", "00000000-0000-0000-0000-000000000001"],
        ] {
            assert!(!parses(&arguments), "{arguments:?}");
        }
    }
    #[test]
    fn retained_directory_and_exact_install_entry_points_parse() {
        for arguments in [
            vec!["pakro", "install", "python@3.13.16"],
            vec!["pakro", "--locked", "install"],
            vec!["pakro", "install", "--dry-run"],
            vec!["pakro", "activate", "bash"],
            vec!["pakro", "where", "python"],
            vec!["pakro", "env", "--json"],
            vec!["pakro", "current", "python"],
            vec!["pakro", "project", "ls"],
            vec!["pakro", "deps", "install", "python"],
            vec!["pakro", "project", "status"],
            vec!["pakro", "prune"],
            vec!["pakro", "prune", "--dry-run"],
            vec![
                "pakro",
                "project",
                "forget",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec![
                "pakro",
                "project",
                "forget",
                "00000000-0000-0000-0000-000000000001",
                "--dry-run",
            ],
            vec!["pakro", "installs", "ls", "--json"],
            vec!["pakro", "hook-env", "-s", "zsh", "--force"],
            vec![
                "pakro",
                "uninstall",
                "--package",
                "python@3.13.16+idna@3.10",
            ],
        ] {
            assert!(parses(&arguments), "{arguments:?}");
        }
    }
    #[test]
    fn project_forget_is_a_catalog_write_and_preview_is_readonly() {
        for (arguments, expected_readonly) in [
            (vec!["pakro", "project", "ls"], true),
            (vec!["pakro", "project", "status"], true),
            (
                vec![
                    "pakro",
                    "project",
                    "forget",
                    "00000000-0000-0000-0000-000000000001",
                ],
                false,
            ),
            (
                vec![
                    "pakro",
                    "project",
                    "forget",
                    "00000000-0000-0000-0000-000000000001",
                    "--dry-run",
                ],
                true,
            ),
        ] {
            let argv = arguments
                .iter()
                .map(std::ffi::OsStr::new)
                .collect::<Vec<_>>();
            let cli = Cli::parse_from_argv(&argv).unwrap();
            let Some(Commands::Project(command)) = cli.command else {
                panic!("project command missing")
            };
            assert_eq!(command.is_readonly(), expected_readonly);
        }
        assert!(!parses(&["pakro", "project", "forget"]));
    }

    #[test]
    fn p4_catalog_commands_inherit_root_preview() {
        for arguments in [
            vec!["pakro", "-n", "prune"],
            vec!["pakro", "-n", "uninstall", "python@3.13.16"],
        ] {
            let argv = arguments
                .iter()
                .map(std::ffi::OsStr::new)
                .collect::<Vec<_>>();
            let mut cli = Cli::parse_from_argv(&argv).unwrap();
            match &mut cli.command {
                Some(Commands::Prune(command)) => {
                    command.inherit_root_dry_run(cli.dry_run);
                    assert!(command.is_dry_run());
                }
                Some(Commands::Uninstall(command)) => {
                    command.inherit_root_dry_run(cli.dry_run);
                    assert!(command.is_dry_run());
                }
                _ => panic!("catalog command missing"),
            }
        }
        for arguments in [
            vec!["pakro", "prune", "--force"],
            vec!["pakro", "prune", "--layout", "layout-id"],
            vec!["pakro", "uninstall", "--layout", "layout-id"],
        ] {
            assert!(!parses(&arguments));
        }
    }
    #[test]
    fn recovery_preflight_is_scoped_to_explicit_install() {
        let arguments = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            bytecode_recovery_command_index(&arguments(&[
                "pakro",
                "--quiet",
                "install",
                "python@3.11.17+20261013",
                "--restore-bytecode",
                "instance"
            ])),
            Some(2)
        );
        assert_eq!(
            bytecode_recovery_command_index(&arguments(&[
                "pakro",
                "install",
                "--",
                "--restore-bytecode",
                "instance"
            ])),
            None
        );
        assert_eq!(
            bytecode_recovery_command_index(&arguments(&[
                "pakro",
                "env",
                "--restore-bytecode",
                "instance"
            ])),
            None
        );
    }
}
