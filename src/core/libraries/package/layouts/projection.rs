//! A graph-owned, COW namespace adapter. Canonical package metadata, RECORDs,
//! scripts and unrelated imports remain in their immutable package trees.
use super::super::lease::PackageLease;
use super::super::manifest::LayoutManifest;
use crate::packages::python_environment::prefix::UnpublishedPrefix;
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixExpectedDirectory, PrefixExpectedFile, PrefixInventoryLimits,
    TreeInventory, TreeInventoryEntry, TreeInventoryKind, verify_prefix_inventory,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::open_directory;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
thread_local! {
    static AFTER_CLONE: std::cell::Cell<Option<fn(&UnpublishedPrefix)>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::libraries::package) struct NativeProjection {
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    files: Vec<ProjectionFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionFile {
    relative: PathBuf,
    owners: Vec<[u8; 32]>,
    size: u64,
    sha256: [u8; 32],
    mode: u32,
}
impl ProjectionFile {
    fn expected(&self) -> PrefixExpectedFile {
        PrefixExpectedFile {
            relative: self.relative.clone(),
            size: self.size,
            sha256: self.sha256,
            mode: self.mode,
        }
    }
}

fn namespace(path: &Path) -> Option<PathBuf> {
    let mut parts = path.strip_prefix("site-packages").ok()?.components();
    let Component::Normal(name) = parts.next()? else {
        return None;
    };
    let name = name.to_str()?;
    if name.ends_with(".dist-info") || name.ends_with(".egg-info") {
        return None;
    }
    Some(Path::new("site-packages").join(name))
}
fn native(path: &Path) -> bool {
    path.file_name()
        .and_then(|p| p.to_str())
        .is_some_and(|name| {
            name.ends_with(".so")
                || name.contains(".so.")
                || name.ends_with(".dylib")
                || name.ends_with(".pyd")
        })
}

pub(super) fn plan(
    context: &OperationContext,
    packages: &[PackageLease],
) -> Result<Option<NativeProjection>> {
    let limits = PrefixInventoryLimits::default();
    let mut used_entries = 0usize;
    let mut used_paths = 0usize;
    let mut roots = BTreeMap::<PathBuf, (BTreeSet<[u8; 32]>, bool)>::new();
    for package in packages {
        for entry in package.manifest().storage.inventory.entries() {
            context.checkpoint()?;
            used_entries = used_entries
                .checked_add(1)
                .filter(|n| *n <= limits.max_files)
                .context("native namespace input entry budget exceeded")?;
            used_paths = used_paths
                .checked_add(entry.relative.as_os_str().as_encoded_bytes().len())
                .filter(|n| *n <= limits.max_total_path_bytes)
                .context("native namespace input path budget exceeded")?;
            if !matches!(entry.kind, TreeInventoryKind::File { .. }) {
                continue;
            }
            let Some(root) = namespace(&entry.relative) else {
                continue;
            };
            if root == entry.relative {
                continue;
            } // A top-level module is not a namespace.
            let (owners, has_native) = roots.entry(root).or_default();
            owners.insert(package.manifest().key());
            *has_native |= native(&entry.relative);
        }
    }
    let roots: Vec<_> = roots
        .into_iter()
        .filter_map(|(root, (owners, native))| (owners.len() > 1 && native).then_some(root))
        .collect();
    if roots.is_empty() {
        return Ok(None);
    }
    let selected: BTreeSet<_> = roots.iter().map(PathBuf::as_path).collect();
    let mut directories = BTreeSet::new();
    let mut files = BTreeMap::<PathBuf, ProjectionFile>::new();
    for package in packages {
        for entry in package.manifest().storage.inventory.entries() {
            context.checkpoint()?;
            if !namespace(&entry.relative).is_some_and(|root| selected.contains(root.as_path())) {
                continue;
            }
            match &entry.kind {
                TreeInventoryKind::Directory => {
                    ensure!(
                        entry.mode == 0o755 && !files.contains_key(&entry.relative),
                        "native namespace has a directory/file collision or unsafe mode"
                    );
                    directories.insert(entry.relative.clone());
                }
                TreeInventoryKind::File { size, sha256 } => {
                    ensure!(
                        !directories.contains(&entry.relative),
                        "native namespace has a file/directory collision"
                    );
                    let key = package.manifest().key();
                    if let Some(previous) = files.get_mut(&entry.relative) {
                        ensure!(
                            (previous.size, previous.sha256, previous.mode)
                                == (*size, *sha256, entry.mode),
                            "native namespace contributors contain conflicting files"
                        );
                        previous.owners.push(key);
                    } else {
                        files.insert(
                            entry.relative.clone(),
                            ProjectionFile {
                                relative: entry.relative.clone(),
                                owners: vec![key],
                                size: *size,
                                sha256: *sha256,
                                mode: entry.mode,
                            },
                        );
                    }
                }
                _ => eyre::bail!(
                    "native namespace projection accepts only owned regular package content"
                ),
            }
        }
    }
    directories.insert("site-packages".into());
    let file_paths: BTreeSet<_> = files.keys().cloned().collect();
    for file in files.values_mut() {
        file.owners.sort();
        file.owners.dedup();
        let mut ancestor = file.relative.parent();
        while let Some(parent) = ancestor.filter(|p| !p.as_os_str().is_empty()) {
            ensure!(
                !file_paths.contains(parent),
                "native namespace file occupies an ancestor directory"
            );
            directories.insert(parent.into());
            ancestor = parent.parent();
        }
    }
    Ok(Some(NativeProjection {
        roots,
        directories: directories.into_iter().collect(),
        files: files.into_values().collect(),
    }))
}

fn open_source(
    package: &PackageLease,
    entry: &TreeInventoryEntry,
    index: &BTreeMap<&Path, &TreeInventoryEntry>,
) -> Result<File> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    let protection = package.protection.as_ref();
    InventoryRoot::check_binding(protection)?;
    let mut directory = protection.root_file().try_clone()?;
    let parent = entry
        .relative
        .parent()
        .context("native namespace source has no parent")?;
    let mut relative = PathBuf::new();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            eyre::bail!("unsafe native namespace source path")
        };
        relative.push(name);
        let expected = index
            .get(relative.as_path())
            .context("source parent is absent from its inventory")?;
        ensure!(
            matches!(expected.kind, TreeInventoryKind::Directory),
            "source parent is not an inventoried directory"
        );
        directory = open_directory(&directory, name)?;
        let metadata = directory.metadata()?;
        ensure!(
            [metadata.dev(), metadata.ino()] == expected.identity
                && metadata.uid() == protection.expected_uid()
                && metadata.mode() & 0o7777 == expected.mode,
            "native namespace source parent binding changed"
        );
    }
    let name = entry.relative.file_name().context("missing source leaf")?;
    let file = File::from(openat(
        &directory,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && [metadata.dev(), metadata.ino()] == entry.identity
            && metadata.uid() == protection.expected_uid()
            && metadata.nlink() == 1
            && metadata.mode() & 0o7777 == entry.mode,
        "native namespace source leaf binding changed"
    );
    InventoryRoot::check_binding(protection)?;
    Ok(file)
}

