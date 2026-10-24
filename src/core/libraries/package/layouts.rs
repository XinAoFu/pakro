//! Lightweight shared startup projection over immutable per-package trees.
use super::lease::{LayoutLease, acquire_layout};
use super::manifest::{self, LayoutManifest, PackageFacts};
use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::prefix::verify::{PrefixExpectedFile, TreeInventoryKind};
use crate::packages::python_environment::prefix::{
    EnvironmentStore, PrefixLimits, UnpublishedPrefix,
};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) mod projection;

pub(crate) fn layout_key(language: [u8; 32], graph: [u8; 32]) -> [u8; 32] {
    hash_layout_key(b"pakro.shared-wheel-layout.v2\0", language, graph)
}
pub(super) fn layout_key_for(recipe: u32, language: [u8; 32], graph: [u8; 32]) -> Result<[u8; 32]> {
    match recipe {
        1 => Ok(hash_layout_key(
            b"pakro.shared-wheel-layout.v1\0",
            language,
            graph,
        )),
        2 => Ok(layout_key(language, graph)),
        _ => eyre::bail!("unsupported shared layout recipe"),
    }
}
fn hash_layout_key(recipe: &[u8], language: [u8; 32], graph: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(recipe);
    hash.update(language);
    hash.update(graph);
    hash.finalize().into()
}
fn quote(path: &Path) -> Result<String> {
    let value = path.to_str().context("shared startup path is not UTF-8")?;
    ensure!(
        !value.contains(['\n', '\r', '\0']),
        "shared startup path contains control characters"
    );
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
fn write(
    prefix: &mut UnpublishedPrefix,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<PrefixExpectedFile> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        prefix.create_dir_all(parent)?;
    }
    let mut file = prefix.create_file_exclusive(path, mode)?;
    file.write_all(bytes)?;
    file.finish()?;
    Ok(PrefixExpectedFile {
        relative: path.into(),
        sha256: Sha256::digest(bytes).into(),
        size: bytes.len() as u64,
        mode,
    })
}
fn startup_environment(prefix: &Path) -> Result<String> {
    Ok(format!(
        "export PYTHONDONTWRITEBYTECODE=1\nexport PYTHONNOUSERSITE=1\nexport PYTHONPATH={}\nexport PATH={}:\"$PATH\"\n",
        quote(&prefix.join("bootstrap"))?,
        quote(&prefix.join("bin"))?
    ))
}

