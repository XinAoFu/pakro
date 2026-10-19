//! Conservative rejection of legacy mutation paths against managed shared
//! language objects (F2). The shared library's published payloads may only
//! change through the controlled install path until F4 opens its permitted
//! removal entry; legacy force/repair/postinstall/uninstall/purge/rollback
//! and delayed-cleanup hooks must stop before their first effect.

use std::path::Path;

use eyre::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyMutation {
    Force,
    Repair,
    PostInstall,
    Uninstall,
    Purge,
    Rollback,
    DelayedCleanup,
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
    // Bind both sides to their real locations; an absent library or target
    // is simply not shared and keeps its existing behavior.
    let Ok(target) = target.canonicalize() else {
        return Ok(());
    };
    let Ok(root) = data_root.join("languages").canonicalize() else {
        return Ok(());
    };
    if !target.starts_with(&root) {
        return Ok(());
    }
    eyre::bail!(
        "a legacy {mutation:?} operation on the managed shared language library \
         is refused before any effect; shared instances only change through the \
         controlled install path"
    )
}

#[cfg(test)]
mod tests;
