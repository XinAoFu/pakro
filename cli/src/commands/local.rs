use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use color_eyre::eyre::{ContextCompat, Result, eyre};
use console::style;
use itertools::Itertools;

use crate::config::{Settings, config_file};
use crate::env::{PAKRO_DEFAULT_CONFIG_FILENAME, PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME};
use crate::file::display_path;
use crate::{
    cli::args::{BackendArg, ToolArg},
    config::Config,
};
use crate::{env, file};

/// Sets/gets tool version in local .tool-versions or pakro.toml
///
/// Use this to set a tool's version when within a directory
/// Use `pakro global` to set a tool version globally
/// This uses `.tool-version` by default unless there is a `pakro.toml` file or if `PAKRO_USE_TOML`
/// is set. A future v2 release of pakro will default to using `pakro.toml`.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    hide = true,
    alias = "l",
    example(
        r###"pakro local --pin node@20"###,
        help = r###"set the current version of node to 20.x for the current directory will use a precise version (e.g.: 20.0.0) in .tool-versions file"###
    ),
    example(
        r###"pakro local -p node@20"###,
        help = r###"set node to 20.x for the current project (recurses up to find .tool-versions)"###
    ),
    example(
        r###"pakro local --fuzzy node@20"###,
        help = r###"set the current version of node to 20.x for the current directory will use a fuzzy version (e.g.: 20) in .tool-versions file"###
    ),
    example(
        r###"pakro local --remove=node"###,
        help = r###"removes node from .tool-versions"###
    ),
    example(
        r###"pakro local node"###,
        help = r###"show the current version of node in .tool-versions; example output: `20.0.0`"###
    )
)]
pub(crate) struct Local {
    /// Tool(s) to add to .tool-versions/pakro.toml
    /// e.g.: node@20
    /// if this is a single tool with no version,
    /// the current value of .tool-versions/pakro.toml will be displayed
    #[usage(value_name = "TOOL@VERSION", verbatim_doc_comment)]
    tool: Vec<ToolArg>,

    /// Recurse up to find a .tool-versions file rather than using the current directory only
    /// by default this command will only set the tool in the current directory ("$PWD/.tool-versions")
    #[usage(short, long, verbatim_doc_comment)]
    parent: bool,

    /// Save fuzzy version to `.tool-versions`
    /// e.g.: `pakro local --fuzzy node@20` will save `node 20` to .tool-versions
    /// This is the default behavior unless PAKRO_ASDF_COMPAT=1
    #[usage(long, overrides = "pin")]
    fuzzy: bool,

    /// Get the path of the config file
    #[usage(long)]
    path: bool,

    /// Save exact version to `.tool-versions`
    /// e.g.: `pakro local --pin node@20` will save `node 20.0.0` to .tool-versions
    #[usage(long, verbatim_doc_comment, overrides = "fuzzy")]
    pin: bool,

    /// Remove the tool(s) from .tool-versions
    #[usage(long, value_name = "TOOL", aliases = ["rm", "unset"])]
    remove: Option<Vec<BackendArg>>,
}

impl Local {
    pub(crate) async fn run(self) -> Result<()> {
        let config = Config::get().await?;
        let path = if self.parent {
            get_parent_path()?
        } else {
            get_path()?
        };
        local(
            &config,
            &path,
            self.tool,
            self.remove,
            self.pin,
            self.fuzzy,
            self.path,
        )
        .await
    }
}

fn get_path() -> Result<PathBuf> {
    let cwd = env::current_dir()?;
    let pakro_toml = cwd.join(PAKRO_DEFAULT_CONFIG_FILENAME.as_str());
    let tool_versions = cwd.join(PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME.as_str());
    if pakro_toml.exists() {
        Ok(pakro_toml)
    } else if tool_versions.exists() {
        Ok(tool_versions)
    } else if *env::PAKRO_USE_TOML {
        Ok(pakro_toml)
    } else {
        Ok(tool_versions)
    }
}

pub(super) fn get_parent_path() -> Result<PathBuf> {
    let mut filenames = vec![PAKRO_DEFAULT_CONFIG_FILENAME.as_str()];
    if !*env::PAKRO_USE_TOML {
        filenames.push(PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME.as_str());
    }
    file::find_up(&env::current_dir()?, &filenames)
        .wrap_err_with(|| eyre!("no {} file found", filenames.join(" or "),))
}

pub(super) async fn local(
    config: &Arc<Config>,
    path: &Path,
    runtime: Vec<ToolArg>,
    remove: Option<Vec<BackendArg>>,
    pin: bool,
    fuzzy: bool,
    show_path: bool,
) -> Result<()> {
    deprecated!(
        "local",
        "pakro local/global are deprecated. Use `pakro use` instead."
    );
    let settings = Settings::try_get()?;
    let cf = config_file::parse_or_init(path).await?;
    if show_path {
        pakroprintln!("{}", path.display());
        return Ok(());
    }

    if let Some(plugins) = &remove {
        for plugin in plugins {
            cf.remove_tool(plugin)?;
        }
        let tools = plugins
            .iter()
            .map(|r| style(&r.short).blue().for_stderr().to_string())
            .join(" ");
        pakroprintln!("{} {} {tools}", style("pakro").dim(), display_path(path));
    }

    if !runtime.is_empty() {
        let runtimes = ToolArg::double_tool_condition(&runtime)?;
        if cf.display_runtime(&runtimes)? {
            return Ok(());
        }
        let pin = pin || (settings.asdf_compat && !fuzzy);
        cf.add_runtimes(config, &runtimes, pin).await?;
        let tools = runtimes.iter().map(|t| t.style()).join(" ");
        pakroprintln!("{} {} {tools}", style("pakro").dim(), display_path(path));
    }

    if !runtime.is_empty() || remove.is_some() {
        trace!("saving config file {}", display_path(path));
        cf.save()?;
    } else {
        pakroprint!("{}", cf.dump()?)?;
    }

    Ok(())
}
