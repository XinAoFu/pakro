//! One install-compatible package tree per exact instance, shared by layouts.
use super::lease::{PackageLease, acquire_package};
use super::manifest::{self, PackageFacts, PackageManifest, StorageManifest};
use crate::libraries::language::lease::{LanguageLease, LanguageMaterialization};
use crate::packages::python_environment::prefix::verify::{
    PrefixExpectedDirectory, PrefixExpectedFile, PrefixInventoryLimits, TreeInventory,
    observe_tree_inventory, verify_prefix_inventory,
};
use crate::packages::python_environment::prefix::{
    EnvironmentStore, PrefixLimits, UnpublishedPrefix,
};
use crate::packages::python_wheel::{WheelLimits, WheelScheme};
use crate::packages::python_wheel_install::{
    WheelInstallLimits, WheelInstallTarget, WheelMaterializationReceipt, materialize_wheel_graph,
    prepare_wheel_graph,
};
use crate::packages::python_wheel_plan::{
    WheelInstallScheme, WheelPlanLimits, WheelReservedDestination, WheelReservedKind,
};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    // One-shot fault injection in unit builds only; no production callback API.
    pub(super) static AFTER_WHEEL_COPY: std::cell::Cell<Option<fn(&UnpublishedPrefix)>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn storage_for(prefix: &UnpublishedPrefix, key: [u8; 32]) -> Result<StorageManifest> {
    storage_with_inventory(
        prefix,
        key,
        observe_tree_inventory(prefix, PrefixInventoryLimits::default())?,
    )
}
pub(super) fn storage_with_inventory(
    prefix: &UnpublishedPrefix,
    key: [u8; 32],
    inventory: TreeInventory,
) -> Result<StorageManifest> {
    let facts = prefix.generation_identity()?.facts()?;
    Ok(StorageManifest {
        schema: 1,
        key,
        allocation: prefix.allocation_id().into(),
        lock: facts.lock,
        root_identity: facts.generation,
        inventory,
    })
}

/// Reuse the final verifier's no-follow, full-byte and owned-inode checks.
/// Expectations originate in the retained C4 receipt and the exact generated
/// adapter/facts bytes, never in a new observation of possibly changed files.
fn verify_materialized_tree(
    prefix: &UnpublishedPrefix,
    wheels: &WheelMaterializationReceipt,
    reserved: &[PrefixExpectedFile],
) -> Result<TreeInventory> {
    ensure!(
        wheels.final_prefix() == prefix.final_path()
            && wheels.allocation_id() == prefix.allocation_id(),
        "wheel receipt belongs to a different allocation"
    );
    let files: Vec<_> = wheels
        .files()
        .iter()
        .map(|file| PrefixExpectedFile {
            relative: file.relative_path().into(),
            sha256: *file.sha256(),
            size: file.size(),
            mode: file.mode(),
        })
        .chain(reserved.iter().cloned())
        .collect();
    let mut directories = BTreeMap::<PathBuf, u32>::new();
    for path in wheels
        .directories()
        .iter()
        .map(|directory| directory.relative_path())
        .chain([Path::new("bin"), Path::new("site-packages")])
    {
        if !path.as_os_str().is_empty() {
            directories.insert(path.into(), 0o755);
        }
    }
    // create_dir_all owns every ancestor; include the complete expected set.
    let paths: Vec<_> = files
        .iter()
        .map(|file| file.relative.clone())
        .chain(directories.keys().cloned())
        .collect();
    for path in paths {
        let mut parent = path.parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(path.into(), 0o755);
            parent = path.parent();
        }
    }
    let directories: Vec<_> = directories
        .into_iter()
        .map(|(relative, mode)| PrefixExpectedDirectory { relative, mode })
        .collect();
    Ok(verify_prefix_inventory(
        prefix,
        &files,
        &directories,
        PrefixInventoryLimits::default(),
    )?
    .into_tree_inventory())
}

pub(super) struct PendingPackageTree {
    prefix: UnpublishedPrefix,
    pub(super) manifest: PackageManifest,
    wheels: WheelMaterializationReceipt,
    reserved: [PrefixExpectedFile; 2],
}
impl PendingPackageTree {
    pub(super) fn publish(
        self,
    ) -> Result<crate::packages::python_environment::prefix::SharedPublishedPrefix> {
        ensure!(
            verify_materialized_tree(&self.prefix, &self.wheels, &self.reserved)?
                == self.manifest.storage.inventory,
            "verified package output changed before publication"
        );
        self.prefix.publish_shared(
            &manifest::package_name(&self.manifest.facts.key),
            &serde_json::to_vec(&self.manifest)?,
            &self.manifest.storage.inventory,
        )
    }
}
/// Prepare while retaining the real original allocation EX. The caller must
/// hold existing dependencies SH or retain all new dependency owners EX.
pub(super) fn prepare_package_inputs(
    context: &OperationContext,
    language: &LanguageLease,
    facts: PackageFacts,
    archive: InspectedWheelArchive,
) -> Result<PendingPackageTree> {
    prepare_package_inputs_checked(context, language, facts, archive, || {
        language.check_binding()
    })
}

