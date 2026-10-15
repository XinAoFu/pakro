mod forgejo;
pub(super) mod git_credential;
pub(crate) mod github;
mod gitlab;

/// Display git provider tokens pakro will use
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment)]
pub(crate) struct Token {
    #[usage(subcommand)]
    subcommand: Commands,
}

#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    /// Show the Forgejo token pakro will use
    Forgejo(forgejo::Forgejo),
    /// Show the GitHub token pakro will use
    Github(github::Github),
    /// Show the GitLab token pakro will use
    Gitlab(gitlab::Gitlab),
}

impl Token {
    pub(crate) fn run_git_credential(&self) -> Option<eyre::Result<()>> {
        match &self.subcommand {
            Commands::Github(cmd) => cmd.git_credential.as_deref().map(git_credential::run),
            _ => None,
        }
    }

    pub(crate) async fn run(self) -> eyre::Result<()> {
        match self.subcommand {
            Commands::Forgejo(cmd) => cmd.run(),
            Commands::Github(cmd) => cmd.run(),
            Commands::Gitlab(cmd) => cmd.run(),
        }
    }
}
