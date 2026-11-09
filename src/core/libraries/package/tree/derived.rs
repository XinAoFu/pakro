//! A new owned projection of a verified Ready tree for changed graph facts.
//! Original wheel bytes are neither reconstructed nor assigned a new digest.
use super::*;
use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
use crate::packages::python_wheel_scripts::{
    PythonScriptTarget, WheelScriptLimits, WheelScriptOperation, generate_entry_point,
    parse_entry_points, rebase_installed_python_script,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

fn write_bytes(
    prefix: &mut UnpublishedPrefix,
    relative: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<PrefixExpectedFile> {
    let mut output = prefix.create_file_exclusive(relative, mode)?;
    output.write_all(bytes)?;
    output.finish()?;
    Ok(PrefixExpectedFile {
        relative: relative.into(),
        sha256: Sha256::digest(bytes).into(),
        size: bytes.len() as u64,
        mode,
    })
}
fn read_small(source: &PackageLease, path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let mut file = source.open_inventory_file(path)?;
    ensure!(
        file.metadata()?.len() <= maximum as u64,
        "derived metadata exceeds its byte budget"
    );
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= maximum,
        "derived metadata exceeds its byte budget"
    );
    Ok(bytes)
}
fn copy_file(
    context: &OperationContext,
    prefix: &mut UnpublishedPrefix,
    source: &mut File,
    expected: &PrefixExpectedFile,
) -> Result<()> {
    context.checkpoint()?;
    #[cfg(target_os = "macos")]
    {
        prefix.clone_file_exclusive(source, expected)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        prefix.copy_file_exclusive(source, expected)?;
    }
    context.checkpoint()?;
    Ok(())
}
// Relocated script bytes are newly generated, held scratch content, rather
// than an inventoried immutable package file with its final output mode.
fn copy_stream(
    context: &OperationContext,
    prefix: &mut UnpublishedPrefix,
    source: &mut File,
    expected: &PrefixExpectedFile,
) -> Result<()> {
    source.rewind()?;
    let mut output = prefix.create_file_exclusive(&expected.relative, expected.mode)?;
    let mut size = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        context.checkpoint()?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .context("derived file size overflow")?;
        ensure!(size <= expected.size, "derived source file grew");
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    ensure!(
        size == expected.size && <[u8; 32]>::from(digest.finalize()) == expected.sha256,
        "derived source bytes differ from their complete inventory"
    );
    output.finish()?;
    context.checkpoint()?;
    Ok(())
}
fn contains_prefix(context: &OperationContext, file: &mut File, prefix: &[u8]) -> Result<bool> {
    ensure!(!prefix.is_empty(), "old package allocation path is empty");
    file.seek(SeekFrom::Start(0))?;
    let mut tail = Vec::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        context.checkpoint()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(false);
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.windows(prefix.len()).any(|window| window == prefix) {
            return Ok(true);
        }
        let keep = prefix.len().saturating_sub(1).min(tail.len());
        tail.drain(..tail.len() - keep);
    }
}

