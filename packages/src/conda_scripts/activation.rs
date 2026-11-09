//! Saved changes observed while preparing a Conda project layout.
use eyre::{ContextCompat, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationPlan {
    schema: u32,
    changes: BTreeMap<String, Option<Vec<Part>>>,
    conditional_copies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Part {
    Literal(String),
    Prefix,
}

fn valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && !matches!(
            key,
            "PATH"
                | "HOME"
                | "PWD"
                | "OLDPWD"
                | "SHELL"
                | "SHLVL"
                | "_"
                | "PREFIX"
                | "CONDA_PREFIX"
                | "TMP"
                | "TEMP"
                | "TMPDIR"
                | "PYTHONDONTWRITEBYTECODE"
        )
        && !key.starts_with("PAKRO_")
        && !key.starts_with("__PAKRO_")
        && !key.starts_with("LD_")
        && !key.starts_with("DYLD_")
}

/// Accept the small POSIX form whose only host-dependent branch is a guarded
/// copy. File tests are evaluated by the original script in the real stage.
pub(super) fn script_shape(bytes: &[u8]) -> Result<(BTreeMap<String, String>, BTreeSet<String>)> {
    let text = std::str::from_utf8(bytes)?;
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let mut copies = BTreeMap::new();
    let mut unsets = BTreeSet::new();
    let mut written = BTreeSet::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(source) = line
            .strip_prefix("if [ -n \"${")
            .and_then(|s| s.strip_suffix(":-}\" ]; then"))
        {
            ensure!(
                valid_key(source) && !written.contains(source) && i + 2 < lines.len(),
                "unsupported Conda conditional environment read"
            );
            let assignment = lines[i + 1]
                .strip_prefix("export ")
                .context("unsupported Conda conditional body")?;
            let (destination, value) = assignment
                .split_once('=')
                .context("invalid Conda conditional assignment")?;
            ensure!(
                valid_key(destination)
                    && (value == format!("${source}") || value == format!("\"${{{source}}}\"")),
                "unsupported Conda conditional environment copy"
            );
            let mut end = i + 2;
            if lines[end] == format!("unset {source}") {
                end += 1;
            }
            ensure!(
                end < lines.len() && lines[end] == "fi",
                "unsupported Conda conditional body"
            );
            ensure!(
                copies
                    .insert(destination.to_owned(), source.to_owned())
                    .is_none(),
                "duplicate Conda conditional output"
            );
            written.insert(destination.to_owned());
            i = end + 1;
            continue;
        }
        let condition = line
            .strip_prefix("if [ -d ")
            .or_else(|| line.strip_prefix("if [ -f "))
            .map(|tail| (tail, true))
            .or_else(|| {
                line.strip_prefix("elif [ -d ")
                    .or_else(|| line.strip_prefix("elif [ -f "))
                    .map(|tail| (tail, false))
            });
        if let Some((tail, opens)) = condition {
            let path = tail
                .strip_prefix("\"${CONDA_PREFIX}/")
                .and_then(|s| s.strip_suffix("\" ]; then"))
                .context("unsupported Conda activation file condition")?;
            ensure!(
                !path.is_empty()
                    && path
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
                    && Path::new(path)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_))),
                "unsupported Conda activation condition path"
            );
            if opens {
                depth += 1;
            } else {
                ensure!(depth > 0, "unmatched Conda elif");
            }
        } else if line == "else" {
            ensure!(depth > 0, "unmatched Conda else");
        } else if line == "fi" {
            ensure!(depth > 0, "unmatched Conda fi");
            depth -= 1;
        } else if let Some(rest) = line.strip_prefix("unset ") {
            ensure!(
                valid_key(rest) && !copies.contains_key(rest),
                "unsupported Conda unset"
            );
            unsets.insert(rest.to_owned());
            written.insert(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("export ") {
            let assignment = rest
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(rest);
            let (name, value) = assignment
                .split_once('=')
                .context("unsupported Conda export")?;
            ensure!(
                valid_key(name) && !copies.contains_key(name),
                "unsupported Conda export key"
            );
            written.insert(name.to_owned());
            let value = value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(value);
            let value = value
                .replace("${CONDA_PREFIX}", "")
                .replace("$CONDA_PREFIX", "");
            ensure!(
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte)),
                "unsupported Conda export expression"
            );
        } else {
            eyre::bail!("unsupported Conda activation shell statement: {line}");
        }
        i += 1;
    }
    ensure!(depth == 0, "unfinished Conda activation condition");
    Ok((copies, unsets))
}

