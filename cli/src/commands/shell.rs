use color_eyre::eyre::{Result, bail, eyre};
use console::style;
use indoc::formatdoc;

use crate::args::ToolArg;
use crate::config::Config;
use crate::env;
use crate::shell::{EXAMPLE_SHELL, require_shell};
use crate::toolset::{InstallOptions, ToolSource, ToolsetBuilder, tool_env_var_name};

/// Set a tool version for the current shell session
///
/// Only works in a session where pakro is already activated.
///
/// This works by setting environment variables for the current shell session
/// such as `PAKRO_NODE_VERSION=20` which is "eval"ed as a shell function created by `pakro activate`.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    visible_alias = "sh",
    example(
        r###"pakro shell node@20
node -v
v20.0.0"###
    )
)]
pub(crate) struct Shell {
    /// Tool(s) to use
    #[usage(value_name = "TOOL@VERSION", required = true)]
    tool: Vec<ToolArg>,

    /// Number of jobs to run in parallel
    /// Values below 1 are treated as 1
    /// Defaults to the `jobs` setting
    #[usage(long, short, env = "PAKRO_JOBS", verbatim_doc_comment)]
    jobs: Option<usize>,

    /// Remove a previously set version
    #[usage(long, short)]
    unset: bool,

    /// Connect backend install command stdin/stdout/stderr directly to the terminal.
    /// Implies `--jobs=1`
    #[usage(long, overrides = "jobs")]
    raw: bool,
}

impl Shell {
    pub(crate) async fn run(self) -> Result<()> {
        let mut config = Config::get().await?;
        if !env::is_activated() {
            err_inactive()?;
        }

        let shell = require_shell(
            None,
            &format!("Re-run `pakro activate {EXAMPLE_SHELL}` in your shell rc file."),
        )?;

        if self.unset {
            for ta in &self.tool {
                let op = shell.unset_env(&tool_env_var_name(&ta.ba.short));
                pakroprint!("{op}")?;
            }
            return Ok(());
        }

        for ta in &self.tool {
            if ta.tvr.is_none() {
                bail!(
                    "no version specified for tool {tool}\nuse `pakro shell {tool}@VERSION` to set a version",
                    tool = ta.ba.short,
                );
            }
        }

        let mut ts = ToolsetBuilder::new()
            .with_args(&self.tool)
            .build(&config)
            .await?;
        let opts = InstallOptions {
            force: false,
            jobs: self.jobs,
            raw: self.raw,
            ..Default::default()
        };
        let (_, missing) = ts.install_missing_versions(&mut config, &opts).await?;
        ts.notify_missing_versions(missing);

        for (p, tv) in ts.list_current_installed_versions(&config) {
            let source = &ts.versions.get(p.ba().as_ref()).unwrap().source;
            if matches!(source, ToolSource::Argument) {
                let k = tool_env_var_name(p.id());
                let op = shell.set_env(&k, &tv.version);
                pakroprint!("{op}")?;
            }
        }

        Ok(())
    }
}

fn err_inactive() -> Result<()> {
    Err(eyre!(formatdoc!(
        r#"
                pakro is not activated in this shell session.
                Please run `{}` first in your shell rc file.
                "#,
        style("pakro activate").yellow()
    )))
}
