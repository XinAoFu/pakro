//! Proposed declaration bytes borrowing their actual project observation.
//! Preparing a document never grants publication authority or writes a file.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::config_file::pakro_toml::insert_preserving_decor;
use crate::config::config_file::{ConfigDocumentFormat, parse_static_config_as};
use crate::packages::PackageInstallRequest;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table, Value};

use super::snapshot::ObservedProjectInputs;

/// The key is an exact declaration spelling, selected by the caller. Resolving
/// aliases or backend options here would consult mutable global configuration.
pub(crate) enum DeclarationChange {
    SetTool {
        key: String,
        versions: Vec<String>,
    },
    RemoveTool {
        key: String,
    },
    AddPinnedPackages {
        requests: Vec<PackageInstallRequest>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum DeclarationEditError {
    #[error("the declaration target was not included in this observation")]
    UnobservedTarget,
    #[error("the declaration format cannot be edited by this operation")]
    UnsupportedFormat,
    #[error("the static declaration edit is invalid")]
    InvalidEdit,
    #[error("this version selector cannot be edited without changing its meaning")]
    UnsupportedSelector,
    #[error("the declaration document is invalid")]
    InvalidDocument,
    #[error("the proposed declaration document exceeds its byte limit")]
    Limit,
    #[error("the project input observation is no longer current")]
    Revoked,
    #[error("the proposed declaration document belongs to another observation")]
    WrongOwner,
}

pub(crate) struct ProposedDeclarationEdit<'snapshot> {
    owner: &'snapshot ObservedProjectInputs,
    target: PathBuf,
    bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl ProposedDeclarationEdit<'_> {
    pub(crate) fn target(&self) -> &Path {
        &self.target
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn recheck_for(
        &self,
        owner: &ObservedProjectInputs,
    ) -> Result<(), DeclarationEditError> {
        if !std::ptr::eq(self.owner, owner) {
            return Err(DeclarationEditError::WrongOwner);
        }
        self.owner
            .recheck()
            .map_err(|_| DeclarationEditError::Revoked)
    }
}

impl std::fmt::Debug for ProposedDeclarationEdit<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("ProposedDeclarationEdit")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn prepare_edit<'snapshot>(
    snapshot: &'snapshot ObservedProjectInputs,
    target: &Path,
    changes: &[DeclarationChange],
) -> Result<ProposedDeclarationEdit<'snapshot>, DeclarationEditError> {
    snapshot
        .recheck()
        .map_err(|_| DeclarationEditError::Revoked)?;
    if !snapshot.contains_edit_target(target) {
        return Err(DeclarationEditError::UnobservedTarget);
    }
    if snapshot.document_format(target) != Some(ConfigDocumentFormat::PakroToml) {
        return Err(DeclarationEditError::UnsupportedFormat);
    }
    let maximum = snapshot.document_byte_limit();
    validate_changes(changes, maximum)?;
    let original = snapshot.document(target).unwrap_or_default();
    if original.len() > maximum {
        return Err(DeclarationEditError::Limit);
    }
    let original =
        std::str::from_utf8(original).map_err(|_| DeclarationEditError::InvalidDocument)?;
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|_| DeclarationEditError::InvalidDocument)?;
    for change in changes {
        match change {
            DeclarationChange::SetTool { key, versions } => set_tool(&mut document, key, versions)?,
            DeclarationChange::RemoveTool { key } => {
                if let Some(tools) = document.get_mut("tools") {
                    tools
                        .as_table_like_mut()
                        .ok_or(DeclarationEditError::InvalidDocument)?
                        .remove(key);
                }
            }
            DeclarationChange::AddPinnedPackages { requests } => {
                add_packages(&mut document, requests)?
            }
        }
    }
    let mut output = BoundedDocument {
        bytes: Vec::new(),
        maximum,
    };
    write!(&mut output, "{document}").map_err(|_| DeclarationEditError::Limit)?;
    parse_static_config_as(&output.bytes, target, ConfigDocumentFormat::PakroToml)
        .map_err(|_| DeclarationEditError::InvalidEdit)?;
    let sha256 = Sha256::digest(&output.bytes).into();
    snapshot
        .recheck()
        .map_err(|_| DeclarationEditError::Revoked)?;
    Ok(ProposedDeclarationEdit {
        owner: snapshot,
        target: target.to_path_buf(),
        bytes: output.bytes,
        sha256,
    })
}

