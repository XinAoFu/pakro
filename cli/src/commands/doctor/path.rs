use crate::Result;
use crate::config::Config;
use std::env;

/// Print the current PATH entries pakro is providing
#[derive(Debug, usage_rs::Args)]
#[usage(
    alias = "paths",
    verbatim_doc_comment,
    example(
        "pakro doctor path",
        help = "Get the PATH entries pakro provides, such as `/home/user/.local/share/pakro/installs/node/24.0.0/bin`, `/home/user/.local/share/pakro/installs/rust/1.90.0/bin`, and `/home/user/.local/share/pakro/installs/python/3.10.0/bin`."
    )
)]
pub(crate) struct Path {
    /// Print all entries including those not provided by pakro
    #[usage(long, short, verbatim_doc_comment)]
    full: bool,
}

impl Path {
    pub(crate) async fn run(self) -> Result<()> {
        let config = Config::get().await?;
        let ts = config.get_toolset().await?;
        let paths = if self.full {
            let env = ts.env_with_path(&config).await?;
            let path = env.get("PATH").cloned().unwrap_or_default();
            env::split_paths(&path).collect()
        } else {
            let (_env, env_results) = ts.final_env(&config).await?;
            ts.list_final_paths(&config, env_results).await?
        };
        for path in paths {
            pakroprintln!("{}", path.display());
        }
        Ok(())
    }
}