pub(super) fn materialize(
    context: &OperationContext,
    prefix: &mut UnpublishedPrefix,
    packages: &[PackageLease],
    projection: &NativeProjection,
) -> Result<()> {
    let packages: BTreeMap<_, _> = packages
        .iter()
        .map(|package| {
            (
                package.manifest().key(),
                (
                    package,
                    package
                        .manifest()
                        .storage
                        .inventory
                        .entries()
                        .iter()
                        .map(|entry| (entry.relative.as_path(), entry))
                        .collect::<BTreeMap<_, _>>(),
                ),
            )
        })
        .collect();
    for directory in &projection.directories {
        context.checkpoint()?;
        prefix.create_dir_all(directory)?;
    }
    for file in &projection.files {
        context.checkpoint()?;
        let (package, index) = packages
            .get(&file.owners[0])
            .context("native namespace source owner is missing")?;
        let entry = index
            .get(file.relative.as_path())
            .context("native namespace source file is missing")?;
        let source = open_source(package, entry, index)?;
        prefix.clone_file_exclusive(&source, &file.expected())?;
    }
    #[cfg(test)]
    AFTER_CLONE.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(prefix);
        }
    });
    Ok(())
}

pub(super) fn verify_output(
    prefix: &UnpublishedPrefix,
    generated: &[PrefixExpectedFile],
    projection: Option<&NativeProjection>,
) -> Result<TreeInventory> {
    let mut files = generated.to_vec();
    let mut directories = BTreeSet::<PathBuf>::new();
    if let Some(projection) = projection {
        files.extend(projection.files.iter().map(ProjectionFile::expected));
        directories.extend(projection.directories.iter().cloned());
    }
    for file in &files {
        let mut parent = file.relative.parent();
        while let Some(path) = parent.filter(|p| !p.as_os_str().is_empty()) {
            directories.insert(path.into());
            parent = path.parent();
        }
    }
    let directories: Vec<_> = directories
        .into_iter()
        .map(|relative| PrefixExpectedDirectory {
            relative,
            mode: 0o755,
        })
        .collect();
    Ok(verify_prefix_inventory(
        prefix,
        &files,
        &directories,
        PrefixInventoryLimits::default(),
    )?
    .into_tree_inventory())
}