pub(super) fn prepare_package_inputs_in_batch(
    context: &OperationContext,
    scope: &LanguageMaterialization<'_>,
    facts: PackageFacts,
    archive: InspectedWheelArchive,
) -> Result<PendingPackageTree> {
    prepare_package_inputs_checked(context, scope.language(), facts, archive, || {
        scope.check_binding()
    })
}

fn prepare_package_inputs_checked(
    context: &OperationContext,
    language: &LanguageLease,
    facts: PackageFacts,
    archive: InspectedWheelArchive,
    check_language: impl Fn() -> Result<()>,
) -> Result<PendingPackageTree> {
    context.checkpoint()?;
    check_language()?;
    ensure!(
        facts.name == archive.manifest().name()
            && facts.version == archive.manifest().version()
            && facts.artifact == archive.digest().to_string(),
        "package facts differ from the inspected wheel"
    );
    ensure!(
        !facts.source.is_empty() && facts.dependencies.len() <= 65536,
        "invalid package source or dependency budget"
    );
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package library writes"
    );
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    let roots = [
        (WheelScheme::Purelib, "site-packages"),
        (WheelScheme::Platlib, "site-packages"),
        (WheelScheme::Headers, "include"),
        (WheelScheme::Scripts, "bin"),
        (WheelScheme::Data, ""),
    ]
    .into_iter()
    .map(|(s, p)| (s, prefix.final_path().join(p)))
    .collect();
    let scheme = WheelInstallScheme::new(prefix.final_path().into(), roots)?;
    let filename = archive.filename().to_owned();
    let source_size = archive.size();
    // A space forces the existing POSIX launcher fallback. A shell adapter
    // must not become another script's kernel shebang interpreter on systems
    // that reject recursive script interpreters.
    let interpreter = prefix.final_path().join("bin/.pakro python");
    let executable = language
        .entrypoint()
        .to_str()
        .context("shared language executable is not UTF-8")?;
    ensure!(
        !executable.contains(['\n', '\r', '\0']),
        "unsafe shared language executable"
    );
    let wrapper = format!(
        "#!/bin/sh\nexport PYTHONDONTWRITEBYTECODE=1\nexport PYTHONNOUSERSITE=1\nexec '{}' \"$@\"\n",
        executable.replace('\'', "'\\''")
    );
    prefix.create_dir_all(Path::new("bin"))?;
    {
        let mut file = prefix.create_file_exclusive(Path::new("bin/.pakro python"), 0o755)?;
        file.write_all(wrapper.as_bytes())?;
        file.finish()?;
    }
    let facts_bytes =
        manifest::package_facts_bytes(&facts, &language.key().fingerprint(), &filename)?;
    let expected_reserved = [
        PrefixExpectedFile {
            relative: "bin/.pakro python".into(),
            sha256: Sha256::digest(wrapper.as_bytes()).into(),
            size: wrapper.len() as u64,
            mode: 0o755,
        },
        PrefixExpectedFile {
            relative: manifest::PACKAGE_FACTS_FILE.into(),
            sha256: Sha256::digest(&facts_bytes).into(),
            size: facts_bytes.len() as u64,
            mode: 0o600,
        },
    ];
    {
        let mut file =
            prefix.create_file_exclusive(Path::new(manifest::PACKAGE_FACTS_FILE), 0o600)?;
        file.write_all(&facts_bytes)?;
        file.finish()?;
    }
    let reserved = [
        WheelReservedDestination {
            path: interpreter.clone(),
            kind: WheelReservedKind::File,
        },
        WheelReservedDestination {
            path: prefix.final_path().join(manifest::PACKAGE_FACTS_FILE),
            kind: WheelReservedKind::File,
        },
    ];
    let prepared = prepare_wheel_graph(
        vec![archive],
        &WheelInstallTarget {
            scheme: &scheme,
            interpreter: &interpreter,
            reserved: &reserved,
        },
        WheelLimits::default(),
        WheelPlanLimits::default(),
        WheelInstallLimits::default(),
        context.requested_cache_root(),
    )?;
    context.checkpoint()?;
    let wheels = materialize_wheel_graph(prepared, &mut prefix)?;
    ensure!(
        wheels.sources().len() == 1
            && wheels.sources()[0].owner().name() == facts.name
            && wheels.sources()[0].owner().version() == facts.version
            && wheels.sources()[0].filename() == filename
            && wheels.sources()[0].digest().to_string() == facts.artifact
            && wheels.sources()[0].size() == source_size,
        "wheel materialization receipt differs from the inspected source"
    );
    #[cfg(test)]
    AFTER_WHEEL_COPY.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(&prefix);
        }
    });
    prefix.create_dir_all(Path::new("site-packages"))?;
    let storage = storage_with_inventory(
        &prefix,
        facts.key,
        verify_materialized_tree(&prefix, &wheels, &expected_reserved)?,
    )?;
    let value = PackageManifest {
        facts,
        language: language.key().fingerprint(),
        filename,
        storage,
    };
    check_language()?;
    Ok(PendingPackageTree {
        prefix,
        manifest: value,
        wheels,
        reserved: expected_reserved,
    })
}