pub(crate) fn prepare_layout(
    context: &OperationContext,
    language: &LanguageLease,
    graph: [u8; 32],
    mut sources: Vec<(PackageFacts, InspectedWheelArchive)>,
) -> Result<LayoutLease> {
    context.checkpoint()?;
    language.check_binding()?;
    let key = layout_key(language.key().fingerprint(), graph);
    if let Some(existing) = acquire_layout(context, key)? {
        return Ok(existing);
    }
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package library writes"
    );
    ensure!(
        !sources.is_empty() && sources.len() <= 65536,
        "shared package graph is empty or exceeds its budget"
    );
    sources.sort_by(|a, b| a.0.key.cmp(&b.0.key));
    let mut identities = BTreeSet::new();
    let mut names = BTreeSet::new();
    for (facts, _) in &sources {
        ensure!(
            identities.insert(facts.key) && names.insert(facts.name.clone()),
            "shared graph repeats a package identity or name"
        );
    }
    for (facts, _) in &sources {
        ensure!(
            facts.dependencies.iter().all(|d| identities.contains(d)),
            "shared graph has an absent dependency"
        );
    }
    // Existing objects retain actual SH; every missing object retains its
    // original allocation EX until the complete closed group is published.
    // This also covers legal dependency cycles without exposing an unprotected
    // interval in which a concurrent remover could erase a new dependency.
    let (mut existing, pending) = language.with_package_materialization(|scope| {
        let mut existing = Vec::new();
        let mut pending = Vec::new();
        for (facts, archive) in sources {
            ensure!(
                facts.name == archive.manifest().name()
                    && facts.version == archive.manifest().version()
                    && facts.artifact == archive.digest().to_string(),
                "package graph facts differ from the inspected wheel"
            );
            if let Some(lease) = super::lease::acquire_package(context, facts.key)? {
                ensure!(
                    lease.manifest().facts == facts
                        && lease.manifest().language == scope.language().key().fingerprint(),
                    "conflicting package facts or language binding"
                );
                existing.push(lease);
            } else {
                pending.push(super::tree::prepare_package_inputs_in_batch(
                    context, scope, facts, archive,
                )?);
            }
        }
        Ok((existing, pending))
    })?;
    // The complete language inventory has been checked again before any
    // pending package can become publicly visible.
    let mut published = Vec::new();
    let mut published_keys = Vec::new();
    for source in pending {
        let facts = source.manifest.facts.clone();
        match source.publish() {
            Ok(guard) => {
                published_keys.push(facts.key);
                published.push(guard);
            }
            Err(error) => {
                match if error
                    .is::<crate::packages::python_environment::prefix::SharedPublicationConflict>()
                {
                    super::lease::acquire_package(context, facts.key)
                } else {
                    Ok(None)
                } {
                    Ok(Some(winner))
                        if winner.manifest().facts == facts
                            && winner.manifest().language == language.key().fingerprint() =>
                    {
                        existing.push(winner)
                    }
                    outcome => {
                        let mut rollback_error = None;
                        for guard in published.into_iter().rev() {
                            if let Err(e) = guard.rollback() {
                                rollback_error = Some(e);
                            }
                        }
                        if let Some(rollback) = rollback_error {
                            return Err(eyre::eyre!(
                                "shared publication failed: {error:#}; rollback retained evidence: {rollback:#}"
                            ));
                        }
                        return Err(eyre::eyre!(
                            "shared package publication failed: {error:#}; winner state: {}",
                            match outcome {
                                Ok(_) => "absent or conflicting".into(),
                                Err(e) => e.to_string(),
                            }
                        ));
                    }
                }
            }
        }
    }
    drop(published); // Every node and dependency manifest now exists.
    let mut packages = existing;
    for key in published_keys {
        packages.push(
            super::lease::acquire_package(context, key)?
                .context("published shared package disappeared")?,
        );
    }
    packages.sort_by_key(|p| p.manifest().key());
    let mut visible_files = BTreeMap::new();
    for package in &packages {
        for entry in package.manifest().storage.inventory.entries() {
            context.checkpoint()?;
            if !entry.relative.starts_with("site-packages") {
                continue;
            }
            if let TreeInventoryKind::File { size, sha256 } = entry.kind {
                let facts = (size, sha256, entry.mode);
                if let Some(previous) = visible_files.insert(entry.relative.clone(), facts) {
                    ensure!(
                        previous == facts,
                        "selected wheels contain conflicting import content at {}",
                        entry.relative.display()
                    );
                }
            }
        }
    }
    let projection = projection::plan(context, &packages)?;
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    let final_prefix = prefix.final_path().to_owned();
    let mut expected_files = Vec::new();
    if let Some(projection) = &projection {
        projection::materialize(context, &mut prefix, &packages, projection)?;
    }
    let mut bootstrap = String::from(
        "# Generated by Pakro for this exact shared package graph.\nimport os, site, sys\ntry:\n",
    );
    if projection.is_some() {
        bootstrap.push_str(&format!(
            "    site.addsitedir({})\n",
            serde_json::to_string(
                final_prefix
                    .join("site-packages")
                    .to_str()
                    .context("projected package path is not UTF-8")?
            )?
        ));
    }
    for package in &packages {
        bootstrap.push_str(&format!(
            "    site.addsitedir({})\n",
            serde_json::to_string(
                package
                    .site_packages()
                    .to_str()
                    .context("package path is not UTF-8")?
            )?
        ));
    }
    bootstrap.push_str("except BaseException as exc:\n    sys.stderr.write('Pakro package startup failed: %s\\n' % exc)\n    os._exit(120)\n");
    expected_files.push(write(
        &mut prefix,
        Path::new("bootstrap/sitecustomize.py"),
        bootstrap.as_bytes(),
        0o644,
    )?);
    let env = startup_environment(&final_prefix)?;
    let interpreter = format!(
        "#!/bin/sh\n{env}exec {} \"$@\"\n",
        quote(language.entrypoint())?
    );
    let short = language.descriptor().markers.python_version.as_str();
    let python_names = [
        "python".to_owned(),
        "python3".to_owned(),
        format!("python{short}"),
    ];
    for name in &python_names {
        expected_files.push(write(
            &mut prefix,
            &Path::new("bin").join(name),
            interpreter.as_bytes(),
            0o755,
        )?);
    }
    let mut scripts = BTreeMap::<String, PathBuf>::new();
    for package in &packages {
        for entry in package.manifest().storage.inventory.entries() {
            if !matches!(entry.kind, TreeInventoryKind::File { .. })
                || entry.relative.parent() != Some(Path::new("bin"))
            {
                continue;
            }
            let name = entry
                .relative
                .file_name()
                .and_then(|v| v.to_str())
                .context("package script name is not UTF-8")?
                .to_owned();
            if name == ".pakro python" {
                continue;
            } // Owned package interpreter adapter.
            ensure!(
                !python_names.contains(&name) && !scripts.contains_key(&name),
                "package entry point collides with another selected executable: {name}"
            );
            scripts.insert(name, package.root().join(&entry.relative));
        }
    }
    for (name, script) in scripts {
        let body = format!("#!/bin/sh\n{env}exec {} \"$@\"\n", quote(&script)?);
        expected_files.push(write(
            &mut prefix,
            &Path::new("bin").join(name),
            body.as_bytes(),
            0o755,
        )?);
    }
    let storage = super::tree::storage_with_inventory(
        &prefix,
        key,
        projection::verify_output(&prefix, &expected_files, projection.as_ref())?,
    )?;
    let value = LayoutManifest {
        language: language.key().fingerprint(),
        graph,
        packages: packages.iter().map(|p| p.manifest().facts.key).collect(),
        storage,
        recipe: 2,
        projection,
    };
    language.check_binding()?;
    for p in &packages {
        p.check_binding()?;
    }
    ensure!(
        projection::verify_output(&prefix, &expected_files, value.projection.as_ref())?
            == value.storage.inventory,
        "verified layout output changed before publication"
    );
    let result = prefix.publish_shared(
        &manifest::layout_name(&key),
        &serde_json::to_vec(&value)?,
        &value.storage.inventory,
    );
    // Keep every package SH through canonical layout reacquisition.
    match result {
        Ok(guard) => drop(guard),
        Err(error) => {
            if !error.is::<crate::packages::python_environment::prefix::SharedPublicationConflict>()
            {
                return Err(error);
            }
            let winner = acquire_layout(context, key)?
                .context("shared layout publication failed without a winner")?;
            ensure!(
                winner.graph() == graph && winner.language() == language.key().fingerprint(),
                "conflicting shared layout publication: {error:#}"
            );
            return Ok(winner);
        }
    }
    let lease = acquire_layout(context, key)?.context("published shared layout is missing")?;
    drop(packages);
    Ok(lease)
}

/// Read-only legacy lookup. Reserving a name cannot manufacture Ready facts.
pub(crate) fn reserve_layout(
    context: &OperationContext,
    language: [u8; 32],
    graph: [u8; 32],
) -> Result<LayoutLease> {
    if let Some(layout) = acquire_layout(context, layout_key(language, graph))? {
        return Ok(layout);
    }
    // Read-only legacy lookup may retain an existing v1 selection. Writers
    // always prepare the v2 recipe and never treat this fallback as a build.
    acquire_layout(context, layout_key_for(1, language, graph)?)?.context(
        "shared layout is missing; prepare it with the selected language and verified wheels",
    )
}
#[cfg(test)]
pub(crate) mod tests;