fn validate_changes(
    changes: &[DeclarationChange],
    maximum: usize,
) -> Result<(), DeclarationEditError> {
    if changes.is_empty() {
        return Err(DeclarationEditError::InvalidEdit);
    }
    let mut bytes = 0usize;
    let mut field = |value: &str| {
        if value.is_empty() {
            return Err(DeclarationEditError::InvalidEdit);
        }
        bytes = bytes
            .checked_add(value.len())
            .filter(|bytes| *bytes <= maximum)
            .ok_or(DeclarationEditError::Limit)?;
        Ok(())
    };
    for change in changes {
        match change {
            DeclarationChange::SetTool { key, versions } => {
                field(key)?;
                if versions.is_empty() {
                    return Err(DeclarationEditError::InvalidEdit);
                }
                for version in versions {
                    field(version)?;
                }
            }
            DeclarationChange::RemoveTool { key } => field(key)?,
            DeclarationChange::AddPinnedPackages { requests } => {
                if requests.is_empty() {
                    return Err(DeclarationEditError::InvalidEdit);
                }
                for request in requests {
                    if request.runtime_name.contains('@') {
                        return Err(DeclarationEditError::InvalidEdit);
                    }
                    for value in [
                        &request.runtime_name,
                        &request.runtime_version,
                        &request.name,
                        &request.version,
                    ] {
                        field(value)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn set_tool(
    document: &mut DocumentMut,
    key: &str,
    versions: &[String],
) -> Result<(), DeclarationEditError> {
    let tools = document.entry("tools").or_insert_with(toml_edit::table);
    let current = tools
        .as_table_like_mut()
        .ok_or(DeclarationEditError::InvalidDocument)?
        .get_mut(key);
    match current {
        None => insert_preserving_decor(tools, key, Item::Value(version_value(versions))),
        Some(Item::Value(Value::Array(array))) => {
            // Each existing element may carry options and comments. Without a
            // one-to-one match, their ownership cannot safely be guessed.
            if array.len() != versions.len() {
                return Err(DeclarationEditError::UnsupportedSelector);
            }
            for (old, version) in array.iter_mut().zip(versions) {
                set_single_version(old, version)?;
            }
        }
        Some(Item::Value(value @ Value::String(_))) if versions.len() > 1 => {
            let mut replacement = version_value(versions);
            *replacement.decor_mut() = value.decor().clone();
            *value = replacement;
        }
        Some(Item::Value(value)) if versions.len() == 1 => set_single_version(value, &versions[0])?,
        Some(item @ Item::Table(_)) if versions.len() == 1 => {
            let table = item
                .as_table_like()
                .ok_or(DeclarationEditError::UnsupportedSelector)?;
            if table.get("version").and_then(Item::as_str).is_none()
                || ["path", "prefix", "ref"]
                    .iter()
                    .any(|key| table.contains_key(key))
            {
                return Err(DeclarationEditError::UnsupportedSelector);
            }
            if table.get("version").and_then(Item::as_str) == Some(versions[0].as_str()) {
                return Ok(());
            }
            insert_preserving_decor(item, "version", toml_edit::value(versions[0].as_str()));
        }
        Some(_) => return Err(DeclarationEditError::UnsupportedSelector),
    }
    Ok(())
}

fn version_value(versions: &[String]) -> Value {
    if versions.len() == 1 {
        return Value::from(versions[0].clone());
    }
    let mut array = toml_edit::Array::new();
    for version in versions {
        array.push(version.as_str());
    }
    Value::Array(array)
}

fn set_single_version(old: &mut Value, version: &str) -> Result<(), DeclarationEditError> {
    match old {
        Value::String(_) => {
            if old.as_str() == Some(version) {
                return Ok(());
            }
            let mut new = Value::from(version);
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        Value::InlineTable(table) => {
            if ["path", "prefix", "ref"]
                .iter()
                .any(|key| table.contains_key(key))
            {
                return Err(DeclarationEditError::UnsupportedSelector);
            }
            let previous = table
                .get_mut("version")
                .filter(|value| value.is_str())
                .ok_or(DeclarationEditError::UnsupportedSelector)?;
            if previous.as_str() == Some(version) {
                return Ok(());
            }
            let mut new = Value::from(version);
            *new.decor_mut() = previous.decor().clone();
            *previous = new;
        }
        _ => return Err(DeclarationEditError::UnsupportedSelector),
    }
    Ok(())
}

fn add_packages(
    document: &mut DocumentMut,
    requests: &[PackageInstallRequest],
) -> Result<(), DeclarationEditError> {
    let packages = document.entry("packages").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    for request in requests {
        let group_key = request.group_key();
        let is_inline = packages.is_inline_table();
        let groups = packages
            .as_table_like_mut()
            .ok_or(DeclarationEditError::InvalidDocument)?;
        if !groups.contains_key(&group_key) {
            let group = if is_inline {
                Item::Value(Value::InlineTable(toml_edit::InlineTable::new()))
            } else {
                Item::Table(Table::new())
            };
            groups.insert(&group_key, group);
        }
        let group = groups
            .get_mut(&group_key)
            .ok_or(DeclarationEditError::InvalidDocument)?;
        if group.as_table_like().is_none() {
            return Err(DeclarationEditError::InvalidDocument);
        }
        if group
            .as_table_like()
            .and_then(|table| table.get(&request.name))
            .and_then(Item::as_str)
            == Some(request.version.as_str())
        {
            continue;
        }
        let mut value = Value::from(request.version.clone());
        // The shared helper preserves ordinary-table keys and values. Inline
        // values carry their spacing directly, so retain it before insertion.
        if let Some(old) = group
            .as_table_like()
            .and_then(|table| table.get(&request.name))
            .and_then(Item::as_value)
        {
            *value.decor_mut() = old.decor().clone();
        }
        insert_preserving_decor(group, &request.name, Item::Value(value));
    }
    Ok(())
}

struct BoundedDocument {
    bytes: Vec<u8>,
    maximum: usize,
}
impl std::fmt::Write for BoundedDocument {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.bytes
            .len()
            .checked_add(value.len())
            .filter(|bytes| *bytes <= self.maximum)
            .ok_or(std::fmt::Error)?;
        self.bytes
            .try_reserve(value.len())
            .map_err(|_| std::fmt::Error)?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests;
