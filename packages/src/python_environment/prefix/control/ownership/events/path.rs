//! Unix byte paths follow the actual prefix's component and byte rules.

use super::*;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

pub(super) fn limits(limits: OwnershipEventLimits) -> Result<()> {
    let prefix = limits.prefix;
    ensure!(
        limits.final_prefix_bytes != 0
            && limits.max_event_bytes != 0
            && limits.max_events != 0
            && limits.max_total_path_bytes != 0
            && prefix.max_components != 0
            && prefix.max_entries != 0
            && prefix.max_path_bytes != 0
            && prefix.max_total_bytes != 0
            && prefix.max_cleanup_entries != 0,
        "ownership event limits must be nonzero"
    );
    Ok(())
}
pub(super) fn length(length: usize, limits: OwnershipEventLimits) -> Result<()> {
    ensure!(
        limits
            .final_prefix_bytes
            .checked_add(1)
            .and_then(|total| total.checked_add(length))
            .is_some_and(|total| total <= limits.prefix.max_path_bytes),
        "ownership relative path byte limit exceeded"
    );
    Ok(())
}
pub(super) fn validate(bytes: &[u8], limits: OwnershipEventLimits) -> Result<()> {
    length(bytes.len(), limits)?;
    ensure!(
        !bytes.is_empty() && bytes[0] != b'/' && !bytes.contains(&0),
        "invalid ownership relative path"
    );
    ensure!(
        !bytes
            .split(|byte| *byte == b'/')
            .any(|part| matches!(part, b"." | b"..")),
        "invalid ownership relative component"
    );
    let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "invalid ownership relative component"
    );
    ensure!(
        path.components().count() <= limits.prefix.max_components,
        "ownership path component limit exceeded"
    );
    Ok(())
}
pub(super) fn canonical(bytes: &[u8], limits: OwnershipEventLimits) -> Result<Vec<u8>> {
    validate(bytes, limits)?;
    let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
    let mut canonical = Vec::new();
    canonical
        .try_reserve_exact(bytes.len())
        .map_err(|_| eyre::eyre!("ownership path allocation limit failed"))?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            unreachable!("validated Unix component")
        };
        if !canonical.is_empty() {
            canonical.push(b'/');
        }
        canonical.extend_from_slice(name.as_bytes());
    }
    Ok(canonical)
}
pub(super) fn quarantine(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() == 34
            && bytes.starts_with(b"q-")
            && bytes[2..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "invalid ownership quarantine name"
    );
    Ok(())
}
