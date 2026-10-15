use std::collections::HashSet;
use std::sync::Arc;

use crate::args::ToolArg;
use crate::config::{Config, Settings, SettingsExt};
use crate::packages::install::{
    PackageRuntimeBinding, PythonPackageLatest, latest_python_package, snapshot_runtime_bindings,
};
use crate::toolset::outdated_info::OutdatedInfo;
use crate::toolset::{ConfigScope, ResolveOptions, ToolsetBuilder};
use crate::ui::table;
use eyre::Result;
use indexmap::IndexMap;
use tabled::settings::Remove;
use tabled::settings::location::ByColumnName;

/// Show outdated tool versions
///
/// See `pakro upgrade` to upgrade these versions.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment, after_long_help = AFTER_LONG_HELP,
    example(r###"pakro outdated
Plugin  Requested  Current  Latest
python  3.11       3.11.0   3.11.1
node    20         20.0.0   20.1.0"###),
    example(r###"pakro outdated node
Plugin  Requested  Current  Latest
node    20         20.0.0   20.1.0"###),
    example(r###"pakro outdated --json
{"python": {"requested": "3.11", "current": "3.11.0", "latest": "3.11.1"}, ...}"###),
    example(r###"pakro outdated --local
Plugin  Requested  Current  Latest
node    20         20.0.0   20.1.0"###))]
pub(crate) struct Outdated {
    /// Tool(s) to show outdated versions for
    /// e.g.: node@20 python@3.10
    /// If not specified, all tools in global and local configs will be shown
    #[usage(value_name = "TOOL@VERSION", verbatim_doc_comment)]
    pub tool: Vec<ToolArg>,

    /// Compare against the latest versions available, not just those matching the current config
    ///
    /// For example, with `node = "20"` in your config, `pakro outdated` normally only reports newer
    /// 20.x versions. With this flag it reports the newest version overall, such as 22.x.
    #[usage(long, short = 'b', verbatim_doc_comment)]
    pub bump: bool,

    /// Output in JSON format
    #[usage(short = 'J', long, verbatim_doc_comment)]
    pub json: bool,

    /// Deprecated shorthand for --bump
    #[usage(short = 'l', hide = true)]
    pub legacy_bump: bool,

    /// Show outdated tools including installed-but-inactive tools not present in the current config
    ///
    /// By default, `pakro outdated` only shows tools that come from the current config.
    #[usage(long, verbatim_doc_comment, conflicts = "local")]
    pub inactive: bool,

    /// Only show outdated tools defined in local config files
    ///
    /// This will only show tools that are defined in project-local pakro.toml and
    /// will skip tools defined in the global config (~/.config/pakro/config.toml).
    #[usage(long, verbatim_doc_comment)]
    pub local: bool,

    /// Placeholder for future monorepo outdated checks; `pakro outdated --monorepo` is not implemented yet.
    #[usage(long, verbatim_doc_comment)]
    pub monorepo: bool,

    /// Don't show table header
    #[usage(long)]
    pub no_header: bool,
}

impl Outdated {
    pub(crate) async fn run(mut self) -> Result<()> {
        if self.legacy_bump {
            deprecated_at!(
                "2026.8.5",
                "2027.8.5",
                "cli.outdated.bump-l",
                "`pakro outdated -l` is deprecated. Use `pakro outdated -b` or `pakro outdated --bump` instead. After removal, `-l` will become shorthand for `--local`."
            );
            self.bump = true;
        }
        if self.monorepo {
            unimplemented!("pakro outdated --monorepo is not implemented yet");
        }
        let config = Config::get().await?;
        let scope = if self.local {
            ConfigScope::LocalOnly
        } else {
            ConfigScope::All
        };
        let mut ts = ToolsetBuilder::new()
            .with_args(&self.tool)
            .with_scope(scope)
            .build(&config)
            .await?;
        let package_bindings = snapshot_runtime_bindings(
            &config,
            &ts.list_current_installed_versions(&config)
                .into_iter()
                .map(|(_, tool)| tool)
                .collect::<Vec<_>>(),
            Settings::get().locked,
        );
        let tool_set = self
            .tool
            .iter()
            .map(|t| t.ba.clone())
            .collect::<HashSet<_>>();
        ts.versions
            .retain(|_, tvl| tool_set.is_empty() || tool_set.contains(&tvl.backend));
        let outdated = ts
            .list_outdated_versions(
                &config,
                self.bump,
                &ResolveOptions {
                    inactive: self.inactive,
                    ..Default::default()
                },
            )
            .await;
        let bump_available = if !self.json && !self.bump && outdated.is_empty() {
            ts.list_outdated_versions(
                &config,
                true,
                &ResolveOptions {
                    inactive: self.inactive,
                    ..Default::default()
                },
            )
            .await
            .iter()
            .any(|o| o.bump.is_some())
        } else {
            false
        };
        self.display(outdated, bump_available)?;
        if !self.bump {
            self.display_packages(&config, &package_bindings).await?;
        }
        Ok(())
    }

    fn display(&self, outdated: Vec<OutdatedInfo>, bump_available: bool) -> Result<()> {
        match self.json {
            true => self.display_json(outdated)?,
            false => self.display_table(outdated, bump_available)?,
        }
        Ok(())
    }

    async fn display_packages(
        &self,
        config: &Arc<Config>,
        bindings: &[PackageRuntimeBinding],
    ) -> Result<()> {
        let requests = config.package_requests();
        if requests.is_empty() {
            return Ok(());
        }
        let index = Arc::new(pakro_index::Index::from_env(
            crate::dirs::CACHE.join("index"),
        ));
        let offline = Settings::get().offline();
        pakroprintln!("\nPackages:");
        for request in &requests {
            let (latest, marker) = match request.runtime_name.as_str() {
                "python" => python_package_display(
                    latest_python_package(
                        Arc::clone(&index),
                        &request.name,
                        &request.runtime_version,
                        bindings,
                        offline,
                    )
                    .await,
                    &request.version,
                ),
                "node" => {
                    let latest = index
                        .latest_stable_npm(&request.name)
                        .await
                        .unwrap_or(None)
                        .unwrap_or_else(|| "?".into());
                    let marker = if latest == request.version {
                        ""
                    } else {
                        " (outdated)"
                    };
                    (latest, marker)
                }
                _ => ("unavailable: unsupported package ecosystem".into(), ""),
            };
            pakroprintln!(
                "  {}@{} {} {} -> {}{marker}",
                request.runtime_name,
                request.runtime_version,
                request.name,
                request.version,
                latest
            );
        }
        Ok(())
    }

    fn display_table(&self, outdated: Vec<OutdatedInfo>, bump_available: bool) -> Result<()> {
        if outdated.is_empty() {
            info!("All tools are up to date");
            if bump_available {
                info!(
                    "Newer versions are available outside the configured version ranges. Use `pakro outdated --bump` to view them."
                );
            }
            return Ok(());
        }
        let mut table = tabled::Table::new(outdated);
        if !self.bump {
            table.with(Remove::column(ByColumnName::new("bump")));
        }
        table::print(&mut table, self.no_header)?;
        Ok(())
    }

    fn display_json(&self, outdated: Vec<OutdatedInfo>) -> Result<()> {
        let mut map = IndexMap::new();
        for o in outdated {
            map.insert(o.name.to_string(), o);
        }
        pakroprintln!("{}", serde_json::to_string_pretty(&map)?);
        Ok(())
    }
}

fn python_package_display(
    result: Result<PythonPackageLatest>,
    current: &str,
) -> (String, &'static str) {
    match result {
        Ok(PythonPackageLatest::Available(Some(version))) => {
            let marker = if version == current {
                ""
            } else {
                " (outdated)"
            };
            (version, marker)
        }
        Ok(PythonPackageLatest::Available(None)) => {
            ("no compatible stable wheel candidate".into(), "")
        }
        Ok(PythonPackageLatest::Unavailable(reason)) => (format!("unavailable: {reason}"), ""),
        Err(error) => (format!("error: {error}"), ""),
    }
}

static AFTER_LONG_HELP: &str = color_print::cstr!(
    r###"<bold><underline>Deprecation:</underline></bold>

The `-l` shorthand for `--bump` is deprecated and will be removed in pakro 2027.8.5.
After removal, `-l` will become shorthand for `--local`. Use `-b` or `--bump` instead."###
);

#[cfg(test)]
mod python_package_tests {
    use super::*;

    #[test]
    fn only_actual_available_candidate_is_marked_outdated() {
        assert_eq!(
            python_package_display(
                Ok(PythonPackageLatest::Available(Some("2.0".into()))),
                "1.0"
            ),
            ("2.0".into(), " (outdated)")
        );
        assert_eq!(
            python_package_display(
                Ok(PythonPackageLatest::Available(Some("1.0".into()))),
                "1.0"
            ),
            ("1.0".into(), "")
        );
        for result in [
            Ok(PythonPackageLatest::Available(None)),
            Ok(PythonPackageLatest::Unavailable("no actual binding".into())),
            Err(eyre::eyre!("registry failure")),
        ] {
            let (text, marker) = python_package_display(result, "1.0");
            assert!(!text.is_empty());
            assert_ne!(text, "?");
            assert_eq!(marker, "");
        }
    }
}
