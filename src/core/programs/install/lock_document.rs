//! Unified project-lock overlay and portable declaration commitment.
use crate::libraries::{language::lease::LanguageLease, project::snapshot::ObservedProjectInputs};
use crate::packages::python_resolution::{
    lock_model::PythonLockModel, lock_session::BoundPythonLock,
};
use eyre::{ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};

pub(crate) fn declaration_fingerprint(observed: &ObservedProjectInputs) -> Result<[u8; 32]> {
    observed.recheck()?;
    let mut hash = Sha256::new_with_prefix(b"pakro.portable-project-declarations/v1\0");
    for path in observed
        .paths()
        .filter(|path| path.extension().is_none_or(|extension| extension != "lock"))
    {
        let name = path
            .strip_prefix(observed.root())
            .ok()
            .or_else(|| path.file_name().map(std::path::Path::new))
            .context("declaration path has no portable name")?;
        hash.update(name.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(
            observed
                .document(path)
                .context("observed declaration is missing")?,
        );
        hash.update([0]);
    }
    Ok(hash.finalize().into())
}

pub(crate) fn validate_declarations(
    observed: &ObservedProjectInputs,
    model: &PythonLockModel,
) -> Result<()> {
    ensure!(
        model.declaration_fingerprint() == Some(&declaration_fingerprint(observed)?),
        "strict Python lock differs from the current declaration documents or lacks their fingerprint"
    );
    Ok(())
}

pub(crate) fn prepare_document(
    root: &BoundPythonLock,
    observed: &ObservedProjectInputs,
    language: &LanguageLease,
    model: PythonLockModel,
) -> Result<Vec<u8>> {
    let model = model.with_declaration_fingerprint(declaration_fingerprint(observed)?);
    let prepared = root.prepare_document(model, Default::default())?;
    let mut document: toml_edit::DocumentMut = std::str::from_utf8(prepared.bytes())?.parse()?;
    let source = language.source_pin()?;
    let mut entry = toml_edit::Table::new();
    let version = language.lock_version()?;
    entry.insert("version", toml_edit::value(version.clone()));
    entry.insert("backend", toml_edit::value("core:python"));
    let declared = crate::programs::query::snapshot::effective_python_declaration(observed)
        .context("project Python declaration is missing")?;
    let mut specifiers = toml_edit::Array::new();
    specifiers.push(declared);
    entry.insert("specifiers", toml_edit::value(specifiers));
    let mut platform = toml_edit::Table::new();
    platform.insert(
        "url",
        toml_edit::value(
            source
                .url
                .context("actual language source URL is missing")?,
        ),
    );
    platform.insert(
        "checksum",
        toml_edit::value(
            source
                .checksum
                .context("actual language source checksum is missing")?,
        ),
    );
    let mut platforms = toml_edit::Table::new();
    platforms.insert(
        &crate::backend::platform_target::PlatformTarget::from_current().to_key(),
        toml_edit::Item::Table(platform),
    );
    entry.insert("platforms", toml_edit::Item::Table(platforms));
    if document.get("tools").is_none() {
        document.insert("tools", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let tools = document
        .get_mut("tools")
        .and_then(toml_edit::Item::as_table_mut)
        .context("native lock tools must be a table")?;
    if tools.get("python").is_none() {
        tools.insert(
            "python",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let entries = tools
        .get_mut("python")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .context("native Python lock must contain table entries")?;
    let bare_version = language.descriptor().language_version.full_version();
    let current = entries.iter_mut().find(|entry| {
        entry
            .get("version")
            .and_then(toml_edit::Item::as_str)
            .is_some_and(|existing| existing == version || existing == bare_version)
    });
    if let Some(current) = current {
        // Preserve comments, unknown Python fields and other targets. Only
        // this selected runtime's source facts belong to the new result.
        replace_value_preserving_decor(current, "version", toml_edit::Value::from(version))?;
        replace_value_preserving_decor(current, "backend", toml_edit::Value::from("core:python"))?;
        if let Some(specifiers) = current.get_mut("specifiers") {
            let specifiers = specifiers
                .as_array_mut()
                .context("native runtime specifiers must be an array")?;
            let declared = entry["specifiers"]
                .as_array()
                .unwrap()
                .get(0)
                .unwrap()
                .as_str()
                .unwrap();
            if !specifiers
                .iter()
                .any(|value| value.as_str() == Some(declared))
            {
                specifiers.push(declared);
            }
        } else {
            current.insert("specifiers", entry.remove("specifiers").unwrap());
        }
        if current.get("platforms").is_none() {
            current.insert("platforms", toml_edit::Item::Table(toml_edit::Table::new()));
        }
        let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
        let mut new_platform = entry["platforms"]
            .as_table_mut()
            .unwrap()
            .remove(&target)
            .unwrap();
        let platforms = current["platforms"]
            .as_table_mut()
            .context("native runtime platforms must be a table")?;
        if let Some(platform) = platforms.get_mut(&target) {
            let platform = platform
                .as_table_mut()
                .context("native runtime target must be a table")?;
            let replacement = new_platform.as_table_mut().unwrap();
            for field in ["url", "checksum"] {
                replace_value_preserving_decor(
                    platform,
                    field,
                    replacement.remove(field).unwrap().into_value().unwrap(),
                )?;
            }
        } else {
            platforms.insert(&target, new_platform);
        }
    } else {
        entries.push(entry);
    }
    let bytes = document.to_string().into_bytes();
    ensure!(
        bytes.len() <= observed.document_byte_limit(),
        "generated unified lock exceeds its observed document budget"
    );
    crate::lockfile::Lockfile::parse_observed(&bytes)?;
    crate::packages::python_resolution::lock_session::decode_root(&bytes, Default::default())?;
    prepared.recheck_for(root)?;
    observed.recheck()?;
    Ok(bytes)
}

fn replace_value_preserving_decor(
    table: &mut toml_edit::Table,
    key: &str,
    mut replacement: toml_edit::Value,
) -> Result<()> {
    if let Some(previous) = table.get_mut(key) {
        let previous = previous
            .as_value_mut()
            .context("native lock field must be a value")?;
        *replacement.decor_mut() = previous.decor().clone();
        *previous = replacement;
    } else {
        table.insert(key, toml_edit::Item::Value(replacement));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
