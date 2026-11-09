//! Immutable Conda package source identities and publication.

use super::lease::{
    LayoutLease, PackageLease, acquire_layout, acquire_layout_with_held_inputs, acquire_package,
};
use super::manifest::{self, LayoutManifest, PackageFacts, PackageManifest};
use super::tree::storage_with_inventory;
use crate::artifact_snapshot::{VerifiedArtifact, spool_verified_reader};
use crate::libraries::language::lease::LanguageLease;
use crate::packages::conda_artifact::CondaTree;
use crate::packages::python_environment::prefix::verify::{
    PrefixExpectedDirectory, PrefixExpectedFile, PrefixExpectedSymlink, PrefixInventoryLimits,
    verify_layout_inventory, verify_prefix_inventory,
};
use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::Digest;
use rattler_conda_types::{MatchSpec, ParseStrictness, RepoDataRecord};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::str::FromStr;
use unicode_normalization::UnicodeNormalization;

const ARCHIVE: &str = "conda-archive";

/// Bind all selected metadata, including the actual source channel and URL.
pub(crate) fn record_source(record: &RepoDataRecord) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"pakro.conda-package-source.v1\0");
    hash.update(serde_json::to_vec(record)?);
    Ok(hex::encode(hash.finalize()))
}

/// Identity of the unmodified artifact and its selected dependency closure.
pub(crate) fn package_key(record: &RepoDataRecord, records: &[RepoDataRecord]) -> Result<[u8; 32]> {
    let by_name: BTreeMap<_, _> = records
        .iter()
        .map(|entry| (entry.package_record.name.as_normalized(), entry))
        .collect();
    let mut pending = vec![record];
    let mut reached = BTreeSet::new();
    while let Some(entry) = pending.pop() {
        if !reached.insert(entry.package_record.name.as_normalized()) {
            continue;
        }
        for dependency in &entry.package_record.depends {
            let spec = MatchSpec::from_str(dependency, ParseStrictness::Lenient)?;
            let name = spec
                .name
                .as_exact()
                .ok_or_else(|| eyre::eyre!("Conda dependency name is not exact"))?
                .as_normalized();
            if !name.starts_with("__") {
                pending.push(*by_name.get(name).ok_or_else(|| {
                    eyre::eyre!("Conda dependency {name} is missing from the resolved graph")
                })?);
            }
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"pakro.conda-package.v1\0");
    hash.update(record_source(record)?.as_bytes());
    for name in reached {
        hash.update(record_source(by_name[name])?.as_bytes());
    }
    Ok(hash.finalize().into())
}

/// Publish the unmodified verified archive under the existing package store.
/// Dependency and optional language leases remain held until Ready publication.
pub(crate) fn publish_conda_package(
    context: &OperationContext,
    artifact: &mut VerifiedArtifact,
    tree: &CondaTree,
    records: &[RepoDataRecord],
    dependencies: &[PackageLease],
    language: Option<&LanguageLease>,
) -> Result<PackageLease> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "package publication is not allowed"
    );
    let record = tree.record();
    let digest: [u8; 32] = record
        .package_record
        .sha256
        .context("Conda archive lacks SHA256")?
        .into();
    let size = record
        .package_record
        .size
        .context("Conda archive lacks size")?;
    ensure!(
        artifact.filename() == record.identifier.to_string()
            && artifact.digest() == Digest::Sha256(digest)
            && artifact.size() == size,
        "verified Conda artifact differs from its extracted record"
    );
    let mut dependency_keys = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        dependency.ensure_library_context(context)?;
        dependency.check_binding()?;
        dependency_keys.push(dependency.manifest().key());
    }
    dependency_keys.sort_unstable();
    dependency_keys.dedup();
    let mut required = Vec::new();
    let mut requires_python = false;
    for dependency in &record.package_record.depends {
        let spec = MatchSpec::from_str(dependency, ParseStrictness::Lenient)?;
        let name = spec
            .name
            .as_exact()
            .context("Conda dependency name is not exact")?
            .as_normalized();
        if name.starts_with("__") {
            continue;
        }
        let selected = records
            .iter()
            .find(|entry| entry.package_record.name.as_normalized() == name)
            .ok_or_else(|| {
                eyre::eyre!("Conda dependency {name} is missing from the resolved graph")
            })?;
        if name == "python" {
            requires_python = true;
            let selected_plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(selected)?;
            ensure!(
                language.is_some_and(
                    |lease| lease.spec().plan().conda_record() == selected_plan.conda_record()
                ),
                "Conda package requires its selected Python before publication"
            );
        } else {
            required.push(package_key(selected, records)?);
        }
    }
    required.sort_unstable();
    required.dedup();
    ensure!(
        required == dependency_keys,
        "Conda package dependencies differ from the selected graph"
    );
    ensure!(
        requires_python == language.is_some(),
        "Conda package has a missing or unexpected Python dependency"
    );
    if let Some(language) = language {
        language.ensure_library_context(context)?;
        language.check_binding()?;
    }
    let language_key = language.map(|language| language.key().fingerprint());
    let facts = PackageFacts {
        key: package_key(record, records)?,
        name: record.package_record.name.as_normalized().to_owned(),
        version: record.package_record.version.to_string(),
        artifact: artifact.digest().to_string(),
        source: record_source(record)?,
        dependencies: dependency_keys,
    };
    let target = if language_key.is_none() {
        Some(record.package_record.subdir.clone())
    } else {
        None
    };
    if let Some(existing) = acquire_package(context, facts.key)? {
        ensure!(
            existing.manifest().facts == facts
                && existing.manifest().language == language_key
                && existing.manifest().target == target
                && existing.manifest().filename == artifact.filename(),
            "existing Conda package differs from its locked source or dependencies"
        );
        return Ok(existing);
    }
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    artifact.seek(SeekFrom::Start(0))?;
    {
        let mut output = prefix.create_file_exclusive(Path::new(ARCHIVE), 0o600)?;
        let mut buffer = [0u8; 65536];
        loop {
            context.checkpoint()?;
            let count = artifact.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
        }
        output.finish()?;
    }
    let facts_bytes = manifest::package_facts_bytes(&facts, &language_key, artifact.filename())?;
    {
        let mut output =
            prefix.create_file_exclusive(Path::new(manifest::PACKAGE_FACTS_FILE), 0o600)?;
        output.write_all(&facts_bytes)?;
        output.finish()?;
    }
    let expected = [
        PrefixExpectedFile {
            relative: ARCHIVE.into(),
            sha256: digest,
            size,
            mode: 0o600,
        },
        PrefixExpectedFile {
            relative: manifest::PACKAGE_FACTS_FILE.into(),
            sha256: Sha256::digest(&facts_bytes).into(),
            size: facts_bytes.len() as u64,
            mode: 0o600,
        },
    ];
    let inventory =
        verify_prefix_inventory(&prefix, &expected, &[], PrefixInventoryLimits::default())?
            .into_tree_inventory();
    let storage = storage_with_inventory(&prefix, facts.key, inventory)?;
    let manifest = PackageManifest {
        facts: facts.clone(),
        language: language_key,
        target,
        conda: Some(record.clone()),
        filename: artifact.filename().into(),
        storage,
    };
    for dependency in dependencies {
        dependency.check_binding()?;
    }
    if let Some(language) = language {
        language.check_binding()?;
    }
    match prefix.publish_shared(
        &manifest::package_name(&facts.key),
        &serde_json::to_vec(&manifest)?,
        &manifest.storage.inventory,
    ) {
        Ok(guard) => drop(guard),
        Err(error)
            if error
                .is::<crate::packages::python_environment::prefix::SharedPublicationConflict>() => {
        }
        Err(error) => return Err(error),
    }
    let lease =
        acquire_package(context, facts.key)?.context("published Conda package is missing")?;
    ensure!(
        lease.manifest().facts == facts
            && lease.manifest().language == language_key
            && lease.manifest().target == manifest.target
            && lease.manifest().filename == artifact.filename(),
        "published Conda package differs from the locked source or dependencies"
    );
    for dependency in dependencies {
        dependency.check_binding()?;
    }
    if let Some(language) = language {
        language.check_binding()?;
    }
    Ok(lease)
}

