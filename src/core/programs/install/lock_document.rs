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
    prepare_document_inner(root, observed, language, model, false)
}

pub(crate) fn prepare_document_for_update(
    root: &BoundPythonLock,
    observed: &ObservedProjectInputs,
    language: &LanguageLease,
    model: PythonLockModel,
) -> Result<Vec<u8>> {
    prepare_document_inner(root, observed, language, model, true)
}

fn prepare_document_inner(
    root: &BoundPythonLock,
    observed: &ObservedProjectInputs,
    language: &LanguageLease,
    model: PythonLockModel,
    update: bool,
) -> Result<Vec<u8>> {
    let model = model.with_declaration_fingerprint(declaration_fingerprint(observed)?);
    let prepared = root.prepare_document(model, Default::default())?;
    if update {
        // Updating a valid result is not authority to erase damaged native
        // input that would otherwise be rejected by the ordinary decoder.
        crate::lockfile::Lockfile::parse_observed(prepared.bytes())?;
    }
    let mut document: toml_edit::DocumentMut = std::str::from_utf8(prepared.bytes())?.parse()?;
    let source = language.source_pin()?;
    let mut entry = toml_edit::Table::new();
    let version = language.lock_version()?;
    entry.insert("version", toml_edit::value(version.clone()));
    entry.insert("backend", toml_edit::value("core:python"));
    if source.source_python.is_some() {
        let mut options = toml_edit::Table::new();
        options.insert("source", toml_edit::value("source"));
        entry.insert("options", toml_edit::Item::Table(options));
    }
    let declared = crate::programs::query::snapshot::effective_python_declaration(observed)
        .context("project Python declaration is missing")?;
    let mut specifiers = toml_edit::Array::new();
    specifiers.push(declared);
    entry.insert("specifiers", toml_edit::value(specifiers));
    let mut platform = toml_edit::Table::new();
    if let Some(record) = &source.source_python {
        platform.insert("install", toml_edit::value("source"));
        platform.insert(
            "source_python",
            toml_edit::Item::Table(source_python_table(record)?),
        );
    }
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
    if update {
        let aliases: Vec<_> = tools
            .iter()
            .filter(|(name, item)| {
                *name != "python"
                    && (crate::programs::query::snapshot::python_tool_name(observed, name)
                        || item.as_array_of_tables().is_some_and(|entries| {
                            !entries.is_empty()
                                && entries.iter().all(|entry| {
                                    entry
                                        .get("backend")
                                        .and_then(toml_edit::Item::as_str)
                                        .is_some_and(|backend| {
                                            matches!(backend, "core:python" | "python")
                                        })
                                })
                        }))
            })
            .map(|(name, _)| name.to_owned())
            .collect();
        for name in aliases {
            tools.remove(&name);
        }
    }
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
    if update {
        let retained = entries
            .iter()
            .find(|entry| {
                entry
                    .get("version")
                    .and_then(toml_edit::Item::as_str)
                    .is_some_and(|existing| existing == version || existing == bare_version)
            })
            .cloned();
        *entries = toml_edit::ArrayOfTables::new();
        if let Some(retained) = retained {
            entries.push(retained);
        }
    }
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
        if update {
            replace_value_preserving_decor(
                current,
                "specifiers",
                entry.remove("specifiers").unwrap().into_value().unwrap(),
            )?;
        } else if let Some(specifiers) = current.get_mut("specifiers") {
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
        let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
        overlay_runtime_source(current, &mut entry, &target, update)?;
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

fn source_python_table(
    record: &crate::libraries::language::source::SourcePythonBuildRecord,
) -> Result<toml_edit::Table> {
    let mut document = toml::to_string(record)?.parse::<toml_edit::DocumentMut>()?;
    // Parsed positions belong to this separate document. Retaining them can
    // emit a child before its enclosing [[tools.python]] in the unified lock.
    let mut pending = vec![document.as_table_mut()];
    while let Some(table) = pending.pop() {
        table.set_position(None);
        for (_, item) in table.iter_mut() {
            match item {
                toml_edit::Item::Table(table) => pending.push(table),
                toml_edit::Item::ArrayOfTables(tables) => pending.extend(tables.iter_mut()),
                _ => {}
            }
        }
    }
    Ok(document.into_table())
}

/// These fields describe one selected runtime result and move together, even
/// when its ordinary version or source archive URL did not change.
fn overlay_runtime_source(
    current: &mut toml_edit::Table,
    entry: &mut toml_edit::Table,
    target: &str,
    update: bool,
) -> Result<()> {
    let source_option = entry.get("options")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|options| options.get("source"))
        .cloned();
    if let Some(options) = current.get_mut("options") {
        let options = options.as_table_like_mut().context("native runtime options must be a table")?;
        if let Some(mut source) = source_option {
            if let Some(previous) = options.get("source").and_then(toml_edit::Item::as_value)
                && let Some(value) = source.as_value_mut()
            {
                *value.decor_mut() = previous.decor().clone();
            }
            options.insert("source", source);
        } else {
            options.remove("source");
        }
        if options.is_empty() { current.remove("options"); }
    } else if let Some(source) = source_option {
        let mut options = toml_edit::Table::new();
        options.insert("source", source);
        current.insert("options", toml_edit::Item::Table(options));
    }
    if current.get("platforms").is_none() {
        current.insert("platforms", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let mut new_platform = entry["platforms"].as_table_mut().unwrap().remove(target).unwrap();
    let platforms = current["platforms"].as_table_mut().context("native runtime platforms must be a table")?;
    if update {
        let stale: Vec<_> = platforms.iter().filter(|(name, _)| *name != target)
            .map(|(name, _)| name.to_owned()).collect();
        for name in stale { platforms.remove(&name); }
    }
    if let Some(platform) = platforms.get_mut(target) {
        let platform = platform.as_table_mut().context("native runtime target must be a table")?;
        let replacement = new_platform.as_table_mut().unwrap();
        for field in ["url", "checksum"] {
            replace_value_preserving_decor(platform, field,
                replacement.remove(field).unwrap().into_value().unwrap())?;
        }
        for field in ["install", "source_python"] {
            if let Some(value) = replacement.remove(field) { platform.insert(field, value); }
            else { platform.remove(field); }
        }
    } else {
        platforms.insert(target, new_platform);
    }
    Ok(())
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
        let unchanged = match (previous.as_str(), replacement.as_str()) {
            (Some(previous), Some(next)) => previous == next,
            _ => match (previous.as_array(), replacement.as_array()) {
                (Some(previous), Some(next)) => {
                    previous.len() == next.len()
                        && previous.iter().zip(next.iter()).all(|(previous, next)| {
                            previous.as_str().is_some() && previous.as_str() == next.as_str()
                        })
                }
                _ => false,
            },
        };
        if unchanged {
            return Ok(());
        }
        *replacement.decor_mut() = previous.decor().clone();
        *previous = replacement;
    } else {
        table.insert(key, toml_edit::Item::Value(replacement));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
