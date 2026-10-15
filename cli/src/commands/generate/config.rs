use std::path::{Path, PathBuf};

use indoc::formatdoc;

use crate::Result;
use crate::config::{config_file, global_config_path};
use crate::file::display_path;
use crate::{env, file};

/// Generate a pakro.toml file
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(
        r###"pakro generate config             # generate a default pakro.toml template
pakro generate config .pakro.toml  # generate a specific file
pakro generate config -g          # generate the global config file
pakro generate config -n          # preview without writing"###
    )
)]
pub(super) struct Config {
    /// Generate the global config file (~/.config/pakro/config.toml)
    #[usage(long, short = 'g', conflicts = "path")]
    global: bool,
    /// Show what would be generated without writing to file
    #[usage(long, short = 'n')]
    dry_run: bool,
    /// Path to the config file to create
    #[usage(verbatim_doc_comment, value_hint = ValueHint::FilePath)]
    path: Option<PathBuf>,
    /// Path to a .tool-versions file to import tools from
    #[usage(long, short, verbatim_doc_comment, value_hint = ValueHint::FilePath)]
    tool_versions: Option<PathBuf>,
}

impl Config {
    pub(super) async fn run(self) -> Result<()> {
        let path = if let Some(path) = self.path.clone() {
            path
        } else if self.global {
            global_config_path()
        } else {
            PathBuf::from(&*env::PAKRO_DEFAULT_CONFIG_FILENAME)
        };
        let doc = if let Some(tool_versions) = &self.tool_versions {
            self.tool_versions(tool_versions, &path).await?
        } else {
            self.default()
        };
        if self.dry_run {
            info!("would write to {}", display_path(&path));
            pakroprintln!("{doc}");
        } else {
            info!("writing to {}", display_path(&path));
            if let Some(parent) = path.parent() {
                file::create_dir_all(parent)?;
            }
            file::write(path, doc)?;
        }
        Ok(())
    }

    async fn tool_versions(&self, tool_versions: &Path, path: &Path) -> Result<String> {
        let to = config_file::parse_or_init(path).await?;
        let from = config_file::parse(tool_versions).await?;
        let tools = from.to_tool_request_set()?.tools;
        for (ba, tools) in tools {
            to.replace_versions(&ba, tools)?;
        }
        to.dump()
    }

    fn default(&self) -> String {
        formatdoc! {r#"
            # pakro config files are hierarchical. pakro will find all of the config files
            # in all parent directories and merge them together.
            # You might have a structure like:
            #
            # * ~/work/project/pakro.toml   # a config file for a specific work project
            # * ~/work/pakro.toml           # a config file for projects related to work
            # * ~/.config/pakro/config.toml # the global config file
            # * /etc/pakro/config.toml      # the system config file
            #
            # This setup allows you to define default versions and configuration across
            # all projects but override them for specific projects.

            # [env]
            # NODE_ENV = "development"
            # _.file = ".env"                # load vars from a dotenv file
            # _.path = "./node_modules/.bin" # add a directory to PATH

            # [tools]
            # node = "22"
            # python = "3.12"
            # go = "latest"
        "#}
    }
}