/// Recreate a private verified artifact from the held package without network.
pub(crate) fn held_conda_artifact(
    context: &OperationContext,
    package: &PackageLease,
) -> Result<VerifiedArtifact> {
    package.ensure_library_context(context)?;
    package.check_binding()?;
    let artifact = spool_verified_reader(
        package.open_inventory_file(Path::new(ARCHIVE))?,
        &package.manifest().filename,
        package.manifest().artifact()?,
        1024 * 1024 * 1024,
        context.requested_cache_root(),
    )?;
    package.check_binding()?;
    Ok(artifact)
}

pub(super) fn layout_member_matches(language: &LanguageLease, package: &PackageManifest) -> bool {
    let Some(record) = &package.conda else {
        return package.language == Some(language.key().fingerprint());
    };
    let expected = match (
        language.descriptor().markers.platform_system.as_str(),
        language.descriptor().markers.platform_machine.as_str(),
    ) {
        ("Darwin", "arm64") => "osx-arm64",
        ("Darwin", "x86_64") => "osx-64",
        ("Linux", "aarch64") => "linux-aarch64",
        ("Linux", "x86_64") => "linux-64",
        _ => return false,
    };
    (record.package_record.subdir == "noarch" || record.package_record.subdir == expected)
        && package
            .language
            .is_none_or(|key| key == language.key().fingerprint())
}

