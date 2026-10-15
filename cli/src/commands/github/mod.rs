mod token;

/// GitHub related commands
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment, hide = true)]
pub(crate) struct Github {
    #[usage(subcommand)]
    subcommand: Commands,
}

#[derive(Debug, usage_rs::Subcommands)]
enum Commands {
    Token(token::Token),
}

impl Github {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        deprecated_at!(
            "2026.5.1",
            "2027.5.0",
            "cli.github",
            "`pakro github ...` is deprecated. Use `pakro token github` instead."
        );
        match self.subcommand {
            Commands::Token(cmd) => cmd.run(),
        }
    }
}
