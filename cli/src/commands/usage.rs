use crate::cli::Cli;
use eyre::Result;

/// Generate a usage CLI spec
///
/// See https://usage.jdx.dev for more information on this specification.
#[derive(Debug, usage_rs::Args)]
#[usage(verbatim_doc_comment, hide = true)]
pub(crate) struct Usage {}

pub(super) fn spec() -> usage::Spec {
    let mut spec: usage::Spec = Cli::to_kdl().parse().expect("generated pakro usage spec");
    crate::cli::command_effects::apply(&mut spec);
    spec.restamp();
    spec
}
pub(super) fn completion_spec() -> usage::Spec {
    spec()
}

impl Usage {
    pub(crate) fn run(self) -> Result<()> {
        // 3.6 added `effect=` (jdx/usage#739), 4.0 added it on flags and args
        // (jdx/usage#742), and 6.6 added flags scoped to implicit clauses
        // (jdx/usage#1343). 6.8 added mount synopsis metadata (jdx/usage#1393).
        // 6.11 adds the `logo` node this spec now carries (jdx/usage#1455).
        // Older `usage` CLIs reject the spec outright, so this
        // moves in lockstep with the fields and layouts the spec actually carries.
        let min_version = r#"min_usage_version "6.11""#;
        pakroprintln!("{min_version}\n{}", completion_spec().to_string().trim());
        Ok(())
    }
}