pub(crate) fn acquire_conda_layout(
    context: &OperationContext,
    language: &LanguageLease,
    graph: [u8; 32],
) -> Result<Option<LayoutLease>> {
    language.ensure_library_context(context)?;
    for recipe in [6, 5] {
        let key = super::layouts::layout_key_for(recipe, language.key().fingerprint(), graph)?;
        if let Some(layout) = acquire_layout(context, key)? {
            return Ok(Some(layout));
        }
    }
    Ok(None)
}

fn reserve_layout_path(
    paths: &mut BTreeMap<String, (std::path::PathBuf, bool)>,
    path: &Path,
) -> Result<()> {
    reserve_layout_entry(paths, path, false)
}

fn reserve_layout_entry(
    paths: &mut BTreeMap<String, (std::path::PathBuf, bool)>,
    path: &Path,
    directory: bool,
) -> Result<()> {
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        let text = ancestor
            .to_str()
            .context("Conda layout path is not UTF-8")?;
        let folded: String = text.nfkc().flat_map(char::to_lowercase).collect();
        let is_directory = directory || ancestor != path;
        if let Some((previous, previous_directory)) = paths.get(&folded) {
            ensure!(
                previous == ancestor && *previous_directory && is_directory,
                "Conda layout paths collide: {}",
                path.display()
            );
        } else {
            paths.insert(folded, (ancestor.to_owned(), is_directory));
        }
    }
    Ok(())
}

fn copy_layout_file(
    context: &OperationContext,
    prefix: &mut crate::packages::python_environment::prefix::UnpublishedPrefix,
    relative: &Path,
    mut input: impl Read,
    mode: u32,
) -> Result<PrefixExpectedFile> {
    if let Some(parent) = relative.parent() {
        prefix.create_dir_all(parent)?;
    }
    let mut output = prefix.create_file_exclusive(relative, mode)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        context.checkpoint()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    output.finish()?;
    Ok(PrefixExpectedFile {
        relative: relative.into(),
        sha256: hash.finalize().into(),
        size,
        mode,
    })
}

fn populate_script_stage(
    context: &OperationContext,
    prefix: &crate::packages::python_environment::prefix::UnpublishedPrefix,
    inventory: &crate::packages::python_environment::prefix::verify::TreeInventory,
    target: &Path,
) -> Result<()> {
    use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
    prefix.check_binding()?;
    for entry in inventory.entries() {
        context.checkpoint()?;
        let destination = target.join(&entry.relative);
        match &entry.kind {
            TreeInventoryKind::Directory => std::fs::create_dir(&destination)?,
            TreeInventoryKind::Symlink { target } => {
                std::os::unix::fs::symlink(target, &destination)?
            }
            TreeInventoryKind::File { size, sha256 } => {
                let source = prefix.final_path().join(&entry.relative);
                ensure!(
                    std::fs::copy(&source, &destination)? == *size,
                    "Conda script input size changed"
                );
                let mut input = std::fs::File::open(&destination)?;
                let mut digest = Sha256::new();
                let mut bytes = [0u8; 65536];
                loop {
                    let count = input.read(&mut bytes)?;
                    if count == 0 {
                        break;
                    }
                    digest.update(&bytes[..count]);
                }
                ensure!(
                    <[u8; 32]>::from(digest.finalize()) == *sha256,
                    "Conda script input changed"
                );
            }
        }
    }
    prefix.check_binding()
}

