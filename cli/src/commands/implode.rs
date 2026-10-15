use std::path::Path;

use eyre::Result;

use crate::config::Settings;
use crate::file::remove_all;
use crate::ui::prompt;
use crate::{dirs, env, file};
use std::collections::BTreeSet;

/// Remove the pakro CLI and all related data
///
/// The config directory is kept unless `--config` is passed.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Implode {
    /// List directories that would be removed without actually removing them
    #[usage(long, short = 'n', verbatim_doc_comment)]
    dry_run: bool,

    /// Also remove config directory
    #[usage(long, verbatim_doc_comment)]
    config: bool,
}

impl Implode {
    pub(super) fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub(crate) fn run(self) -> Result<()> {
        let mut files: BTreeSet<&Path> =
            [*dirs::STATE, *dirs::DATA, *dirs::CACHE, &*env::PAKRO_BIN]
                .into_iter()
                .collect();
        if self.config {
            files.insert(&dirs::CONFIG);
        }
        // include system data dir (e.g. /usr/local/share/pakro) used by `pakro install --system`
        let system_data: &Path = &env::PAKRO_SYSTEM_DATA_DIR;
        files.insert(system_data);
        for f in files.into_iter().filter(|d| d.exists()) {
            if self.dry_run {
                pakroprintln!("rm -rf {}", f.display());
            }

            if self.confirm_remove(f)? {
                if f.is_dir() {
                    remove_all(f)?;
                } else {
                    file::remove_file(f)?;
                }
            }
        }

        Ok(())
    }

    fn confirm_remove(&self, f: &Path) -> Result<bool> {
        let settings = Settings::try_get()?;
        if self.dry_run {
            Ok(false)
        } else if settings.yes {
            Ok(true)
        } else {
            Ok(prompt::confirm(format!("remove {} ?", f.display()))?.is_yes())
        }
    }
}