pub(in crate::libraries::package) fn validate(
    context: &OperationContext,
    manifest: &LayoutManifest,
    packages: &[PackageLease],
) -> Result<()> {
    ensure!(
        packages
            .iter()
            .all(|package| package.manifest().language == manifest.language),
        "layout package target differs from its selected language"
    );
    if manifest.recipe == 1 {
        ensure!(
            manifest.projection.is_none(),
            "legacy layout cannot contain a new native namespace projection"
        );
        return Ok(());
    }
    ensure!(manifest.recipe == 2, "unsupported shared layout recipe");
    let expected = plan(context, packages)?;
    ensure!(
        manifest.projection == expected,
        "native namespace mapping differs from the held package graph"
    );
    let files: BTreeMap<_, _> = expected
        .as_ref()
        .into_iter()
        .flat_map(|p| &p.files)
        .map(|file| (file.relative.as_path(), file))
        .collect();
    let directories: BTreeSet<_> = expected
        .as_ref()
        .into_iter()
        .flat_map(|p| &p.directories)
        .map(PathBuf::as_path)
        .collect();
    let sources: BTreeMap<_, _> = packages
        .iter()
        .map(|package| {
            (
                package.manifest().key(),
                package
                    .manifest()
                    .storage
                    .inventory
                    .entries()
                    .iter()
                    .map(|entry| (entry.relative.as_path(), entry))
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect();
    let mut seen = 0;
    let mut seen_directories = 0;
    for entry in manifest.storage.inventory.entries() {
        context.checkpoint()?;
        if !entry.relative.starts_with("site-packages") {
            continue;
        }
        match &entry.kind {
            TreeInventoryKind::File { size, sha256 } => {
                let expected = files
                    .get(entry.relative.as_path())
                    .context("layout contains an unmapped namespace file")?;
                ensure!(
                    (*size, *sha256, entry.mode) == (expected.size, expected.sha256, expected.mode),
                    "projected file differs from canonical package content"
                );
                for owner in &expected.owners {
                    let source = sources
                        .get(owner)
                        .and_then(|index| index.get(entry.relative.as_path()))
                        .context("mapped canonical source is missing")?;
                    ensure!(
                        source.identity != entry.identity,
                        "native namespace mapping aliases its source inode"
                    );
                }
                seen += 1;
            }
            TreeInventoryKind::Directory => {
                ensure!(
                    directories.contains(entry.relative.as_path()) && entry.mode == 0o755,
                    "layout contains an unmapped namespace directory"
                );
                seen_directories += 1;
            }
            _ => eyre::bail!("layout native namespace contains an unsupported link"),
        }
    }
    ensure!(
        seen == files.len(),
        "layout is missing a native namespace mapping"
    );
    ensure!(
        seen_directories == directories.len(),
        "layout is missing a native namespace directory"
    );
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::super::{layout_key, prepare_layout};
    use super::*;
    use crate::libraries::language::lease::LanguageLease;
    use crate::libraries::language::repository::tests::{
        archive, install_spec, operation_context, prepare,
    };
    use crate::libraries::package::layouts::tests::wheel;
    use crate::libraries::package::{lease::acquire_layout, manifest};
    use std::time::Duration;

    async fn fixture() -> (tempfile::TempDir, OperationContext, LanguageLease) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, true, Duration::from_secs(120));
        let bytes = archive("3.15.0");
        let language = prepare(&install_spec(&bytes), &context, &bytes)
            .await
            .unwrap();
        (owner, context, language)
    }
    fn inputs(
        context: &OperationContext,
        language: &LanguageLease,
    ) -> Vec<(
        manifest::PackageFacts,
        crate::packages::python_wheel_source::InspectedWheelArchive,
    )> {
        vec![
            wheel(
                context,
                language,
                "projectiona",
                [85; 32],
                vec![[86; 32]],
                vec![
                    ("sharednative/native.so", b"native fixture"),
                    ("sharednative/__init__.py", b""),
                ],
                None,
            ),
            wheel(
                context,
                language,
                "projectionb",
                [86; 32],
                vec![],
                vec![("sharednative/lib/provider.dylib", b"provider fixture")],
                None,
            ),
        ]
    }
    #[tokio::test]
    async fn f3_native_projection_mapping_survives_reopen_and_rejects_forged_owner() {
        let (_owner, context, language) = fixture().await;
        let layout =
            prepare_layout(&context, &language, [87; 32], inputs(&context, &language)).unwrap();
        let key = layout.key();
        let path = manifest::store_path(&context)
            .join(".control/catalog")
            .join(manifest::layout_name(&key));
        let original = std::fs::read(&path).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        changed["projection"]["files"][0]["owners"] = serde_json::json!([]);
        let changed: LayoutManifest = serde_json::from_value(changed).unwrap();
        drop(layout);
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            acquire_layout(&context, key).is_err(),
            "a valid JSON mapping cannot invent its source owner"
        );
        std::fs::write(&path, original).unwrap();
        acquire_layout(&context, key)
            .unwrap()
            .unwrap()
            .check_binding()
            .unwrap();
    }
    #[tokio::test]
    async fn f3_native_projection_rejects_file_directory_collisions() {
        let (_owner, context, language) = fixture().await;
        let a = wheel(
            &context,
            &language,
            "collisiona",
            [88; 32],
            vec![],
            vec![("collision/native.so", b"first")],
            None,
        );
        let b = wheel(
            &context,
            &language,
            "collisionb",
            [89; 32],
            vec![],
            vec![("collision/native.so/child", b"second")],
            None,
        );
        assert!(prepare_layout(&context, &language, [90; 32], vec![a, b]).is_err());
        assert!(
            acquire_layout(&context, layout_key(language.key().fingerprint(), [90; 32]))
                .unwrap()
                .is_none()
        );
        language.check_binding().unwrap();
    }
    #[tokio::test]
    async fn f3_native_projection_changed_same_inode_is_not_blessed_ready() {
        let (_owner, context, language) = fixture().await;
        fn change(prefix: &UnpublishedPrefix) {
            let path = prefix
                .final_path()
                .join("site-packages/sharednative/native.so");
            let before = std::fs::metadata(&path).unwrap();
            std::fs::write(&path, b"changedfixture").unwrap();
            let after = std::fs::metadata(path).unwrap();
            assert_eq!((before.ino(), before.len()), (after.ino(), after.len()));
        }
        AFTER_CLONE.with(|slot| slot.set(Some(change)));
        assert!(
            prepare_layout(&context, &language, [91; 32], inputs(&context, &language)).is_err()
        );
        assert!(AFTER_CLONE.with(|slot| slot.take().is_none()));
        assert!(
            acquire_layout(&context, layout_key(language.key().fingerprint(), [91; 32]))
                .unwrap()
                .is_none()
        );
        for key in [[85; 32], [86; 32]] {
            crate::libraries::package::lease::acquire_package(&context, key)
                .unwrap()
                .unwrap()
                .check_binding()
                .unwrap();
        }
    }
    #[tokio::test]
    async fn f3_layout_legacy_recipe_roundtrips_and_keeps_original_key() {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let (_owner, context, language) = fixture().await;
        let package = wheel(
            &context,
            &language,
            "legacyfixture",
            [92; 32],
            vec![],
            vec![("legacyfixture.py", b"VALUE=1\n")],
            None,
        );
        let layout = prepare_layout(&context, &language, [93; 32], vec![package]).unwrap();
        assert!(
            !layout.prefix().join("site-packages").exists(),
            "pure wheels retain the tiny startup mapping"
        );
        let mut old = layout.manifest.clone();
        let new_key = layout.key();
        old.recipe = 1;
        old.storage.key = super::super::layout_key_for(1, old.language, old.graph).unwrap();
        let key = old.storage.key;
        let bytes = serde_json::to_vec(&old).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.get("recipe").is_none() && json.get("projection").is_none());
        let old_path = manifest::store_path(&context)
            .join(".control/catalog")
            .join(manifest::layout_name(&key));
        let mut old_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(old_path)
            .unwrap();
        old_file.write_all(&bytes).unwrap();
        old_file.sync_all().unwrap();
        drop(old_file);
        drop(layout);
        std::fs::remove_file(
            manifest::store_path(&context)
                .join(".control/catalog")
                .join(manifest::layout_name(&new_key)),
        )
        .unwrap();
        acquire_layout(&context, key)
            .unwrap()
            .unwrap()
            .check_binding()
            .unwrap();
        assert_eq!(
            super::super::reserve_layout(&context, old.language, old.graph)
                .unwrap()
                .key(),
            key
        );
    }
}