/// Build the shared startup layout from selected, held Conda package sources.
pub(crate) async fn prepare_conda_layout(
    context: &OperationContext,
    language: &LanguageLease,
    graph: [u8; 32],
    packages: &[PackageLease],
    projected: &[(&PackageLease, &CondaTree)],
) -> Result<LayoutLease> {
    use rattler_conda_types::package::{LinkJson, NoArchLinks, PackageFile, PathType};
    use rattler_conda_types::{Platform, prefix::Prefix};
    use std::os::unix::fs::PermissionsExt;

    context.checkpoint()?;
    language.ensure_library_context(context)?;
    language.check_binding()?;
    ensure!(
        !packages.is_empty(),
        "Conda layout requires its selected packages"
    );
    let mut held = packages.to_vec();
    held.sort_by_key(|package| package.manifest().key());
    for pair in held.windows(2) {
        ensure!(
            pair[0].manifest().key() != pair[1].manifest().key(),
            "duplicate Conda layout package"
        );
    }
    for package in &held {
        package.ensure_library_context(context)?;
        package.check_binding()?;
        ensure!(
            layout_member_matches(language, package.manifest()),
            "Conda package target differs from its selected Python"
        );
    }
    let key = super::layouts::layout_key_for(6, language.key().fingerprint(), graph)?;
    if let Some(existing) = acquire_layout_with_held_inputs(context, key, language, &held)? {
        return Ok(existing);
    }
    ensure!(
        context.policy().writes.libraries,
        "Conda layout publication is not allowed"
    );
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    let scripted: Vec<_> = projected
        .iter()
        .filter(|(_, tree)| tree.post_link().is_some())
        .collect();
    let script_names = projected.iter().flat_map(|(_, tree)| {
        let name = tree.record().package_record.name.as_normalized();
        tree.activation_scripts().into_iter().chain(tree.post_link())
            .map(move |path| format!("{name}:{}", path.display()))
    }).collect::<Vec<_>>().join(", ");
    let mut activation = Vec::new();
    for (_, tree) in projected {
        let scripts = tree.activation_scripts();
        if scripts.is_empty() {
            continue;
        }
        let active = scripts
            .iter()
            .filter(|path| {
                path.starts_with("etc/conda/activate.d")
                    && path.extension().is_some_and(|ext| ext == "sh")
            })
            .copied()
            .collect::<Vec<_>>();
        let inactive = scripts
            .iter()
            .filter(|path| {
                path.starts_with("etc/conda/deactivate.d")
                    && path.extension().is_some_and(|ext| ext == "sh")
            })
            .copied()
            .collect::<Vec<_>>();
        ensure!(
            active.len() == 1 && inactive.len() == 1,
            "Conda package {} scripts [{}]: expected one .sh activation and one .sh deactivation script; found {} activation and {} deactivation scripts",
            tree.record().package_record.name.as_normalized(),
            scripts.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "),
            active.len(), inactive.len()
        );
        let name = tree.record().package_record.name.as_normalized();
        if scripts.iter().any(|path| path.extension().is_some_and(|ext| ext == "fish" || ext == "csh")) {
            log::warn!("包 {name} 的 fish/csh 版本脚本不会执行");
        }
        activation.push((name, active[0], inactive[0]));
    }
    ensure!(
        activation.len() <= 1,
        "Conda scripts [{script_names}]: only one package with activation/deactivation scripts is supported per project"
    );
    ensure!(
        activation.is_empty() || scripted.is_empty(),
        "Conda scripts [{script_names}]: activation/deactivation cannot be combined with post-link"
    );
    ensure!(
        scripted.is_empty() || (scripted.len() == 1 && projected.len() == 1),
        "Conda scripts [{script_names}]: post-link requires one independent project package; found {} project packages",
        projected.len()
    );
    if let Some((_, tree)) = scripted.first() {
        ensure!(
            held.iter()
                .all(|package| package.manifest().conda.is_some()),
            "Conda package {} script {}: post-link with PyPI packages is not supported",
            tree.record().package_record.name.as_normalized(), tree.post_link().unwrap().display()
        );
        for dependency in &tree.record().package_record.depends {
            let spec = MatchSpec::from_str(dependency, ParseStrictness::Lenient)?;
            ensure!(
                spec.name
                    .as_exact()
                    .is_some_and(|name| name.as_normalized().starts_with("__")),
                "Conda package {} script {}: post-link dependency {dependency} is not supported; only virtual platform requirements are allowed",
                tree.record().package_record.name.as_normalized(), tree.post_link().unwrap().display()
            );
        }
    }
    let mut scripted_destination = if scripted.is_empty() && activation.is_empty() {
        None
    } else {
        Some(store.allocate()?)
    };
    let final_prefix = scripted_destination.as_ref().map_or_else(
        || prefix.final_path().to_owned(),
        |final_owner| final_owner.final_path().to_owned(),
    );
    let mut expected = Vec::new();
    let mut expected_links = Vec::new();
    let mut expected_directories = BTreeSet::new();
    let mut used = BTreeMap::new();
    let mut projected_keys = BTreeSet::new();
    let platform_site = std::path::PathBuf::from(format!(
        "lib/python{}/site-packages",
        language.descriptor().markers.python_version
    ));
    for (package, tree) in projected {
        ensure!(
            held.iter()
                .any(|held| held.manifest().key() == package.manifest().key())
                && projected_keys.insert(package.manifest().key())
                && package.manifest().conda.as_ref() == Some(tree.record()),
            "Conda projection differs from its held package"
        );
        let noarch = tree.record().package_record.noarch.is_python();
        let platform = Platform::current().context("unsupported Conda layout platform")?;
        let scratch = tempfile::Builder::new()
            .prefix("conda-layout-link-")
            .tempdir_in(context.requested_cache_root())?;
        let linked = Prefix::create(scratch.path().join("prefix"))?;
        let target_prefix = final_prefix
            .to_str()
            .context("Conda layout path is not UTF-8")?;
        for entry in &tree.paths().paths {
            ensure!(
                !noarch || entry.relative_path.starts_with("site-packages"),
                "Conda noarch scripts or non-site-packages members are not supported yet"
            );
            let relative = &entry.relative_path;
            let logical_path = relative
                .strip_prefix(&platform_site)
                .map(|suffix| Path::new("site-packages").join(suffix))
                .unwrap_or_else(|_| relative.to_owned());
            if entry.path_type == PathType::Directory {
                reserve_layout_entry(&mut used, &logical_path, true)?;
                prefix.create_dir_all(relative)?;
                expected_directories.insert(relative.to_owned());
                continue;
            }
            reserve_layout_path(&mut used, &logical_path)?;
            let destination = linked.path().join(relative);
            std::fs::create_dir_all(
                destination
                    .parent()
                    .context("Conda link parent is missing")?,
            )?;
            let mut link_entry = entry.clone();
            if platform.is_osx()
                && link_entry.sha256.is_none()
                && link_entry
                    .prefix_placeholder
                    .as_ref()
                    .is_some_and(|placeholder| {
                        placeholder.file_mode == rattler_conda_types::package::FileMode::Binary
                    })
            {
                let mut file = std::fs::File::open(tree.root().join(relative))?;
                let mut hash = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    context.checkpoint()?;
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
                link_entry.sha256 = Some(hash.finalize());
            }
            ensure!(
                rattler::install::link_file(
                    &link_entry,
                    relative.to_owned(),
                    tree.root(),
                    &linked,
                    target_prefix,
                    true,
                    false,
                    false,
                    platform,
                    rattler::install::AppleCodeSignBehavior::Fail,
                    filetime::FileTime::from_unix_time(0, 0),
                    rattler::install::ExternalSymlinkPolicy::Deny,
                )?
                .is_some(),
                "Conda layout member disappeared during linking"
            );
            let metadata = std::fs::symlink_metadata(&destination)?;
            if metadata.file_type().is_symlink() {
                ensure!(
                    entry.path_type == PathType::SoftLink,
                    "Conda layout member type changed"
                );
                let target = std::fs::read_link(&destination)?;
                if let Some(parent) = relative.parent() {
                    prefix.create_dir_all(parent)?;
                }
                prefix.create_layout_symlink(relative, &target)?;
                expected_links.push(PrefixExpectedSymlink {
                    relative: relative.to_owned(),
                    target,
                });
                continue;
            }
            ensure!(
                entry.path_type == PathType::HardLink && metadata.is_file(),
                "Conda layout member is not a regular file"
            );
            let mode = if metadata.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            expected.push(copy_layout_file(
                context,
                &mut prefix,
                relative,
                std::fs::File::open(destination)?,
                mode,
            )?);
        }
        let link_path = tree.root().join("info/link.json");
        if link_path.exists() {
            let link = LinkJson::from_package_directory(tree.root())?;
            if let NoArchLinks::Python(python) = link.noarch {
                for entry in python.entry_points {
                    let path = Path::new("bin").join(&entry.command);
                    reserve_layout_path(&mut used, &path)?;
                    let module = serde_json::to_string(&entry.module)?;
                    let function = serde_json::to_string(&entry.function)?;
                    let code = format!(
                        "import importlib,sys,functools; sys.exit(functools.reduce(getattr, {function}.split('.'), importlib.import_module({module}))())"
                    );
                    let script = format!(
                        "#!/bin/sh\nexec {} -c {} \"$@\"\n",
                        super::layouts::quote(&final_prefix.join("bin/python"))?,
                        super::layouts::quote(Path::new(&code))?
                    );
                    expected.push(super::layouts::write(
                        &mut prefix,
                        &path,
                        script.as_bytes(),
                        0o755,
                    )?);
                }
            }
        }
    }
    for package in held
        .iter()
        .filter(|package| package.manifest().conda.is_none())
    {
        projected_keys.insert(package.manifest().key());
        for entry in package.manifest().storage.inventory.entries() {
            use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
            if matches!(entry.kind, TreeInventoryKind::Directory) {
                continue;
            }
            if entry.relative == Path::new("bin/.pakro python")
                || entry.relative == Path::new(manifest::PACKAGE_FACTS_FILE)
            {
                continue;
            }
            ensure!(
                entry.relative.starts_with("site-packages")
                    && matches!(entry.kind, TreeInventoryKind::File { .. }),
                "wheel scripts and non-site-packages members are not supported in Conda layouts yet"
            );
            reserve_layout_path(&mut used, &entry.relative)?;
            let copied = copy_layout_file(
                context,
                &mut prefix,
                &entry.relative,
                package.open_inventory_file(&entry.relative)?,
                entry.mode,
            )?;
            let TreeInventoryKind::File { size, sha256 } = &entry.kind else {
                unreachable!()
            };
            ensure!(
                copied.size == *size && copied.sha256 == *sha256,
                "wheel layout file differs from its held package"
            );
            expected.push(copied);
        }
    }
    for package in &held {
        ensure!(
            language
                .runtime_dependencies()
                .contains(&package.manifest().key())
                || projected_keys.contains(&package.manifest().key()),
            "selected Conda package is not present in the runtime or project layout"
        );
    }
    let bootstrap = format!(
        "import site\nsite.addsitedir({})\nsite.addsitedir({})\n",
        serde_json::to_string(
            final_prefix
                .join("site-packages")
                .to_str()
                .context("Conda layout path is not UTF-8")?
        )?,
        serde_json::to_string(
            final_prefix
                .join("lib")
                .join(format!(
                    "python{}",
                    language.descriptor().markers.python_version
                ))
                .join("site-packages")
                .to_str()
                .context("Conda platform site path is not UTF-8")?
        )?
    );
    let path = Path::new("bootstrap/sitecustomize.py");
    reserve_layout_path(&mut used, path)?;
    expected.push(super::layouts::write(
        &mut prefix,
        path,
        bootstrap.as_bytes(),
        0o644,
    )?);
    let env = super::layouts::startup_environment(&final_prefix)?;
    let interpreter = format!(
        "#!/bin/sh\n{env}exec {} \"$@\"\n",
        super::layouts::quote(language.entrypoint())?
    );
    for name in [
        "python".to_owned(),
        "python3".to_owned(),
        format!("python{}", language.descriptor().markers.python_version),
    ] {
        let path = Path::new("bin").join(name);
        reserve_layout_path(&mut used, &path)?;
        expected.push(super::layouts::write(
            &mut prefix,
            &path,
            interpreter.as_bytes(),
            0o755,
        )?);
    }
    for path in expected
        .iter()
        .map(|file| file.relative.as_path())
        .chain(expected_links.iter().map(|link| link.relative.as_path()))
        .chain(expected_directories.clone().iter().map(|dir| dir.as_path()))
    {
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|directory| !directory.as_os_str().is_empty()) {
            expected_directories.insert(directory.to_owned());
            parent = directory.parent();
        }
    }
    let directories: Vec<_> = expected_directories
        .into_iter()
        .map(|relative| PrefixExpectedDirectory {
            relative,
            mode: 0o755,
        })
        .collect();
    let mut inventory = verify_layout_inventory(
        &prefix,
        &expected,
        &directories,
        &expected_links,
        PrefixInventoryLimits::default(),
    )?
    .into_tree_inventory();
    if !scripted.is_empty() || !activation.is_empty() {
        let staged = if let Some((_, tree)) = scripted.first() {
            crate::packages::conda_scripts::run_post_link(context, tree.record(), |target| {
                populate_script_stage(context, &prefix, &inventory, target)
            })
            .await?
        } else {
            let (package, active, inactive) = activation[0];
            crate::packages::conda_scripts::run_activation(context, package, active, inactive, |target| {
                populate_script_stage(context, &prefix, &inventory, target)
            })
            .await?
        };
        let mut destination = scripted_destination
            .take()
            .expect("scripted destination reserved");
        inventory = staged.import_layout(context, &mut destination)?;
        if let Some(plan) = staged.activation() {
            let path = Path::new("bootstrap/.pakro-conda-activation.json");
            reserve_layout_path(&mut used, path)?;
            expected.push(super::layouts::write(
                &mut destination,
                path,
                &serde_json::to_vec(plan)?,
                0o644,
            )?);
            inventory = verify_layout_inventory(
                &destination,
                &expected,
                &directories,
                &expected_links,
                PrefixInventoryLimits::default(),
            )?
            .into_tree_inventory();
        }
        prefix = destination;
    }
    let storage = storage_with_inventory(&prefix, key, inventory)?;
    let value = LayoutManifest {
        language: language.key().fingerprint(),
        graph,
        packages: held
            .iter()
            .map(|package| package.manifest().key())
            .collect(),
        storage,
        recipe: 6,
        activation: !activation.is_empty(),
        projection: None,
    };
    language.check_binding()?;
    for package in &held {
        package.check_binding()?;
    }
    match prefix.publish_shared(
        &manifest::layout_name(&key),
        &serde_json::to_vec(&value)?,
        &value.storage.inventory,
    ) {
        Ok(guard) => drop(guard),
        Err(error)
            if error
                .is::<crate::packages::python_environment::prefix::SharedPublicationConflict>() => {
        }
        Err(error) => return Err(error),
    }
    acquire_layout_with_held_inputs(context, key, language, &held)?
        .context("published Conda layout is missing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{PackageName, PackageRecord, Version};

    fn record(name: &str, version: &str, dependencies: &[&str]) -> RepoDataRecord {
        let mut facts = PackageRecord::new(
            PackageName::from_str(name).unwrap(),
            Version::from_str(version).unwrap(),
            "a_0".into(),
        );
        facts.depends = dependencies.iter().map(|value| (*value).into()).collect();
        RepoDataRecord {
            package_record: facts,
            identifier: DistArchiveIdentifier::new(
                ArchiveIdentifier {
                    name: name.into(),
                    version: version.into(),
                    build_string: "a_0".into(),
                },
                DistArchiveType::Conda(CondaArchiveType::Conda),
            ),
            url: format!("https://example.invalid/noarch/{name}-{version}-a_0.conda")
                .parse()
                .unwrap(),
            channel: Some("https://example.invalid/".into()),
        }
    }

    #[test]
    fn package_identity_uses_only_its_selected_dependency_closure() {
        let root = record("example", "1.0", &["dep >=1"]);
        let dep = record("dep", "1.0", &[]);
        let unrelated = record("other", "1.0", &[]);
        let key = package_key(&root, &[root.clone(), dep.clone()]).unwrap();
        assert_eq!(
            key,
            package_key(&root, &[root.clone(), dep, unrelated]).unwrap()
        );
        assert_ne!(
            key,
            package_key(&root, &[root.clone(), record("dep", "2.0", &[])]).unwrap()
        );
        assert!(package_key(&root, &[root.clone()]).is_err());
    }

    #[test]
    fn layout_paths_reject_case_and_unicode_collisions() {
        let mut paths = BTreeMap::new();
        reserve_layout_path(&mut paths, Path::new("site-packages/Example/file.py")).unwrap();
        assert!(
            reserve_layout_path(&mut paths, Path::new("site-packages/example/other.py")).is_err()
        );
        assert!(
            reserve_layout_path(&mut paths, Path::new("site-packages/Example/file.py")).is_err()
        );
    }
}
