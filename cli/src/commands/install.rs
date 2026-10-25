use crate::args::{BackendArg, ToolArg};
use crate::config::{Config, Settings};
use crate::packages::PackageInstallRequest;
use eyre::{Result, ensure};
use std::str::FromStr;

/// Install the exact Python and package versions declared by this project.
/// Contents are shared in the Pakro library; project declarations are not edited.
#[derive(Debug, Default, usage_rs::Args)]
#[usage(
    visible_alias = "i",
    verbatim_doc_comment,
    example("pakro install python@3.13.16"),
    example("pakro install"),
    example("pakro install --dry-run")
)]
pub(crate) struct Install {
    #[usage(value_name = "PYTHON@VERSION")]
    tool: Option<Vec<ToolArg>>,
    #[usage(long, short = 'n')]
    dry_run: bool,
    /// Restore only the previously approved four bytecode files in one exact instance.
    #[usage(long, value_name = "INSTANCE")]
    restore_bytecode: Option<String>,
    #[usage(long, short, env = "PAKRO_JOBS")]
    jobs: Option<usize>,
    #[usage(long)]
    raw: bool,
}

struct Input {
    exact: Option<String>,
    extra: Vec<PackageInstallRequest>,
}
impl Install {
    pub(super) fn inherit_root_dry_run(&mut self, dry_run: bool) {
        self.dry_run |= dry_run;
    }
    pub(super) fn inherit_recovery_flags(&mut self, dry_run: bool, raw: bool, jobs: Option<usize>) {
        self.dry_run |= dry_run;
        self.raw |= raw;
        self.jobs = self.jobs.or(jobs);
    }
    async fn input(&self) -> Result<Input> {
        let config = super::read_config_for_admission().await?;
        let tools = self.tool.clone().unwrap_or_default();
        let exact = match tools.as_slice() {
            [one] if crate::programs::python::is_python_backend(&one.ba) => one.version.clone(),
            [one, two]
                if crate::programs::python::is_python_backend(&one.ba)
                    && one.version.is_none()
                    && two.version.is_none() =>
            {
                Some(two.short.clone())
            }
            _ => None,
        };
        let mut extra = crate::packages::request::CLI_PACKAGE_ARGS
            .get()
            .cloned()
            .unwrap_or_default();
        if exact.is_none() {
            let requests = config.get_tool_request_set().await?;
            let runtimes = requests
                .iter()
                .filter_map(|(backend, requests, _)| {
                    crate::programs::python::is_python_backend(backend)
                        .then(|| {
                            requests
                                .first()
                                .map(|request| ("python".to_owned(), request.version().to_owned()))
                        })
                        .flatten()
                })
                .collect::<Vec<_>>();
            let (kept, bare) =
                crate::packages::split_bare_packages(tools.clone(), &runtimes, |name| {
                    BackendArg::from_str(name)
                        .ok()
                        .and_then(|backend| backend.backend().ok())
                        .is_some()
                })
                .map_err(|error| eyre::eyre!("{error}"))?;
            ensure!(
                kept.is_empty()
                    || matches!(kept.as_slice(), [tool] if crate::programs::python::is_python_backend(&tool.ba) && tool.version.is_none()),
                "installation currently supports one shared Python target or its exact package pins"
            );
            extra.extend(bare);
        }
        ensure!(
            extra.iter().all(|request| request.runtime_name == "python"),
            "only shared Python package bindings are supported"
        );
        for tool in tools
            .iter()
            .filter(|tool| crate::programs::python::is_python_backend(&tool.ba))
        {
            ensure!(
                tool.ba.opts.is_none() && tool.ba.full() == tool.ba.full_without_opts(),
                "Python installation options are not supported"
            );
        }
        if let Some(version) = &exact {
            ensure!(
                crate::programs::directory::is_exact_python_version(version),
                "Python installation requires a complete exact version, e.g. 3.13.16"
            );
        }
        let project = crate::programs::python::prepare_current_project(&extra)?;
        if exact.is_none() {
            let project = project.ok_or_else(|| eyre::eyre!("declare an exact Python version in pakro.toml before installing project packages"))?;
            ensure!(
                project.only_python_tools(),
                "this installation path prepares Python-only project declarations"
            );
        }
        Ok(Input { exact, extra })
    }
    pub(super) async fn preflight(&self) -> Result<()> {
        if self.restore_bytecode.is_none() {
            self.input().await?;
        }
        Ok(())
    }
    pub(crate) async fn run(self) -> Result<()> {
        if let Some(instance) = &self.restore_bytecode {
            return self.run_bytecode_recovery(instance);
        }
        let input = self.input().await?;
        let config = Config::get().await?;
        if let Some(version) = input.exact {
            if self.dry_run {
                pakroprintln!("Would install Python {version} and its matching declared packages");
                return Ok(());
            }
            crate::programs::python::install_exact_python(&version, config, &input.extra).await
        } else {
            let project = crate::programs::python::prepare_current_project(&input.extra)?
                .ok_or_else(|| eyre::eyre!("the project no longer declares Python"))?;
            if self.dry_run {
                pakroprintln!(
                    "Would install the declared Python {} and its exact project packages",
                    project.version()
                );
                return Ok(());
            }
            project.install(config).await
        }
    }
    fn run_bytecode_recovery(&self, instance: &str) -> Result<()> {
        eyre::ensure!(
            !self.raw
                && self.jobs.is_none()
                && crate::packages::request::CLI_PACKAGE_ARGS
                    .get()
                    .is_none_or(Vec::is_empty),
            "bytecode recovery does not support installation options or package requests"
        );
        let version = match self.tool.as_deref().unwrap_or(&[]) {
            [single]
                if single.short == "python"
                    && !single.ba.has_explicit_backend()
                    && single.ba.opts.is_none() =>
            {
                single.version.as_deref()
            }
            [first, second]
                if first.short == "python"
                    && !first.ba.has_explicit_backend()
                    && first.ba.opts.is_none()
                    && first.version.is_none()
                    && !second.ba.has_explicit_backend()
                    && second.ba.opts.is_none()
                    && second.version.is_none() =>
            {
                Some(second.short.as_str())
            }
            _ => None,
        };
        let version = version.filter(|version| {
            version
                .split_once('+')
                .is_some_and(|(_, build)| !build.is_empty())
                && crate::programs::directory::is_exact_python_version(version)
        });
        eyre::ensure!(
            version.is_some(),
            "bytecode recovery requires one complete explicit Python version including its build"
        );
        let report = crate::programs::library::restore_python_bytecode(
            version.expect("validated exact Python recovery request"),
            instance,
            self.dry_run,
        )?;
        if Settings::get().quiet {
            return Ok(());
        }
        if report.already_healthy {
            pakroprintln!(
                "Shared Python {} is already healthy; no files changed",
                report.instance
            );
        } else {
            pakroprintln!(
                "{} {} bytecode files in {}",
                if report.restored {
                    "Restored"
                } else {
                    "Would restore"
                },
                report.paths.len(),
                report.instance
            );
            for path in &report.paths {
                pakroprintln!("  {}", path.display());
            }
        }
        pakroprintln!(
            "Verified {} unchanged inventory entries",
            report.unchanged_entries
        );
        Ok(())
    }
}
