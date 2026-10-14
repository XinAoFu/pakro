use eyre::{Result, bail};

use crate::config::Config;
use crate::config::Settings;
use crate::system;
use crate::system::history::OperationScope;
#[cfg(unix)]
use crate::system::packages::SystemPackageManager;
#[cfg(unix)]
use crate::system::packages::brew;
use crate::ui::prompt;

/// Prune installed system packages no longer declared in `[bootstrap.packages]`
///
/// Supports Homebrew formulae and conservatively removable mise-owned casks.
/// Pruning keeps packages needed by the current config or by trusted, loadable
/// tracked configs.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(
        r###"mise bootstrap packages prune --manager brew
mise bootstrap packages prune --manager brew --dry-run
mise bootstrap packages prune --manager brew --yes
mise bootstrap packages prune --manager brew-cask --dry-run"###
    )
)]
pub(crate) struct SystemPrune {
    /// Only prune packages for this manager
    #[usage(long, short, default = "brew")]
    manager: String,

    /// Print what would be removed without deleting anything
    #[usage(long, short = 'n')]
    dry_run: bool,

    /// Skip the confirmation prompt
    #[usage(long, short)]
    yes: bool,
}

impl SystemPrune {
    pub(crate) async fn run(self) -> Result<()> {
        OperationScope::wrap("bootstrap packages prune", self.dry_run, self.run_inner()).await
    }

    async fn run_inner(self) -> Result<()> {
        if Settings::get()
            .system_packages
            .managers
            .as_ref()
            .is_some_and(|enabled| !enabled.contains(&self.manager))
        {
            bail!(
                "manager '{}' is excluded by the system_packages.managers setting",
                self.manager
            );
        }
        match self.manager.as_str() {
            "brew" => self.run_brew().await,
            "brew-cask" => self.run_brew_cask().await,
            _ => bail!(
                "package manager '{}' does not support pruning",
                self.manager
            ),
        }
    }

    #[cfg(unix)]
    async fn run_brew(self) -> Result<()> {
        debug_assert_eq!(self.manager, "brew");
        let manager = brew::BrewManager::new();
        if !manager.is_available() {
            bail!("brew is not available: {}", manager.unavailable_reason());
        }
        let config = Config::get().await?;
        let packages = system::packages_from_config_and_tracked_config_files(&config).await?;
        let mut configured = packages
            .iter()
            .find(|mp| mp.manager.name() == "brew")
            .map(|mp| mp.requests.clone())
            .unwrap_or_default();
        let configured_casks = packages
            .iter()
            .find(|mp| mp.manager.name() == "brew-cask")
            .map(|mp| mp.requests.as_slice())
            .unwrap_or_default();
        configured.extend(brew::cask_formula_dependencies(configured_casks).await?);
        let plan = match brew::prune_plan(&configured).await {
            Ok(plan) => plan,
            Err(err) => return Err(with_declaring_config_files(&config, err).await),
        };
        if plan.is_empty() {
            info!("brew: nothing to prune");
            return Ok(());
        }
        if self.dry_run {
            brew::apply_prune_plan(&plan, true)?;
            return Ok(());
        }
        let remove = plan
            .remove
            .iter()
            .map(|c| format!("{}@{}", c.name, c.version))
            .collect::<Vec<_>>();
        if !self.yes && !Settings::get().yes && console::user_attended_stderr() {
            let msg = format!("brew: prune {}?", remove.join(", "));
            if !prompt::confirm(msg)?.is_yes() {
                info!("brew: skipped");
                return Ok(());
            }
        }
        let removed = plan.remove.len();
        brew::apply_prune_plan(&plan, false)?;
        info!("brew: pruned {removed} formulae");
        Ok(())
    }

    #[cfg(unix)]
    async fn run_brew_cask(self) -> Result<()> {
        debug_assert_eq!(self.manager, "brew-cask");
        let manager = brew::BrewCaskManager::new();
        if !manager.is_available() {
            bail!(
                "brew-cask is not available: {}",
                manager.unavailable_reason()
            );
        }
        let config = Config::get().await?;
        let configured = system::packages_from_config_and_tracked_config_files(&config)
            .await?
            .into_iter()
            .find(|mp| mp.manager.name() == "brew-cask")
            .map(|mp| mp.requests)
            .unwrap_or_default();
        let plan = brew::cask_prune_plan(&configured).await?;
        for skipped in &plan.skipped {
            warn!("brew-cask:{}: skipped: {}", skipped.token, skipped.reason);
        }
        if plan.is_empty() {
            info!("brew-cask: nothing to prune");
            return Ok(());
        }
        if self.dry_run {
            brew::apply_cask_prune_plan(&plan, true)?;
            return Ok(());
        }
        let remove = plan
            .remove
            .iter()
            .map(|candidate| format!("{}@{}", candidate.token, candidate.version))
            .collect::<Vec<_>>();
        if !self.yes && !Settings::get().yes && console::user_attended_stderr() {
            let msg = format!("brew-cask: prune {}?", remove.join(", "));
            if !prompt::confirm(msg)?.is_yes() {
                info!("brew-cask: skipped");
                return Ok(());
            }
        }
        let removed = brew::apply_cask_prune_plan(&plan, false)?;
        info!("brew-cask: pruned {removed} casks");
        Ok(())
    }

    #[cfg(not(unix))]
    async fn run_brew(self) -> Result<()> {
        let _ = self.manager;
        bail!("brew prune is not supported on windows")
    }

    #[cfg(not(unix))]
    async fn run_brew_cask(self) -> Result<()> {
        let _ = self.manager;
        bail!("brew-cask prune is not supported on windows")
    }
}

/// Prune keeps packages declared by every tracked config, so a formula that
/// cannot be resolved may come from a project other than the current one.
/// Name the config files that declare it.
#[cfg(unix)]
async fn with_declaring_config_files(
    config: &std::sync::Arc<Config>,
    err: eyre::Report,
) -> eyre::Report {
    let Some(name) = brew::failed_formula_name(&err).map(str::to_string) else {
        return err;
    };
    match system::config_files_declaring_package(config, "brew", &name).await {
        Ok(paths) if !paths.is_empty() => {
            let paths = paths
                .iter()
                .map(crate::file::display_path)
                .collect::<Vec<_>>()
                .join(", ");
            err.wrap_err(format!("brew:{name} is declared in {paths}"))
        }
        Ok(_) => err,
        Err(lookup_err) => {
            debug!("brew: could not find the config declaring {name}: {lookup_err:#}");
            err
        }
    }
}
