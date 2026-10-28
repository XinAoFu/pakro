//! Effects of the currently exposed commands. Local queries do not install;
//! hook-env may commit a project selection of existing content.
use std::collections::HashMap;
use usage::SpecCommandEffect::{self, Destructive, Read, Write};
pub(super) const EFFECTS: &[(&str, SpecCommandEffect)] = &[
    ("bin-paths", Read),
    ("cache", Read),
    ("cache path", Read),
    ("completion", Read),
    ("config", Read),
    ("config get", Read),
    ("config ls", Read),
    ("current", Read),
    ("deactivate", Read),
    ("doctor", Read),
    ("doctor path", Read),
    ("env", Read),
    ("hook-not-found", Read),
    ("installs", Read),
    ("installs ls", Read),
    ("latest", Read),
    ("ls", Read),
    ("ls-remote", Read),
    ("project", Read),
    ("project ls", Read),
    ("project status", Read),
    ("project forget", Write),
    ("registry", Read),
    ("search", Read),
    ("settings", Read),
    ("settings get", Read),
    ("settings ls", Read),
    ("usage", Read),
    ("version", Read),
    ("where", Read),
    ("which", Read),
    ("activate", Write),
    ("cache clear", Write),
    ("cache prune", Write),
    ("deps", Write),
    ("deps install", Write),
    ("hook-env", Write),
    ("install", Write),
    ("reshim", Write),
    ("self-update", Write),
    ("trust", Write),
    ("untrust", Write),
    ("implode", Destructive),
    ("uninstall", Destructive),
    ("prune", Destructive),
];
pub(super) fn apply(spec: &mut usage::Spec) {
    let effects: HashMap<_, _> = EFFECTS.iter().copied().collect();
    annotate(&mut spec.cmd, &mut Vec::new(), &effects);
}
fn annotate(
    command: &mut usage::SpecCommand,
    path: &mut Vec<String>,
    effects: &HashMap<&str, SpecCommandEffect>,
) {
    for (name, child) in &mut command.subcommands {
        path.push(name.clone());
        child.effect = effects.get(path.join(" ").as_str()).copied();
        annotate(child, path, effects);
        path.pop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_commands_have_exact_effect_coverage() {
        fn collect(
            command: &usage::SpecCommand,
            path: &mut Vec<String>,
            names: &mut std::collections::BTreeSet<String>,
        ) {
            for (name, child) in &command.subcommands {
                path.push(name.clone());
                names.insert(path.join(" "));
                collect(child, path, names);
                path.pop();
            }
        }
        let spec: usage::Spec = crate::cli::Cli::to_kdl().parse().unwrap();
        let mut actual = std::collections::BTreeSet::new();
        collect(&spec.cmd, &mut Vec::new(), &mut actual);
        assert_eq!(
            actual,
            EFFECTS.iter().map(|(name, _)| name.to_string()).collect()
        );
    }
}
