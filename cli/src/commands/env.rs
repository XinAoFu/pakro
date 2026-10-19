use eyre::Result;
use std::{collections::BTreeMap, sync::Arc};

use crate::args::ToolArg;
use crate::config::Config;
use crate::env::{PATH_KEY, join_paths, split_paths};
use crate::env_diff::EnvMap;
use crate::shell::{ShellType, get_shell};
use crate::toolset::{InstallOptions, Toolset, ToolsetBuilder};
use crate::wildcard::wildcard_match;
use indexmap::IndexSet;

/// Print the environment for the current configuration
///
/// Evaluate the shell output to load tools and variables once, without installing
/// prompt hooks. Changing directories afterward does not recompute this environment.
/// Use `pakro exec -- command` to apply it to one child process instead.
///
/// JSON, dotenv, and shell output contain actual variable values, including secrets.
/// `--redacted` selects variables marked for redaction; it does not mask their values.
/// Environment construction may install missing tools according to pakro's settings.
#[derive(Debug, usage_rs::Args)]
#[usage(
    visible_alias = "e",
    verbatim_doc_comment,
    example(
        r###"eval "$(pakro env -s bash)"
eval "$(pakro env -s zsh)"
pakro env -s fish | source
execx($(pakro env -s xonsh))"###
    )
)]
pub(crate) struct Env {
    /// Tool(s) to include in addition to those in config, e.g. node@20
    #[usage(value_name = "TOOL@VERSION")]
    tool: Vec<ToolArg>,

    /// Output in dotenv format
    #[usage(long, short = 'D', overrides = "shell")]
    dotenv: bool,

    /// Output in JSON format
    #[usage(long, short = 'J', overrides = "shell")]
    json: bool,

    /// Shell type to generate environment variables for
    #[usage(long, short, overrides = "json", value_enum)]
    shell: Option<ShellType>,

    /// Output in JSON format with additional information (source, tool)
    #[usage(long, overrides = "shell")]
    json_extended: bool,

    /// Only include variables marked for redaction; their values are printed unmasked
    #[usage(long)]
    redacted: bool,

    /// Only show values of environment variables
    #[usage(long)]
    values: bool,
}

impl Env {
    pub(crate) async fn run(self) -> Result<()> {
        let mut config = Config::get().await?;
        let mut ts = ToolsetBuilder::new()
            .with_args(&self.tool)
            .build(&config)
            .await?;
        // Shared library python (D07/D08): the declared exact version comes
        // from the shared library when published; this command is read-only
        // and python never auto-installs through the legacy pipeline here.
        #[cfg(unix)]
        let shared_python = crate::programs::directory::select_directory_python(false)?;
        #[cfg(unix)]
        if shared_python.is_some() {
            ts.versions.retain(|backend, _| backend.short != "python");
        }
        let shared_bin: Option<&std::path::Path> = {
            #[cfg(unix)]
            {
                shared_python
                    .as_ref()
                    .map(|shared| shared.bin_dir.as_path())
            }
            #[cfg(not(unix))]
            {
                None
            }
        };
        let (_, missing) = ts
            .install_missing_versions(&mut config, &InstallOptions::default())
            .await?;
        // The printed environment places the shim farms on PATH for lazy
        // declarations, so evaluating it must leave working bootstrap shims
        // behind: an eval'd shell has no command-not-found handler to fall
        // back on (discussion #12678).
        if ts.has_lazy_declarations()
            && let Err(err) = crate::shims::ensure_lazy_shims(&missing)
        {
            warn!("failed to create shims for lazy tools: {err:#}");
        }
        ts.notify_missing_versions(missing);

        // Pre-compute final_env when needed by --redacted or --dotenv to
        // avoid calling it twice. final_env includes the tools-only pass
        // whose redactions are not in config.env_results().
        let final_env = if self.redacted || self.dotenv {
            let (mut env, results) = ts.final_env(&config).await?;
            Self::prepend_shared_path(&mut env, shared_bin)?;
            Some((env, results))
        } else {
            None
        };

        let redacted_keys = if self.redacted {
            let env_results = config.env_results().await?;
            let mut keys = IndexSet::new();
            keys.extend(env_results.redactions.clone());
            if let Some((_, ref tools_env_results)) = final_env {
                keys.extend(tools_env_results.redactions.clone());
            }
            keys.extend(config.redaction_keys());
            Some(keys)
        } else {
            None
        };

        if self.json {
            self.output_json(&config, ts, &redacted_keys, shared_bin)
                .await
        } else if self.json_extended {
            self.output_extended_json(&config, ts, &redacted_keys, shared_bin)
                .await
        } else if self.dotenv {
            self.output_dotenv(final_env.unwrap().0, &redacted_keys, shared_bin)
        } else if self.values {
            self.output_values(&config, ts, &redacted_keys, shared_bin)
                .await
        } else {
            self.output_shell(&config, ts, &redacted_keys, shared_bin)
                .await
        }
    }

    async fn output_json(
        &self,
        config: &Arc<Config>,
        ts: Toolset,
        redacted_keys: &Option<IndexSet<String>>,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        let mut env = ts.env_with_path(config).await?;
        Self::prepend_shared_path(&mut env, shared_bin)?;

        if let Some(keys) = redacted_keys {
            env.retain(|k, _| self.should_include_key(k, keys));
        }

        pakroprintln!("{}", serde_json::to_string_pretty(&env)?);
        Ok(())
    }

