//! Conservative rejection of legacy mutation paths against managed shared
//! language objects (F2). The shared library's published payloads may only
//! change through the controlled install path until F4 opens its permitted
//! removal entry; legacy force/repair/postinstall/uninstall/purge/rollback
//! and delayed-cleanup hooks must stop before their first effect.

use std::path::{Component, Path, PathBuf};

use eyre::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyMutation {
    Force,
    Repair,
    PostInstall,
    Uninstall,
    Purge,
    DelayedCleanup,
    SystemPublish,
}

/// Rejects a legacy mutation before any effect when its target belongs to
/// the managed shared language library; targets outside the shared library
/// keep their existing behavior. Shared instances only change through the
/// controlled install path until F4 opens its permitted removal entry.
pub(crate) fn reject_legacy_mutation(
    data_root: &Path,
    mutation: LegacyMutation,
    target: &Path,
) -> Result<()> {
    let root = normalized_absolute(&data_root.join("languages"))?;
    let target = normalized_absolute(target)?;
    // A legacy creator must not gain access merely because its destination
    // does not exist yet. Resolve an existing ancestor to also cover aliases.
    if !target.starts_with(&root)
        && !resolve_ancestor(&target)?.starts_with(resolve_ancestor(&root)?)
    {
        return Ok(());
    }
    eyre::bail!(
        "a legacy {mutation:?} operation on the managed shared language library \
         is refused before any effect; shared instances only change through the \
         controlled install path"
    )
}

fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    eyre::ensure!(
        path.is_absolute(),
        "legacy mutation requires an absolute target"
    );
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                eyre::bail!("legacy mutation target cannot contain parent components")
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn resolve_ancestor(path: &Path) -> Result<PathBuf> {
    let mut parent = path;
    let mut missing = Vec::new();
    loop {
        match parent.canonicalize() {
            Ok(mut bound) => {
                for name in missing.iter().rev() {
                    bound.push(name);
                }
                return Ok(bound);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A broken link is an unknown binding, not a missing target.
                if std::fs::symlink_metadata(parent).is_ok() {
                    return Err(eyre::eyre!("legacy mutation target has an unresolved link"));
                }
                missing.push(
                    parent
                        .file_name()
                        .ok_or_else(|| eyre::eyre!("no target ancestor"))?,
                );
                parent = parent
                    .parent()
                    .ok_or_else(|| eyre::eyre!("no target ancestor"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests;