pub(super) fn prepare(
    context: &OperationContext,
    scope: &LanguageMaterialization<'_>,
    facts: PackageFacts,
    source: PackageLease,
) -> Result<PendingPackageTree> {
    context.checkpoint()?;
    scope.check_binding()?;
    source.check_binding()?;
    let original = source.manifest();
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids derived package publication"
    );
    ensure!(
        original.language == Some(scope.language().key().fingerprint())
            && original.facts.name == facts.name
            && original.facts.version == facts.version
            && original.facts.source == facts.source
            && original.facts.artifact == facts.artifact,
        "derived package may only change its graph key and dependency references"
    );
    ensure!(
        original.facts.key != facts.key
            && facts.dependencies.len() <= 65536
            && facts.dependencies.windows(2).all(|pair| pair[0] < pair[1]),
        "derived package has conflicting identity or noncanonical dependency facts"
    );
    let limits = PrefixInventoryLimits::default();
    let inventory = &original.storage.inventory;
    ensure!(
        inventory.entries().len() <= limits.max_files + limits.max_directories
            && inventory.total_bytes() <= limits.max_total_bytes,
        "derived package inventory budget exceeded"
    );
    let executable = scope
        .language()
        .entrypoint()
        .to_str()
        .context("derived language executable is not UTF-8")?;
    ensure!(
        !executable.contains(['\n', '\r', '\0']),
        "unsafe derived language executable"
    );
    let wrapper = format!(
        "#!/bin/sh\nexport PYTHONDONTWRITEBYTECODE=1\nexport PYTHONNOUSERSITE=1\nexec '{}' \"$@\"\n",
        executable.replace('\'', "'\\''")
    );
    let wrapper_entry = inventory
        .entries()
        .iter()
        .find(|entry| entry.relative == Path::new("bin/.pakro python"))
        .context("derived source is missing its known language wrapper")?;
    ensure!(
        wrapper_entry.mode == 0o755
            && matches!(&wrapper_entry.kind, TreeInventoryKind::File { size, sha256 }
        if *size == wrapper.len() as u64 && *sha256 == <[u8;32]>::from(Sha256::digest(wrapper.as_bytes()))),
        "derived source wrapper does not bind this actual held language location"
    );
    let metadata = inventory
        .entries()
        .iter()
        .filter(|entry| {
            entry.relative.parent().is_some_and(|parent| {
                parent.parent() == Some(Path::new("site-packages"))
                    && parent
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(".dist-info"))
            }) && entry
                .relative
                .file_name()
                .is_some_and(|name| name == "METADATA")
        })
        .collect::<Vec<_>>();
    ensure!(
        metadata.len() == 1,
        "derived package has no unique installed metadata owner"
    );
    let dist_info = metadata[0]
        .relative
        .parent()
        .context("installed metadata parent is missing")?;
    let record_path = dist_info.join("RECORD");
    let record_entry = inventory
        .entries()
        .iter()
        .find(|entry| entry.relative == record_path)
        .context("derived package has no installed RECORD")?;
    ensure!(
        matches!(record_entry.kind, TreeInventoryKind::File { .. }),
        "installed RECORD is not a regular file"
    );
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    let old_interpreter = source.root().join("bin/.pakro python");
    let new_interpreter = prefix.final_path().join("bin/.pakro python");
    let old_target = PythonScriptTarget {
        final_prefix: source.root(),
        interpreter: &old_interpreter,
    };
    let new_root = prefix.final_path().to_owned();
    let new_target = PythonScriptTarget {
        final_prefix: &new_root,
        interpreter: &new_interpreter,
    };
    let script_limits = WheelScriptLimits::default();
    let mut entry_scripts = BTreeMap::new();
    let entry_path = dist_info.join("entry_points.txt");
    if inventory.contains_file(&entry_path) {
        for entry in parse_entry_points(
            &read_small(&source, &entry_path, script_limits.max_entrypoints_bytes)?,
            script_limits,
        )? {
            let old = generate_entry_point(&entry, &old_target, script_limits)?;
            let new = generate_entry_point(&entry, &new_target, script_limits)?;
            let path = Path::new("bin").join(old.name());
            ensure!(
                entry_scripts.insert(path, (old, new)).is_none(),
                "derived package repeats an entry point"
            );
        }
    }
    let mut directories = Vec::new();
    for entry in inventory.entries() {
        context.checkpoint()?;
        ensure!(
            !matches!(entry.kind, TreeInventoryKind::Symlink { .. }),
            "derived package does not support relocating a link-bearing tree without an explicit link recipe"
        );
        if let TreeInventoryKind::Directory = entry.kind {
            ensure!(
                entry.mode == 0o755,
                "derived package has an unsupported directory mode"
            );
            prefix.create_dir_all(&entry.relative)?;
            directories.push(PrefixExpectedDirectory {
                relative: entry.relative.clone(),
                mode: entry.mode,
            });
        }
    }
    let mut files = Vec::new();
    for entry in inventory.entries() {
        context.checkpoint()?;
        let TreeInventoryKind::File { size, sha256 } = entry.kind else {
            continue;
        };
        if entry.relative == Path::new(manifest::PACKAGE_FACTS_FILE)
            || entry.relative == record_path
        {
            continue;
        }
        let expected = PrefixExpectedFile {
            relative: entry.relative.clone(),
            sha256,
            size,
            mode: entry.mode,
        };
        let mut input = source.open_inventory_file(&entry.relative)?;
        if let Some((old, new)) = entry_scripts.remove(&entry.relative) {
            ensure!(
                expected.sha256 == *old.sha256()
                    && expected.size == old.size()
                    && expected.mode == old.mode(),
                "installed entry point does not match its exact original generation recipe"
            );
            files.push(write_bytes(
                &mut prefix,
                &entry.relative,
                new.bytes(),
                new.mode(),
            )?);
        } else if entry.relative.parent() == Some(Path::new("bin"))
            && entry.relative != Path::new("bin/.pakro python")
        {
            let mut spool = tempfile::tempfile()?;
            let transform = rebase_installed_python_script(
                &mut input,
                &mut spool,
                &old_target,
                &new_target,
                entry.mode,
                script_limits,
            )?;
            ensure!(
                transform.raw_sha256() == &sha256 && transform.raw_size() == size,
                "installed script differs from its verified source inventory"
            );
            if transform.operation() == WheelScriptOperation::Copy {
                ensure!(
                    !contains_prefix(
                        context,
                        &mut input,
                        source.root().as_os_str().as_encoded_bytes()
                    )?,
                    "cannot relocate an unknown installed script that refers to its old package allocation"
                );
                copy_file(context, &mut prefix, &mut input, &expected)?;
                files.push(expected);
            } else {
                spool.rewind()?;
                let relocated = PrefixExpectedFile {
                    relative: entry.relative.clone(),
                    sha256: *transform.sha256(),
                    size: transform.size(),
                    mode: transform.mode(),
                };
                copy_stream(context, &mut prefix, &mut spool, &relocated)?;
                files.push(relocated);
            }
        } else {
            copy_file(context, &mut prefix, &mut input, &expected)?;
            files.push(expected);
        }
    }
    ensure!(
        entry_scripts.is_empty(),
        "installed entry point is absent from its complete inventory"
    );
    let record_files = files
        .iter()
        .filter(|file| file.relative != Path::new("bin/.pakro python"))
        .cloned()
        .collect::<Vec<_>>();
    let record = crate::packages::python_wheel_install::installed_record_from_verified_files(
        &record_path,
        &record_files,
        WheelInstallLimits::default(),
    )?;
    files.push(write_bytes(
        &mut prefix,
        &record_path,
        &record,
        record_entry.mode,
    )?);
    let facts_bytes =
        manifest::package_facts_bytes(&facts, &original.language, &original.filename)?;
    files.push(write_bytes(
        &mut prefix,
        Path::new(manifest::PACKAGE_FACTS_FILE),
        &facts_bytes,
        0o600,
    )?);
    source.check_binding()?;
    scope.check_binding()?;
    let checked =
        verify_prefix_inventory(&prefix, &files, &directories, limits)?.into_tree_inventory();
    let storage = storage_with_inventory(&prefix, facts.key, checked)?;
    let value = PackageManifest {
        facts,
        language: original.language,
        target: original.target.clone(),
        conda: original.conda.clone(),
        filename: original.filename.clone(),
        storage,
    };
    Ok(PendingPackageTree {
        prefix,
        manifest: value,
        proof: PackageTreeProof::Derived {
            source,
            language: scope.language().clone(),
            files,
            directories,
        },
    })
}
