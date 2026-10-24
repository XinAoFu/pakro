use std::sync::Arc;

use crate::args::ToolArg;
use crate::config::Config;
use crate::dirs;
use crate::file;
use crate::toolset::{Toolset, ToolsetBuilder};
use eyre::{Result, bail};
use itertools::Itertools;

/// Show the path a tool's executable resolves to
///
/// Use this to figure out what version of a tool is currently active.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(
        r###"pakro which node
/home/username/.local/share/pakro/installs/node/20.0.0/bin/node"###
    ),
    example(
        r###"pakro which node --plugin
node"###
    ),
    example(
        r###"pakro which node --version
20.0.0"###
    )
)]
pub(crate) struct Which {
    /// The executable to look up
    #[usage(required_unless = "complete")]
    pub bin_name: Option<String>,

    /// Use a specific tool@version
    /// e.g.: `pakro which npm --tool=node@20`
    #[usage(short, long, value_name = "TOOL@VERSION", verbatim_doc_comment)]
    pub tool: Option<ToolArg>,

    #[usage(long, hide = true)]
    pub complete: bool,

    /// Show the plugin name instead of the path
    #[usage(long, conflicts = "version")]
    pub plugin: bool,

    /// Show the version instead of the path
    #[usage(long, conflicts = "plugin")]
    pub version: bool,
}

impl Which {
    pub(super) fn explicitly_non_python(&self) -> bool {
        self.tool
            .as_ref()
            .is_some_and(|tool| !crate::programs::python::is_python_backend(&tool.ba))
    }

    pub(super) async fn requires_shared_python(&self) -> Result<bool> {
        if let Some(tool) = &self.tool {
            return Ok(crate::programs::python::is_python_backend(&tool.ba));
        }
        let config = Config::get().await?;
        let effective = ToolsetBuilder::new().build_unresolved(&config)?;
        Ok(effective.versions.is_empty()
            || effective
                .versions
                .keys()
                .any(|backend| crate::programs::python::is_python_backend(backend))
            || self.bin_name.as_deref().is_some_and(|command| {
                python_command(command)
                    && !crate::shims::configured_non_python_provider(&effective, command)
            }))
    }

