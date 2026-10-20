use console::style;
use eyre::{Result, bail};

use crate::args::BackendArg;
use crate::backend::Backend;
use crate::config::Config;
use crate::toolset::{Toolset, ToolsetBuilder};

/// Shows current active and installed runtime versions
///
/// This is similar to `pakro ls --current`, but this only shows the runtime
/// and/or version. It's designed to fit into scripts more easily.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    hide = true,
    example(
        r###"pakro current
python 3.11.0 3.10.0
shfmt 3.6.0
shellcheck 0.9.0
node 20.0.0"###,
        help = r###"outputs `.tool-versions` compatible format"###
    ),
    example(
        r###"pakro current node
20.0.0"###
    ),
    example(
        r###"pakro current python
3.11.0 3.10.0"###,
        help = r###"can output multiple versions"###
    )
)]
pub(crate) struct Current {
    /// Plugin to show versions of
    /// e.g.: ruby, node, cargo:eza, github:cli/cli, etc.
    #[usage()]
    plugin: Option<BackendArg>,
}

impl Current {
    pub(crate) async fn run(self) -> Result<()> {
        if self
            .plugin
            .as_ref()
            .is_none_or(|plugin| plugin.short == "python")
        {
            let report = crate::programs::library::local_project()?;
            let has_python = report.current.is_some();
            if let Some(current) = report.current {
                if self.plugin.is_none() {
                    pakroprintln!("python {}", current.requested);
                } else {
                    pakroprintln!("{}", current.requested);
                }
                if current.status != "ready" {
                    warn!(
                        "python {}: {} {}",
                        current.requested,
                        current.status,
                        current.detail.unwrap_or_default()
                    );
                }
            }
            if self.plugin.is_some()
                || (!report.has_legacy_tools && !report.legacy_installs_present)
            {
                return Ok(());
            }
            if has_python {
                let config = Config::get().await?;
                let mut ts = ToolsetBuilder::new().build_unresolved(&config)?;
                ts.versions.retain(|backend, _| backend.short != "python");
                if let Err(error) = ts
                    .resolve_with_opts(
                        &config,
                        &crate::toolset::ResolveOptions {
                            offline: true,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    warn!("failed to resolve local toolset: {error}");
                }
                return self.all(ts).await;
            }
        }
        let config = Config::get().await?;
        let ts = ToolsetBuilder::new().build(&config).await?;
        match &self.plugin {
            Some(ba) => {
                if let Some(plugin) = ba.backend()?.plugin()
                    && !plugin.is_installed()
                {
                    bail!("Plugin {ba} is not installed");
                }
                self.one(ts, ba.backend()?.as_ref()).await
            }
            None => self.all(ts).await,
        }
    }

    async fn one(&self, ts: Toolset, tool: &dyn Backend) -> Result<()> {
        if let Some(plugin) = tool.plugin()
            && !plugin.is_installed()
        {
            warn!("Plugin {} is not installed", tool.id());
            return Ok(());
        }
        match ts
            .list_versions_by_plugin()
            .into_iter()
            .find(|(p, _)| p.id() == tool.id())
        {
            Some((_, tvl)) => {
                let versions = tvl.os_supported_versions().collect::<Vec<_>>();
                if versions.is_empty() {
                    warn!(
                        "Plugin {} does not have a version set",
                        style(tool.id()).blue().for_stderr()
                    );
                    return Ok(());
                }
                pakroprintln!(
                    "{}",
                    versions
                        .iter()
                        .map(|v| v.version.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            None => {
                warn!(
                    "Plugin {} does not have a version set",
                    style(tool.id()).blue().for_stderr()
                );
            }
        };
        Ok(())
    }

    async fn all(&self, ts: Toolset) -> Result<()> {
        let config = Config::get().await?;
        for (plugin, tvl) in ts.list_versions_by_plugin() {
            let versions = tvl.os_supported_versions().collect::<Vec<_>>();
            if versions.is_empty() {
                continue;
            }
            for tv in &versions {
                if !plugin.is_version_installed(&config, tv, true) {
                    let source = ts.versions.get(tv.ba()).unwrap().source.clone();
                    warn!(
                        "{}@{} is specified in {}, but not installed",
                        &tv.ba(),
                        &tv.version,
                        &source
                    );
                    hint!(
                        "tools_missing",
                        "install missing tools with",
                        "pakro install"
                    );
                }
            }
            pakroprintln!(
                "{} {}",
                &plugin.id(),
                versions
                    .iter()
                    .map(|v| v.version.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        Ok(())
    }
}
