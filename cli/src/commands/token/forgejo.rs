use crate::forgejo;
use crate::tokens;

/// Display the Forgejo token pakro will use for a given host
///
/// Shows which token source pakro would use, useful for debugging
/// authentication issues. The token is masked by default.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(
        r###"pakro token forgejo
codeberg.org: a180…61f6 (source: FORGEJO_TOKEN)"###
    ),
    example(
        r###"pakro token forgejo --unmask
codeberg.org: a18099ca69064be387fbe37b8ad1d333758361f6 (source: FORGEJO_TOKEN)"###
    ),
    example(
        r###"pakro token forgejo forgejo.mycompany.com
forgejo.mycompany.com: (none)"###
    )
)]
pub(super) struct Forgejo {
    /// Forgejo hostname
    #[usage(default = "codeberg.org")]
    host: String,

    /// Show the full unmasked token
    #[usage(long)]
    unmask: bool,
}

impl Forgejo {
    pub(super) fn run(self) -> eyre::Result<()> {
        match forgejo::resolve_token(&self.host) {
            Some((token, source)) => {
                let display_token = if self.unmask {
                    token
                } else {
                    tokens::mask_token(&token)
                };
                pakroprintln!("{}: {} (source: {})", self.host, display_token, source);
            }
            None => {
                pakroprintln!("{}: (none)", self.host);
            }
        }
        Ok(())
    }
}