    pub(crate) async fn run(self) -> Result<()> {
        if let Some(tool) = &self.tool {
            crate::programs::python::validate_python_query_options(&tool.ba)?;
        }
        let config = Config::get().await?;
        if self.complete {
            return self.complete(&config).await;
        }
        let include_shared_python = self.requires_shared_python().await?;
        let ts = self.get_toolset(&config).await?;
        let bin_name = self.bin_name.clone().unwrap();
        let python_owned = self
            .tool
            .as_ref()
            .is_some_and(|tool| crate::programs::python::is_python_backend(&tool.ba))
            || (self.tool.is_none()
                && python_command(&bin_name)
                && !crate::shims::configured_non_python_provider(&ts, &bin_name));

        // env and the directory hook prepend the verified shared bin paths.
        // Resolve that candidate once before returning a legacy tool's same-name
        // executable. Retain a failed Python query until local ownership is
        // known, so an unrelated missing Python does not hide a real companion.
        #[cfg(unix)]
        let shared = if include_shared_python {
            self.shared_python(&ts, Some(&bin_name))
        } else {
            Ok(None)
        };
        #[cfg(unix)]
        if let Ok(Some(selected)) = &shared {
            if let Some(path) = selected.command(&bin_name)? {
                if self.version {
                    pakroprintln!("{}", selected.version);
                } else if self.plugin {
                    pakroprintln!("python");
                } else {
                    pakroprintln!("{}", path.display());
                }
                return Ok(());
            }
        }
        #[cfg(unix)]
        if python_owned {
            let selected = shared?.ok_or_else(|| {
                eyre::eyre!("no current shared Python declaration; name an installed target with --tool=python@VERSION")
            })?;
            bail!(
                "shared Python {} does not provide command {bin_name:?}",
                selected.version
            );
        }
        let remaining = ts.which_for_query(&config, &bin_name).await?;
        #[cfg(unix)]
        if remaining.is_none() {
            // Missing, invalid and damaged Python must not turn into an empty
            // result or a legacy Python fallback when no other tool owns it.
            shared?;
        }
        match remaining {
            Some((p, tv, path)) => {
                if self.version {
                    pakroprintln!("{}", tv.version);
                } else if self.plugin {
                    pakroprintln!("{p}");
                } else {
                    pakroprintln!("{}", path.display());
                }
                Ok(())
            }
            None => {
                if let Some(msg) = self.uninstalled_tool_message(&config, &ts) {
                    bail!(msg);
                }
                if let Some(msg) =
                    crate::shims::unavailable_configured_tool_message(&config, &ts, &bin_name)
                {
                    bail!(msg);
                }
                if self.has_shim(&bin_name) {
                    bail!(
                        "{bin_name} is a pakro bin however it is not currently active. Use `pakro use` to activate it in this directory."
                    )
                } else {
                    bail!("{bin_name} is not a pakro bin. Perhaps you need to install it first.",)
                }
            }
        }
    }
    async fn complete(&self, config: &Arc<Config>) -> Result<()> {
        let ts = self.get_toolset(config).await?;
        let mut paths = ts.list_paths_for_query(config).await?;
        #[allow(unused_mut)]
        let mut verified_paths = std::collections::HashSet::<std::path::PathBuf>::new();
        #[cfg(unix)]
        if let Some(shared) = self.shared_python(&ts, None)? {
            verified_paths.extend(shared.bin_dirs.iter().cloned());
            paths.extend(shared.bin_dirs);
        }
        let mut bins = Vec::new();
        for path in paths {
            let entries = match file::ls(&path) {
                Ok(entries) => entries,
                Err(error) if verified_paths.contains(&path) => return Err(error),
                Err(_) => continue,
            };
            for entry in entries {
                if entry.is_file() && file::is_executable(&entry) {
                    bins.push(entry.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let bins = bins.into_iter().unique().sorted();
        for bin in bins {
            pakroprintln!("{bin}");
        }
        Ok(())
    }
    async fn get_toolset(&self, config: &Arc<Config>) -> Result<Toolset> {
        let mut tsb = ToolsetBuilder::new();
        if let Some(tool) = &self.tool {
            tsb = tsb
                .with_args(std::slice::from_ref(tool))
                .with_overridden_lockfile_warnings();
        }
        let mut ts = tsb.build_unresolved(config)?;
        #[cfg(unix)]
        ts.versions
            .retain(|backend, _| !crate::programs::python::is_python_backend(backend));
        if let Some(tool) = &self.tool {
            ts.versions.retain(|backend, _| **backend == *tool.ba);
        }
        ts.resolve_with_opts(
            config,
            &crate::toolset::ResolveOptions {
                offline: self
                    .tool
                    .as_ref()
                    .is_none_or(|tool| crate::programs::python::is_python_backend(&tool.ba)),
                ..Default::default()
            },
        )
        .await?;
        Ok(ts)
    }

    #[cfg(unix)]
    fn shared_python(
        &self,
        ts: &Toolset,
        command: Option<&str>,
    ) -> Result<Option<crate::programs::directory::DirectoryPython>> {
        use crate::programs::directory::{PythonQueryRequest, PythonQueryScope, query_python};
        if self
            .tool
            .as_ref()
            .is_some_and(|tool| !crate::programs::python::is_python_backend(&tool.ba))
        {
            return Ok(None);
        }
        let selected = query_python(PythonQueryRequest {
            target: self.tool.as_ref().and_then(|tool| tool.version.clone()),
            scope: PythonQueryScope::Environment,
        })?
        .into_ready()?;
        if selected.is_none()
            && (self
                .tool
                .as_ref()
                .is_some_and(|tool| crate::programs::python::is_python_backend(&tool.ba))
                || command.is_some_and(|name| {
                    python_command(name) && !crate::shims::configured_non_python_provider(ts, name)
                }))
        {
            bail!(
                "no current shared Python declaration; name an installed target with --tool=python@VERSION"
            );
        }
        Ok(selected)
    }
    /// `--tool` replaces whatever the config requested, so a version that isn't
    /// installed leaves nothing for [`Toolset::which`] to search. Saying the bin
    /// "is not currently active" would blame the config instead of the flag.
    fn uninstalled_tool_message(&self, config: &Arc<Config>, ts: &Toolset) -> Option<String> {
        let tool = self.tool.as_ref()?;
        let tv = ts
            .list_current_versions()
            .into_iter()
            .find(|(b, tv)| {
                tv.ba() == tool.ba.as_ref() && !b.is_version_installed(config, tv, true)
            })
            .map(|(_, tv)| tv)?;
        let requested = tool.version.clone().unwrap_or_else(|| tv.version.clone());
        let resolved = if requested == tv.version {
            String::new()
        } else {
            format!(" (resolved to {})", tv.version)
        };
        Some(format!(
            "{}@{requested} is not installed{resolved}\n\
             hint: run `pakro install {}@{requested}`, or `pakro ls {}` to see installed versions",
            tool.short, tool.short, tool.short
        ))
    }
    fn has_shim(&self, shim: &str) -> bool {
        dirs::shims().join(shim).exists() || dirs::system_shims().join(shim).exists()
    }
}

/// Python's version-qualified commands are still owned by the shared library
/// when no current declaration exists; they cannot fall back to an old install.
#[cfg(unix)]
fn python_command(command: &str) -> bool {
    ["python", "pip"].iter().any(|prefix| {
        command.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.is_empty()
                || (!suffix.starts_with('.')
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.'))
        })
    })
}
