use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};
use console::style;
use path_absolutize::Absolutize;

use crate::backend::unalias_backend;
use crate::file::{make_symlink, remove_all};
use crate::plugins::PluginType;
use crate::{dirs, file};

/// Link a local plugin directory into pakro for development
///
/// Edits in the source directory take effect without reinstalling the plugin. Pass
/// both a name and directory, or only a directory to infer the name after stripping
/// a known prefix such as `pakro-` or `asdf-`. This does not install a tool version.
#[derive(Debug, usage_rs::Args)]
#[usage(
    visible_alias = "ln",
    verbatim_doc_comment,
    example(r###"pakro plugins link my-tool ./pakro-my-tool"###),
    example(
        r###"pakro plugins link ./pakro-my-tool"###,
        help = r###"Alternative: infer the name "my-tool""###
    ),
    example(
        r###"pakro ls-remote my-tool"###,
        help = r###"List versions through the linked plugin"###
    )
)]
pub(super) struct PluginsLink {
    /// The name of the plugin
    /// With one argument, this is the plugin directory and the name is inferred
    #[usage(verbatim_doc_comment)]
    name: String,

    /// The local path to the plugin
    /// e.g.: ./pakro-my-tool
    #[usage(value_hint = ValueHint::DirPath, verbatim_doc_comment)]
    dir: Option<PathBuf>,

    /// Overwrite existing plugin
    #[usage(long, short = 'f')]
    force: bool,
}

impl PluginsLink {
    pub(super) async fn run(self) -> Result<()> {
        let (name, path) = match self.dir {
            Some(path) => (self.name, path),
            None => {
                let path = PathBuf::from(PathBuf::from(&self.name).absolutize()?);
                let name = get_name_from_path(&path);
                (name, path)
            }
        };
        let name = unalias_backend(&name);
        let name = name.as_ref();
        if name.starts_with("packslip:") {
            bail!("Packslip plugin sources have been removed; use an ASDF Git source");
        }
        let (_, name) = PluginType::from_plugin_config(name)?;
        let path = path.absolutize()?;
        if path.join("metadata.lua").exists() {
            bail!(
                "Vfox and Lua plugin directories have been removed: {}",
                path.display()
            );
        }
        let symlink = dirs::PLUGINS.join(name);
        if symlink.exists() {
            if self.force {
                remove_all(&symlink)?;
            } else {
                return Err(eyre!(
                    "plugin {} already exists, use --force to overwrite",
                    style(&name).blue().for_stderr()
                ));
            }
        }
        file::create_dir_all(*dirs::PLUGINS)?;
        make_symlink(&path, &symlink)?;

        Ok(())
    }
}

fn get_name_from_path(path: &Path) -> String {
    let name = path.file_name().unwrap().to_str().unwrap();
    let name = name.strip_prefix("asdf-").unwrap_or(name);
    // ASDF plugin repositories retain their actual upstream prefix.
    let name = name
        .strip_prefix("pakro-")
        .or_else(|| name.strip_prefix("mise-"))
        .unwrap_or(name);
    unalias_backend(name).to_string()
}
