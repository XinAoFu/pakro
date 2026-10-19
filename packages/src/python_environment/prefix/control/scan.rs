//! Bounded namespace facts for recovery and conservative input collection.
//! A listed directory is never cleanup or publication authority.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct NamespaceScanLimits {
    pub max_entries: usize,
    pub max_total_name_bytes: usize,
}
impl Default for NamespaceScanLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_048_576,
            max_total_name_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NamespaceDirectory {
    pub name: String,
    pub identity: [u64; 2],
}

#[derive(Debug)]
pub(crate) struct EnvironmentNamespace {
    pub generations: Vec<NamespaceDirectory>,
    pub quarantined: Vec<NamespaceDirectory>,
}

impl StoreControl<'_> {
    pub(crate) fn scan_environment_namespace(
        &self,
        limits: NamespaceScanLimits,
    ) -> Result<EnvironmentNamespace> {
        ensure!(
            limits.max_entries != 0 && limits.max_total_name_bytes != 0,
            "environment namespace limits must be nonzero"
        );
        self.check_binding()?;
        self.core.ensure_catalog()?;
        let mut budget = ScanBudget {
            limits,
            entries: 0,
            name_bytes: 0,
        };
        let mut result = EnvironmentNamespace {
            generations: Vec::new(),
            quarantined: Vec::new(),
        };
        visit_names(&self.store.root, &mut budget, |name, budget| {
            match name {
                ".allocation.lock" => {
                    let file = File::from(openat(
                        &self.store.root,
                        name,
                        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                        Mode::empty(),
                    )?);
                    self.validate_file(&file)?;
                    ensure_entry_identity(&self.store.root, name, FileIdentity::of(&file)?)?;
                }
                ".control" => {
                    let (directory, _) =
                        inspect_directory(&self.store.root, name, self.store.expected_uid)?;
                    ensure!(
                        FileIdentity::of(&directory)? == FileIdentity::of(&self.root)?,
                        "environment control namespace identity changed"
                    );
                }
                ".leases" => {
                    inspect_directory(&self.store.root, name, self.store.expected_uid)?;
                }
                ".quarantine" => {
                    let (directory, _) =
                        inspect_directory(&self.store.root, name, self.store.expected_uid)?;
                    visit_names(&directory, budget, |captured, _budget| {
                        ensure!(
                            valid_identifier(captured, b'q'),
                            "unknown quarantined environment namespace"
                        );
                        let (_, identity) =
                            inspect_directory(&directory, captured, self.store.expected_uid)?;
                        result.quarantined.push(NamespaceDirectory {
                            name: captured.into(),
                            identity,
                        });
                        Ok(())
                    })?;
                    ensure_entry_identity(&self.store.root, name, FileIdentity::of(&directory)?)?;
                }
                _ => {
                    ensure!(
                        valid_identifier(name, b'g'),
                        "unknown environment store namespace"
                    );
                    let (_, identity) =
                        inspect_directory(&self.store.root, name, self.store.expected_uid)?;
                    result.generations.push(NamespaceDirectory {
                        name: name.into(),
                        identity,
                    });
                }
            }
            Ok(())
        })?;
        self.check_binding()?;
        result
            .generations
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        result
            .quarantined
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(result)
    }
}

struct ScanBudget {
    limits: NamespaceScanLimits,
    entries: usize,
    name_bytes: usize,
}
fn visit_names(
    parent: &File,
    budget: &mut ScanBudget,
    mut visit: impl FnMut(&str, &mut ScanBudget) -> Result<()>,
) -> Result<()> {
    let mut directory = nix::dir::Dir::openat(
        parent,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    for entry in directory.iter() {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        budget.entries = budget
            .entries
            .checked_add(1)
            .filter(|count| *count <= budget.limits.max_entries)
            .ok_or_else(|| eyre::eyre!("environment namespace entry limit exceeded"))?;
        budget.name_bytes = budget
            .name_bytes
            .checked_add(raw.len())
            .filter(|count| *count <= budget.limits.max_total_name_bytes)
            .ok_or_else(|| eyre::eyre!("environment namespace name byte limit exceeded"))?;
        let name = std::str::from_utf8(raw)
            .map_err(|_| eyre::eyre!("invalid environment namespace name"))?;
        visit(name, budget)?;
    }
    Ok(())
}
fn valid_identifier(name: &str, prefix: u8) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 34
        && bytes[0] == prefix
        && bytes[1] == b'-'
        && bytes[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}
fn inspect_directory(parent: &File, name: &str, uid: u32) -> Result<(File, [u64; 2])> {
    let directory = open_directory(parent, name)?;
    validate_owned_directory(&directory, uid)?;
    let metadata = directory.metadata()?;
    ensure!(
        metadata.mode() & 0o7777 == 0o700,
        "environment namespace directory is not private"
    );
    ensure_entry_identity(parent, name, FileIdentity::of(&directory)?)?;
    Ok((directory, [metadata.dev(), metadata.ino()]))
}

#[cfg(test)]
mod tests;