    async fn output_extended_json(
        &self,
        config: &Arc<Config>,
        ts: Toolset,
        redacted_keys: &Option<IndexSet<String>>,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        let mut res = BTreeMap::new();

        let mut env = ts.env_with_path(config).await?;
        Self::prepend_shared_path(&mut env, shared_bin)?;
        env.iter().for_each(|(k, v)| {
            res.insert(k.to_string(), BTreeMap::from([("value", v.to_string())]));
        });

        config.env_with_sources().await?.iter().for_each(|(k, v)| {
            res.insert(
                k.to_string(),
                BTreeMap::from([
                    ("value", v.0.to_string()),
                    ("source", v.1.to_string_lossy().into_owned()),
                ]),
            );
        });

        let tool_map: BTreeMap<String, String> = ts
            .list_all_versions(config)
            .await?
            .into_iter()
            .map(|(b, tv)| {
                (
                    b.id().into(),
                    tv.request
                        .source()
                        .path()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "".to_string()),
                )
            })
            .collect();

        ts.env_from_tools(config)
            .await
            .iter()
            .for_each(|(name, value, tool_id)| {
                res.insert(
                    name.to_string(),
                    BTreeMap::from([
                        ("value", value.to_string()),
                        ("tool", tool_id.to_string()),
                        (
                            "source",
                            tool_map
                                .get(tool_id)
                                .cloned()
                                .unwrap_or_else(|| "unknown_source".to_string()),
                        ),
                    ]),
                );
            });

        if let Some(keys) = redacted_keys {
            res.retain(|k, _| self.should_include_key(k, keys));
        }

        pakroprintln!("{}", serde_json::to_string_pretty(&res)?);
        Ok(())
    }

    async fn output_shell(
        &self,
        config: &Arc<Config>,
        ts: Toolset,
        redacted_keys: &Option<IndexSet<String>>,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        let default_shell = get_shell(Some(fallback_shell())).unwrap();
        let shell = get_shell(self.shell).unwrap_or(default_shell);
        let (mut env, mut env_remove) = ts.env_with_path_and_removals(config).await?;
        Self::prepend_shared_path(&mut env, shared_bin)?;

        if let Some(keys) = redacted_keys {
            env.retain(|k, _| self.should_include_key(k, keys));
            env_remove.retain(|k| self.should_include_key(k, keys));
        }

        for k in env_remove {
            pakroprint!("{}", shell.unset_env(&k))?;
        }
        for (k, v) in env {
            let k = k.to_string();
            let v = v.to_string();
            pakroprint!("{}", shell.set_env(&k, &v))?;
        }
        Ok(())
    }

    fn output_dotenv(
        &self,
        mut env: EnvMap,
        redacted_keys: &Option<IndexSet<String>>,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        Self::prepend_shared_path(&mut env, shared_bin)?;
        if let Some(keys) = redacted_keys {
            env.retain(|k, _| self.should_include_key(k, keys));
        }

        for (k, v) in env {
            let k = k.to_string();
            let v = v.to_string();
            pakroprint!("{}={}\n", k, v)?;
        }
        Ok(())
    }

    async fn output_values(
        &self,
        config: &Arc<Config>,
        ts: Toolset,
        redacted_keys: &Option<IndexSet<String>>,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        let mut env = ts.env_with_path(config).await?;
        Self::prepend_shared_path(&mut env, shared_bin)?;

        if let Some(keys) = redacted_keys {
            env.retain(|k, _| self.should_include_key(k, keys));
        }

        for (_, v) in env {
            pakroprintln!("{}", v);
        }
        Ok(())
    }

    fn should_include_key(&self, key: &str, redacted_keys: &IndexSet<String>) -> bool {
        redacted_keys
            .iter()
            .any(|pattern| wildcard_match(key, pattern))
    }

    /// Puts the shared library's python bin directory in front of PATH so the
    /// selected shared interpreter takes precedence over any legacy install.
    fn prepend_shared_path(
        env: &mut EnvMap,
        shared_bin: Option<&std::path::Path>,
    ) -> Result<()> {
        let Some(bin) = shared_bin else { return Ok(()) };
        match env.get_mut(&*PATH_KEY) {
            Some(current) => {
                let mut parts: Vec<std::path::PathBuf> = split_paths(current).collect();
                if parts.first().is_some_and(|first| first == bin) {
                    return Ok(());
                }
                parts.insert(0, bin.to_path_buf());
                *current = join_paths(&parts)?.to_string_lossy().into_owned();
            }
            None => {
                env.insert((*PATH_KEY).clone(), bin.to_string_lossy().into_owned());
            }
        }
        Ok(())
    }
}

/// The shell to emit for when there is nothing to detect from.
///
/// Detection is `PAKRO_SHELL`, then `SHELL`. On unix that always resolves — an unset `SHELL` falls
/// back to `sh` — so this is reached only on Windows, where PowerShell and cmd set neither and the
/// value pakro would otherwise read, `COMSPEC`, names cmd.exe. pakro has no cmd implementation, so
/// bash was being printed into a PowerShell session:
///
/// ```console
/// PS> pakro env
/// export MY_PATH='C:\Users\me'
/// ```
///
/// pwsh is the only shell pakro can emit for that a Windows user is likely to be in. It is not
/// right for a cmd user, but neither was bash, and there is no third answer available.
fn fallback_shell() -> ShellType {
    match cfg!(windows) {
        true => ShellType::Pwsh,
        false => ShellType::Bash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserted on the output rather than on the enum, which would only restate the `cfg!` above.
    /// What matters is that the syntax is one the platform's shell can run.
    #[test]
    fn the_fallback_emits_syntax_this_platform_can_run() {
        let line = fallback_shell().as_shell().set_env("K", "V");

        if cfg!(windows) {
            // `export K=V` is what a PowerShell session used to be handed.
            assert!(!line.starts_with("export "), "{line}");
            assert!(line.contains("${Env:K}"), "{line}");
        } else {
            assert!(line.starts_with("export "), "{line}");
        }
    }
}
