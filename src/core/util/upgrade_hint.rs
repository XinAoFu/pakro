//! What to tell a user whose pakro is out of date: run `pakro self-update`, or
//! follow the instructions the packager shipped.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::env;

/// pakro has no published release identity yet. Neither a feature flag nor an
/// environment override can authorize replacing it with an upstream binary.
pub fn self_update_available() -> bool {
    false
}

#[derive(Debug, Default, serde::Deserialize)]
struct InstructionsToml {
    message: Option<String>,
    #[serde(flatten)]
    commands: BTreeMap<String, String>,
}

fn read_instructions_file(path: &PathBuf) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let parsed: InstructionsToml = toml::from_str(&body).ok()?;
    if let Some(msg) = parsed.message {
        return Some(msg);
    }
    if let Some((_k, v)) = parsed.commands.into_iter().next() {
        return Some(v);
    }
    None
}

pub fn upgrade_instructions_text() -> Option<String> {
    if let Some(path) = &*env::PAKRO_SELF_UPDATE_INSTRUCTIONS
        && let Some(msg) = read_instructions_file(path)
    {
        return Some(msg);
    }
    None
}

/// Shown when pakro cannot update itself and the packager shipped no instructions
/// file. Local builds and package-manager installs need to follow their own
/// update method until pakro publishes verified release artifacts.
pub(crate) const SELF_UPDATE_DISABLED_HINT: &str =
    "pakro release updates are not configured; update pakro the same way you installed it";

/// How to update pakro when `pakro self-update` is not available: the packager's
/// instructions when they shipped some, otherwise the generic hint.
pub fn upgrade_instructions_or_hint() -> String {
    upgrade_instructions_text().unwrap_or_else(|| SELF_UPDATE_DISABLED_HINT.to_string())
}

/// Appends self-update guidance and packaging instructions (if any) to a message.
pub(crate) fn append_self_update_instructions(mut message: String) -> String {
    if self_update_available() {
        message.push_str("\nRun `pakro self-update` to update pakro");
    }
    if let Some(instructions) = upgrade_instructions_text() {
        message.push('\n');
        message.push_str(&instructions);
    } else if !self_update_available() {
        message.push('\n');
        message.push_str(SELF_UPDATE_DISABLED_HINT);
    }
    message
}