pub(crate) fn prepare_package_tree(
    context: &OperationContext,
    language: &LanguageLease,
    facts: PackageFacts,
    archive: InspectedWheelArchive,
) -> Result<PackageLease> {
    context.checkpoint()?;
    language.check_binding()?;
    ensure!(
        facts.name == archive.manifest().name()
            && facts.version == archive.manifest().version()
            && facts.artifact == archive.digest().to_string(),
        "package facts differ from the inspected wheel"
    );
    if let Some(existing) = acquire_package(context, facts.key)? {
        ensure!(
            existing.manifest().facts == facts
                && existing.manifest().language == language.key().fingerprint(),
            "conflicting package identity or bound language"
        );
        return Ok(existing);
    }
    let mut dependencies = facts.dependencies.clone();
    dependencies.sort();
    dependencies.dedup();
    let mut dependency_leases = Vec::new();
    for dependency in dependencies {
        dependency_leases.push(
            acquire_package(context, dependency)?.context("package dependency is not Ready")?,
        );
    }
    let value = facts.clone();
    let pending = prepare_package_inputs(context, language, facts, archive)?;
    let result = pending.publish();
    match result {
        Ok(guard) => drop(guard),
        Err(error) => {
            if !error.is::<crate::packages::python_environment::prefix::SharedPublicationConflict>()
            {
                return Err(error);
            }
            let winner = acquire_package(context, value.key)?
                .context("package publication failed without a winner")?;
            ensure!(
                winner.manifest().facts == value
                    && winner.manifest().language == language.key().fingerprint(),
                "conflicting package publication: {error:#}"
            );
            return Ok(winner);
        }
    }
    let lease = acquire_package(context, value.key)?.context("published package is missing")?;
    for dependency in &dependency_leases {
        dependency.check_binding()?;
    }
    Ok(lease)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::language::repository::tests::{
        archive, install_spec, operation_context, prepare,
    };
    use crate::libraries::package::layouts::tests::wheel;
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;

    #[tokio::test]
    async fn f3_changed_same_inode_after_wheel_copy_is_never_published_ready() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, true, Duration::from_secs(120));
        let bytes = archive("3.15.0");
        let language = prepare(&install_spec(&bytes), &context, &bytes)
            .await
            .unwrap();
        let (facts, archive) = wheel(
            &context,
            &language,
            "corruptme",
            [71; 32],
            vec![],
            vec![("corruptme.py", b"VALUE=1\n")],
            None,
        );
        let key = facts.key;
        fn mutate(prefix: &UnpublishedPrefix) {
            let path = prefix.final_path().join("site-packages/corruptme.py");
            let before = std::fs::metadata(&path).unwrap();
            std::fs::write(&path, b"VALUE=2\n").unwrap();
            let after = std::fs::metadata(&path).unwrap();
            assert_eq!(
                (before.dev(), before.ino(), before.len()),
                (after.dev(), after.ino(), after.len()),
                "the reproduction changes bytes without replacing the verified inode"
            );
        }
        AFTER_WHEEL_COPY.with(|slot| slot.set(Some(mutate)));
        let result = prepare_package_tree(&context, &language, facts, archive);
        assert!(
            AFTER_WHEEL_COPY.with(|slot| slot.take().is_none()),
            "the real C4 materialization must finish before fault injection"
        );
        assert!(
            result.is_err(),
            "modified output must not be re-hashed into a successful Ready publication"
        );
        assert!(
            manifest::read_package(&context, &key).unwrap().is_none(),
            "a changed verified wheel output must not gain Ready metadata"
        );
    }
}