pub(super) fn parse_snapshot(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= 1024 * 1024 && bytes.last() == Some(&0),
        "Conda activation environment snapshot is invalid or too large"
    );
    let mut env = BTreeMap::new();
    for item in bytes[..bytes.len() - 1].split(|byte| *byte == 0) {
        let text = std::str::from_utf8(item)?;
        let (name, value) = text
            .split_once('=')
            .context("invalid Conda activation environment entry")?;
        ensure!(
            !name.is_empty() && env.insert(name.to_owned(), value.to_owned()).is_none(),
            "duplicate Conda activation environment entry"
        );
    }
    Ok(env)
}

impl ActivationPlan {
    pub(super) fn from_observations(
        before: &BTreeMap<String, String>,
        after: &BTreeMap<String, String>,
        explicit_unsets: &BTreeSet<String>,
        conditional_copies: BTreeMap<String, String>,
        staging: &Path,
        workspace: &Path,
    ) -> Result<Self> {
        let stage = staging
            .to_str()
            .context("Conda staging prefix is not UTF-8")?;
        let workspace = workspace
            .to_str()
            .context("Conda staging workspace is not UTF-8")?;
        let names: BTreeSet<_> = before
            .keys()
            .chain(after.keys())
            .chain(explicit_unsets.iter())
            .collect();
        let mut changes = BTreeMap::new();
        for name in names {
            if conditional_copies.contains_key(name) {
                continue;
            }
            let old = before.get(name);
            let new = after.get(name);
            if old == new && !explicit_unsets.contains(name) {
                continue;
            }
            ensure!(
                valid_key(name),
                "Conda activation modifies unsupported environment key {name}"
            );
            let value = if let Some(value) = new {
                let mut parts = Vec::new();
                let mut remainder = value.as_str();
                while let Some((head, tail)) = remainder.split_once(stage) {
                    ensure!(
                        !head.contains(workspace),
                        "Conda activation captured a temporary workspace path"
                    );
                    if !head.is_empty() {
                        parts.push(Part::Literal(head.to_owned()));
                    }
                    parts.push(Part::Prefix);
                    remainder = tail;
                }
                ensure!(
                    !remainder.contains(workspace),
                    "Conda activation captured a temporary workspace path"
                );
                if !remainder.is_empty() {
                    parts.push(Part::Literal(remainder.to_owned()));
                }
                Some(parts)
            } else {
                None
            };
            changes.insert(name.clone(), value);
        }
        let plan = Self {
            schema: 1,
            changes,
            conditional_copies,
        };
        plan.validate()?;
        Ok(plan)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && self.changes.len() <= 4096 && self.conditional_copies.len() <= 4096,
            "invalid Conda activation plan"
        );
        ensure!(
            self.conditional_copies
                .iter()
                .all(|(destination, source)| valid_key(destination)
                    && valid_key(source)
                    && !self.changes.contains_key(destination)),
            "invalid Conda conditional environment copy"
        );
        for (name, parts) in &self.changes {
            ensure!(
                valid_key(name),
                "unsupported Conda activation environment key"
            );
            if let Some(parts) = parts {
                ensure!(
                    parts.len() <= 4096
                        && parts.iter().all(|part| match part {
                            Part::Literal(value) => value.len() <= 65536 && !value.contains('\0'),
                            Part::Prefix => true,
                        }),
                    "invalid Conda activation value"
                );
            }
        }
        Ok(())
    }
    pub(crate) fn bind(
        &self,
        prefix: &Path,
        original: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, Option<String>>> {
        self.validate()?;
        let prefix = prefix
            .to_str()
            .context("Conda layout prefix is not UTF-8")?;
        let mut output: BTreeMap<_, _> = self
            .changes
            .iter()
            .map(|(name, parts)| {
                (
                    name.clone(),
                    parts.as_ref().map(|parts| {
                        let mut value = String::new();
                        for part in parts {
                            match part {
                                Part::Literal(text) => value.push_str(text),
                                Part::Prefix => value.push_str(prefix),
                            }
                        }
                        value
                    }),
                )
            })
            .collect();
        for (destination, source) in &self.conditional_copies {
            if let Some(value) = original.get(source).filter(|value| !value.is_empty()) {
                output.insert(destination.clone(), Some(value.clone()));
            }
        }
        Ok(output)
    }
}
