//! A graph-owned native namespace adapter. Canonical package metadata, RECORDs,
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
    static INITIAL_METHOD: std::cell::Cell<Option<ProjectionMethod>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::libraries::package) struct NativeProjection {
    #[serde(default, skip_serializing_if = "ProjectionMethod::is_legacy")]
    pub(super) method: ProjectionMethod,
    roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    files: Vec<ProjectionFile>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::libraries::package) enum ProjectionMethod {
    #[default]
    MacosCow,
    LinuxReflink,
    VerifiedCopy,
}
impl ProjectionMethod {
    fn is_legacy(&self) -> bool {
        *self == Self::MacosCow
    }
    pub(super) fn recipe(self) -> u32 {
        match self {
            Self::MacosCow => 2,
            Self::LinuxReflink => 3,
            Self::VerifiedCopy => 4,
        }
    }
}

pub(super) fn initial_method() -> ProjectionMethod {
    #[cfg(test)]
    if let Some(method) = INITIAL_METHOD.with(|slot| slot.take()) {
        return method;
    }
    #[cfg(target_os = "linux")]
    {
        ProjectionMethod::LinuxReflink
    }
    #[cfg(not(target_os = "linux"))]
    {
        ProjectionMethod::MacosCow
    }
}

pub(in crate::libraries::package) fn validate_metadata(
    recipe: u32,
    projection: Option<&NativeProjection>,
) -> Result<()> {
    match (recipe, projection) {
        (1, None) | (2, None) | (5, None) | (6, None) => Ok(()),
        (2..=4, Some(projection)) => {
            ensure!(
                recipe == projection.method.recipe()
                    && !projection.roots.is_empty()
                    && !projection.files.is_empty(),
                "shared layout recipe differs from its actual native projection method"
            );
            Ok(())
        }
        _ => eyre::bail!("shared layout recipe has no matching native projection"),
    }
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
    method: ProjectionMethod,
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
        method,
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
) -> Result<bool> {
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
        match projection.method {
            ProjectionMethod::MacosCow => {
                prefix.clone_file_exclusive(&source, &file.expected())?;
            }
            ProjectionMethod::LinuxReflink => {
                if prefix
                    .try_reflink_file_exclusive(&source, &file.expected())?
                    .is_none()
                {
                    return Ok(false);
                }
            }
            ProjectionMethod::VerifiedCopy => {
                prefix.copy_file_exclusive(&source, &file.expected())?;
            }
        }
    }
    #[cfg(test)]
    AFTER_CLONE.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(prefix);
        }
    });
    Ok(true)
}

pub(in crate::libraries::package) fn verify_output(
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
    validate_metadata(manifest.recipe, manifest.projection.as_ref())?;
    if matches!(manifest.recipe, 5 | 6) {
        return Ok(());
    }
    ensure!(
        packages
            .iter()
            .all(|package| package.manifest().language == Some(manifest.language)),
        "layout package target differs from its selected language"
    );
    if manifest.recipe == 1 {
        ensure!(
            manifest.projection.is_none(),
            "legacy layout cannot contain a new native namespace projection"
        );
        return Ok(());
    }
    let method = manifest
        .projection
        .as_ref()
        .map_or(ProjectionMethod::MacosCow, |projection| projection.method);
    let expected = plan(context, packages, method)?;
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

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod method_tests {
    use super::*;
    use crate::libraries::language::lease::LanguageLease;
    use crate::libraries::language::repository::tests::{operation_context, prepare};
    #[cfg(target_os = "linux")]
    use crate::libraries::package::layouts::prepare_layout_with_sources;
    use crate::libraries::package::layouts::tests::wheel;
    use crate::libraries::package::layouts::{layout_key_for, prepare_layout};
    #[cfg(target_os = "linux")]
    use crate::packages::python_resolution::source::registry_depot::closed::SharedRegistryPackageSource;
    use std::os::unix::fs::FileExt;
    use std::time::Duration;

    fn mapping(method: ProjectionMethod) -> NativeProjection {
        NativeProjection {
            method,
            roots: vec!["site-packages/sharednative".into()],
            directories: vec!["site-packages".into(), "site-packages/sharednative".into()],
            files: vec![ProjectionFile {
                relative: "site-packages/sharednative/native.so".into(),
                owners: vec![[1; 32]],
                size: 1,
                sha256: [2; 32],
                mode: 0o644,
            }],
        }
    }

    #[test]
    fn p6_projection_methods_have_distinct_recipes_and_legacy_json_is_unchanged() {
        let old = mapping(ProjectionMethod::MacosCow);
        let json = serde_json::to_value(&old).unwrap();
        assert!(json.get("method").is_none());
        let decoded: NativeProjection = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(decoded, old);
        assert_eq!(serde_json::to_value(decoded).unwrap(), json);
        let mut keys = BTreeSet::new();
        for recipe in 1..=6 {
            assert!(keys.insert(layout_key_for(recipe, [3; 32], [4; 32]).unwrap()));
        }
        for method in [
            ProjectionMethod::MacosCow,
            ProjectionMethod::LinuxReflink,
            ProjectionMethod::VerifiedCopy,
        ] {
            let value = mapping(method);
            validate_metadata(method.recipe(), Some(&value)).unwrap();
            for recipe in 1..=6 {
                if recipe != method.recipe() {
                    assert!(validate_metadata(recipe, Some(&value)).is_err());
                }
            }
        }
        assert!(validate_metadata(3, None).is_err());
        assert!(validate_metadata(4, None).is_err());
        assert!(validate_metadata(5, None).is_ok());
        assert!(validate_metadata(6, None).is_ok());
    }

    async fn fixture() -> (tempfile::TempDir, OperationContext, LanguageLease) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, true, Duration::from_secs(120));
        #[cfg(target_os = "macos")]
        let (bytes, spec) = {
            use crate::libraries::language::repository::tests::{archive, install_spec};
            let bytes = archive("3.15.0");
            let spec = install_spec(&bytes);
            (bytes, spec)
        };
        #[cfg(target_os = "linux")]
        let (bytes, spec) = {
            use crate::libraries::language::repository::tests::{archive_for, install_spec_for};
            use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
            #[cfg(target_env = "musl")]
            let platform = RuntimePlatform::LinuxMusl;
            #[cfg(not(target_env = "musl"))]
            let platform = RuntimePlatform::LinuxGnu;
            let target = RuntimeArchiveTarget::new(std::env::consts::ARCH, platform).unwrap();
            let bytes = archive_for("3.15.0", &target, "");
            let spec = install_spec_for("3.15.0", &bytes, &target, "20261013");
            (bytes, spec)
        };
        let language = prepare(&spec, &context, &bytes).await.unwrap();
        (owner, context, language)
    }

    fn inputs(
        context: &OperationContext,
        language: &LanguageLease,
    ) -> Vec<(
        crate::libraries::package::manifest::PackageFacts,
        crate::packages::python_wheel_source::InspectedWheelArchive,
    )> {
        vec![
            wheel(
                context,
                language,
                "projectiona",
                [185; 32],
                vec![[186; 32]],
                vec![
                    ("sharednative/__init__.py", b"VALUE=1\n"),
                    ("sharednative/native.so", b"native fixture"),
                ],
                None,
            ),
            wheel(
                context,
                language,
                "projectionb",
                [186; 32],
                vec![],
                vec![("sharednative/lib/provider.so", b"provider fixture")],
                None,
            ),
        ]
    }

    fn unavailable(_: &File, _: &File) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP))
    }

    fn one_success_then_unavailable(source: &File, target: &File) -> std::io::Result<()> {
        // Exercise the owned successful-FD boundary before the next attempt
        // reports unavailable. This is not evidence of a Linux kernel clone.
        UnpublishedPrefix::install_reflink_test_attempt(Some(unavailable));
        let mut offset = 0u64;
        let mut buffer = [0u8; 4096];
        loop {
            let count = source.read_at(&mut buffer, offset)?;
            if count == 0 {
                break;
            }
            target.write_all_at(&buffer[..count], offset)?;
            offset += count as u64;
        }
        Ok(())
    }

    struct ResetHooks;
    impl Drop for ResetHooks {
        fn drop(&mut self) {
            INITIAL_METHOD.with(|slot| slot.set(None));
            UnpublishedPrefix::install_reflink_test_attempt(None);
        }
    }

    #[tokio::test]
    async fn p6_unavailable_reflink_discards_the_whole_projection_and_rebuilds_copy() {
        let (_owner, context, language) = fixture().await;
        let _reset = ResetHooks;
        INITIAL_METHOD.with(|slot| slot.set(Some(ProjectionMethod::LinuxReflink)));
        UnpublishedPrefix::install_reflink_test_attempt(Some(one_success_then_unavailable));
        let layout =
            prepare_layout(&context, &language, [187; 32], inputs(&context, &language)).unwrap();
        assert_eq!(layout.manifest.recipe, 4);
        assert_eq!(
            layout.manifest.projection.as_ref().unwrap().method,
            ProjectionMethod::VerifiedCopy
        );
        assert_eq!(
            layout.key(),
            layout_key_for(4, layout.language(), layout.graph()).unwrap()
        );
        layout.manifest.validate_metadata().unwrap();
        layout.check_binding().unwrap();
        let allocations = std::fs::read_dir(super::super::super::manifest::store_path(&context))
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with("g-")
                    .then_some(entry.path())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            allocations.len(),
            3,
            "failed reflink allocation must be fully discarded"
        );
        let original = layout.packages()[0]
            .root()
            .join("site-packages/sharednative/native.so");
        let projected = layout.prefix().join("site-packages/sharednative/native.so");
        let a = std::fs::metadata(&original).unwrap();
        let b = std::fs::metadata(&projected).unwrap();
        assert_ne!((a.dev(), a.ino()), (b.dev(), b.ino()));
        assert_eq!(b.nlink(), 1);
        assert_eq!(
            std::fs::read(&original).unwrap(),
            std::fs::read(&projected).unwrap()
        );
        let mut altered = layout.manifest.clone();
        altered.recipe = 3;
        altered.storage.key = layout_key_for(3, altered.language, altered.graph).unwrap();
        assert!(altered.validate_metadata().is_err());
        assert!(
            !layout.prefix().join("lib").exists(),
            "layout must not copy the interpreter"
        );
        #[cfg(target_os = "linux")]
        {
            let existing = layout
                .packages()
                .iter()
                .map(|package| {
                    (
                        package.manifest().facts.clone(),
                        SharedRegistryPackageSource::Existing(package.clone()),
                    )
                })
                .collect();
            let again =
                prepare_layout_with_sources(&context, &language, layout.graph(), existing).unwrap();
            assert_eq!(again.key(), layout.key());
            assert_eq!(again.prefix(), layout.prefix());
            assert_eq!(
                super::super::reserve_layout(&context, layout.language(), layout.graph())
                    .unwrap()
                    .prefix(),
                layout.prefix()
            );
        }
        std::fs::write(&projected, b"mutate fixture").unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"native fixture");
        assert!(layout.check_binding().is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn p6_linux_native_layout_records_the_real_method_and_reuses_the_combination() {
        let (_owner, context, language) = fixture().await;
        let layout =
            prepare_layout(&context, &language, [188; 32], inputs(&context, &language)).unwrap();
        let projection = layout.manifest.projection.as_ref().unwrap();
        assert!(matches!(
            projection.method,
            ProjectionMethod::LinuxReflink | ProjectionMethod::VerifiedCopy
        ));
        assert_eq!(projection.method.recipe(), layout.manifest.recipe);
        println!("P6_LINUX_PROJECTION_METHOD={:?}", projection.method);
        if let Ok(expected) = std::env::var("PAKRO_TEST_EXPECT_PROJECTION_METHOD") {
            assert_eq!(
                serde_json::to_value(projection.method)
                    .unwrap()
                    .as_str()
                    .unwrap(),
                expected
            );
        }
        let existing = layout
            .packages()
            .iter()
            .map(|package| {
                (
                    package.manifest().facts.clone(),
                    SharedRegistryPackageSource::Existing(package.clone()),
                )
            })
            .collect();
        let again =
            prepare_layout_with_sources(&context, &language, layout.graph(), existing).unwrap();
        assert_eq!(layout.key(), again.key());
        assert_eq!(layout.prefix(), again.prefix());
        layout.check_binding().unwrap();
    }

    #[cfg(target_os = "linux")]
    mod elf {
        use super::*;
        use crate::libraries::package::manifest::PackageFacts;
        use crate::packages::python_wheel::{WheelExpectation, WheelLimits};
        use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};
        use base64::Engine as _;
        use sha2::{Digest as _, Sha256};
        use std::io::{Cursor, Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        use std::process::{Command, Output};

        const INIT: &[u8] = b"import ctypes\nfrom pathlib import Path\nLIBRARY_PATH = str(Path(__file__).with_name('native.so'))\n_lib = ctypes.CDLL(LIBRARY_PATH)\n_lib.pakro_answer.restype = ctypes.c_int\ndef answer():\n    return _lib.pakro_answer()\n";

        fn required_path(name: &str) -> PathBuf {
            let path = PathBuf::from(
                std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")),
            );
            assert!(path.is_absolute(), "{name} must be absolute");
            path.canonicalize().unwrap()
        }

        fn input(root: &File, parent: Option<&str>, leaf: &str, expected_env: &str) -> Vec<u8> {
            let expected = std::env::var(expected_env)
                .unwrap_or_else(|_| panic!("{expected_env} is required"));
            let decoded = hex::decode(&expected).unwrap();
            assert!(
                decoded.len() == 32 && hex::encode(&decoded) == expected,
                "{expected_env} must be a complete lowercase SHA256"
            );
            let directory = parent.map(|name| open_directory(root, name).unwrap());
            let directory = directory.as_ref().unwrap_or(root);
            pakro_store::control::validate_owned_directory(
                directory,
                nix::unistd::geteuid().as_raw(),
            )
            .unwrap();
            let mut file = File::from(
                nix::fcntl::openat(
                    directory,
                    leaf,
                    nix::fcntl::OFlag::O_RDONLY
                        | nix::fcntl::OFlag::O_NOFOLLOW
                        | nix::fcntl::OFlag::O_CLOEXEC
                        | nix::fcntl::OFlag::O_NONBLOCK,
                    nix::sys::stat::Mode::empty(),
                )
                .unwrap(),
            );
            let before = file.metadata().unwrap();
            assert!(
                before.is_file()
                    && before.uid() == nix::unistd::geteuid().as_raw()
                    && before.nlink() == 1
                    && before.mode() & 0o022 == 0
                    && before.len() >= 64
                    && before.len() <= 512 * 1024,
                "ELF fixture must be an owned, bounded, independent regular file"
            );
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(512 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            let after = file.metadata().unwrap();
            assert_eq!(
                (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.mtime(),
                    before.mtime_nsec(),
                    before.ctime(),
                    before.ctime_nsec()
                ),
                (
                    after.dev(),
                    after.ino(),
                    after.len(),
                    after.mtime(),
                    after.mtime_nsec(),
                    after.ctime(),
                    after.ctime_nsec()
                )
            );
            assert_eq!(bytes.len() as u64, before.len());
            assert_eq!(hex::encode(Sha256::digest(&bytes)), expected);
            assert_eq!(&bytes[..6], b"\x7fELF\x02\x01");
            assert_eq!(
                u16::from_le_bytes([bytes[16], bytes[17]]),
                3,
                "fixture must be ET_DYN"
            );
            let machine = match std::env::consts::ARCH {
                "aarch64" => 183,
                "x86_64" => 62,
                other => panic!("unsupported ELF fixture architecture {other}"),
            };
            assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), machine);
            println!(
                "P6_ELF_INPUT={leaf} SHA256={expected} BYTES={}",
                bytes.len()
            );
            bytes
        }

        fn native_wheel(
            context: &OperationContext,
            language: &LanguageLease,
            name: &str,
            key: [u8; 32],
            dependencies: Vec<[u8; 32]>,
            files: Vec<(&str, &[u8])>,
        ) -> (PackageFacts, InspectedWheelArchive) {
            let tag = language
                .target()
                .supported()
                .ordered()
                .iter()
                .find(|tag| {
                    tag.interpreter() == "py3"
                        && tag.abi() == "none"
                        && tag.platform().starts_with("linux_")
                })
                .expect("fixture requires a supported native Linux py3-none tag");
            let tag = format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform());
            let info = format!("{name}-1.0.dist-info");
            let mut members = files
                .into_iter()
                .map(|(path, bytes)| (path.to_owned(), bytes.to_vec()))
                .collect::<Vec<_>>();
            members.push((
                format!("{info}/WHEEL"),
                format!("Wheel-Version: 1.0\nRoot-Is-Purelib: false\nTag: {tag}\n").into_bytes(),
            ));
            members.push((
                format!("{info}/METADATA"),
                format!("Metadata-Version: 2.4\nName: {name}\nVersion: 1.0\n").into_bytes(),
            ));
            let mut record = String::new();
            for (path, bytes) in &members {
                record.push_str(&format!(
                    "{path},sha256={},{}\n",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
                    bytes.len()
                ));
            }
            record.push_str(&format!("{info}/RECORD,,\n"));
            members.push((format!("{info}/RECORD"), record.into_bytes()));
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (path, bytes) in members {
                archive
                    .start_file(
                        path,
                        zip::write::SimpleFileOptions::default()
                            .last_modified_time(zip::DateTime::default())
                            .unix_permissions(0o644),
                    )
                    .unwrap();
                archive.write_all(&bytes).unwrap();
            }
            let bytes = archive.finish().unwrap().into_inner();
            let filename = format!("{name}-1.0-{tag}.whl");
            let digest = pakro_store::Digest::from_sha256_bytes(&bytes);
            let artifact = crate::artifact_snapshot::spool_verified_reader(
                bytes.as_slice(),
                &filename,
                digest,
                1024 * 1024,
                context.requested_cache_root(),
            )
            .unwrap();
            let archive = inspect_owned_wheel(
                artifact,
                digest,
                &WheelExpectation {
                    filename: &filename,
                    name,
                    version: "1.0",
                    supported: language.target().supported(),
                },
                WheelLimits::default(),
            )
            .unwrap();
            (
                PackageFacts {
                    key,
                    name: name.into(),
                    version: "1.0".into(),
                    artifact: digest.to_string(),
                    source: "fixture-precompiled-elf-source".into(),
                    dependencies,
                },
                archive,
            )
        }

        fn python(python: &Path, home: &Path, mode: &str, first: &Path, second: &Path) -> Output {
            let program = r#"import ctypes, pathlib, sys
if sys.argv[1] == 'bootstrap':
    sys.path.insert(0, sys.argv[2])
    import sitecustomize
else:
    sys.path[:0] = [sys.argv[2], sys.argv[3]]
import sharednative
assert sharednative.answer() == 42
library = ctypes.CDLL(sharednative.LIBRARY_PATH)
library.pakro_answer.restype = ctypes.c_int
assert library.pakro_answer() == 42
if sys.argv[1] == 'bootstrap':
    from importlib import metadata, resources
    assert metadata.version('projectionelfconsumer') == '1.0'
    assert metadata.version('projectionelfprovider') == '1.0'
    assert resources.files('sharednative').joinpath('lib/libpakro_provider.so').is_file()
print('P6_ELF_ANSWER=42', sharednative.__file__, sharednative.LIBRARY_PATH, sys.version)
"#;
            let output = Command::new(python)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .current_dir(home)
                .args(["-I", "-S", "-B", "-c", program, mode])
                .arg(first)
                .arg(second)
                .output()
                .unwrap();
            println!(
                "P6_SYSTEM_PYTHON mode={mode} EXIT={:?}\n{}\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            output
        }

        #[ignore = "requires owned precompiled ELF inputs, explicit SHA256s and a system Python; compiles nothing"]
        #[tokio::test]
        async fn p6_linux_precompiled_elf_loads_from_shared_projection() {
            let input_path = required_path("PAKRO_TEST_ELF_FIXTURE_DIR");
            let directory = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(&input_path)
                .unwrap();
            pakro_store::control::validate_owned_directory(
                &directory,
                nix::unistd::geteuid().as_raw(),
            )
            .unwrap();
            let consumer = input(
                &directory,
                None,
                "native.so",
                "PAKRO_TEST_ELF_CONSUMER_SHA256",
            );
            let provider = input(
                &directory,
                Some("lib"),
                "libpakro_provider.so",
                "PAKRO_TEST_ELF_PROVIDER_SHA256",
            );
            let interpreter = required_path("PAKRO_TEST_SYSTEM_PYTHON");
            let metadata = std::fs::symlink_metadata(&interpreter).unwrap();
            assert!(metadata.is_file() && metadata.mode() & 0o111 != 0);
            println!(
                "P6_SYSTEM_PYTHON={} SHA256={}",
                interpreter.display(),
                hex::encode(Sha256::digest(std::fs::read(&interpreter).unwrap()))
            );
            let expected = std::env::var("PAKRO_TEST_EXPECT_PROJECTION_METHOD")
                .expect("actual expected projection method is required");
            assert!(matches!(
                expected.as_str(),
                "linux-reflink" | "verified-copy"
            ));
            let (_owner, context, language) = fixture().await;
            let home = context.requested_data_root().parent().unwrap();
            println!(
                "P6_MANAGED_RUNTIME=synthetic; real ELF execution uses the explicit system Python only"
            );
            let reference = home.join("reference");
            let namespace = reference.join("sharednative");
            std::fs::create_dir_all(namespace.join("lib")).unwrap();
            std::fs::write(namespace.join("__init__.py"), INIT).unwrap();
            std::fs::write(namespace.join("native.so"), &consumer).unwrap();
            std::fs::write(namespace.join("lib/libpakro_provider.so"), &provider).unwrap();
            let control = python(&interpreter, home, "reference", &reference, &reference);
            assert!(
                control.status.success(),
                "adjacent ELF reference must load before testing projection"
            );

            let sources = vec![
                native_wheel(
                    &context,
                    &language,
                    "projectionelfconsumer",
                    [195; 32],
                    vec![[196; 32]],
                    vec![
                        ("sharednative/__init__.py", INIT),
                        ("sharednative/native.so", &consumer),
                    ],
                ),
                native_wheel(
                    &context,
                    &language,
                    "projectionelfprovider",
                    [196; 32],
                    vec![],
                    vec![("sharednative/lib/libpakro_provider.so", &provider)],
                ),
            ];
            let layout = prepare_layout(&context, &language, [197; 32], sources).unwrap();
            let projection = layout.manifest.projection.as_ref().unwrap();
            assert_eq!(
                serde_json::to_value(projection.method)
                    .unwrap()
                    .as_str()
                    .unwrap(),
                expected
            );
            assert_eq!(
                layout.manifest.recipe,
                if expected == "linux-reflink" { 3 } else { 4 }
            );
            let consumer_package = layout
                .packages()
                .iter()
                .find(|package| package.manifest().key() == [195; 32])
                .unwrap();
            let provider_package = layout
                .packages()
                .iter()
                .find(|package| package.manifest().key() == [196; 32])
                .unwrap();
            let split = python(
                &interpreter,
                home,
                "split",
                &consumer_package.site_packages(),
                &provider_package.site_packages(),
            );
            assert!(
                !split.status.success(),
                "split canonical wheels must not accidentally satisfy loader adjacency"
            );
            assert!(String::from_utf8_lossy(&split.stderr).contains("libpakro_provider.so"));
            let output = python(
                &interpreter,
                home,
                "bootstrap",
                &layout.prefix().join("bootstrap"),
                &reference,
            );
            assert!(
                output.status.success(),
                "actual shared bootstrap must make the ELF dependency adjacent"
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(layout.prefix().to_str().unwrap())
            );
            for (package, path, bytes) in [
                (consumer_package, "sharednative/native.so", &consumer),
                (
                    provider_package,
                    "sharednative/lib/libpakro_provider.so",
                    &provider,
                ),
            ] {
                let original = package.site_packages().join(path);
                let projected = layout.prefix().join("site-packages").join(path);
                let a = std::fs::metadata(&original).unwrap();
                let b = std::fs::metadata(&projected).unwrap();
                assert_ne!((a.dev(), a.ino()), (b.dev(), b.ino()));
                assert_eq!(a.nlink(), 1);
                assert_eq!(b.nlink(), 1);
                assert_eq!(std::fs::read(original).unwrap(), *bytes);
                assert_eq!(std::fs::read(projected).unwrap(), *bytes);
            }
            let sources = layout
                .packages()
                .iter()
                .map(|package| {
                    (
                        package.manifest().facts.clone(),
                        SharedRegistryPackageSource::Existing(package.clone()),
                    )
                })
                .collect();
            let again =
                prepare_layout_with_sources(&context, &language, layout.graph(), sources).unwrap();
            assert_eq!(layout.key(), again.key());
            assert_eq!(layout.prefix(), again.prefix());
            // Two real catalog fixture registrations select the two actual
            // preparation results. This verifies shared layout identity, not
            // a project install or a product compiler execution path.
            {
                use crate::libraries::project::repository::ProjectRepository;
                use crate::packages::python_environment::model::{
                    GenerationRecord, InputKind, InputReference, SelectionKey,
                };
                use crate::programs::{OperationContextInputs, capture_operation_context};
                let mut policy = *context.policy();
                policy.writes.project_selection = true;
                let selection_context = capture_operation_context(OperationContextInputs {
                    settings: context.settings(),
                    policy,
                    cancellation: tokio_util::sync::CancellationToken::new(),
                    data_root: context.requested_data_root().to_owned(),
                    cache_root: context.requested_cache_root().to_owned(),
                    python_sources: None,
                    python_target_description: None,
                })
                .unwrap();
                let mut repository = ProjectRepository::open_writer(&selection_context).unwrap();
                let writer = repository.writer().unwrap();
                let allocation = layout.prefix().file_name().unwrap().to_str().unwrap();
                let inputs = std::iter::once(InputReference::new(
                    InputKind::Language,
                    pakro_store::Digest::Sha256(layout.language()),
                ))
                .chain(layout.packages().iter().map(|package| {
                    InputReference::new(
                        InputKind::Package,
                        pakro_store::Digest::Sha256(package.manifest().key()),
                    )
                }))
                .collect();
                let record = GenerationRecord::new_shared(
                    allocation,
                    language.key().target(),
                    layout.graph(),
                    [198; 32],
                    layout.key(),
                    inputs,
                    repository.model().limits(),
                )
                .unwrap();
                repository
                    .model_mut()
                    .register_shared_generation(record)
                    .unwrap();
                let mut keys = Vec::new();
                for (index, selected) in [&layout, &again].into_iter().enumerate() {
                    let project = uuid::Uuid::from_u128(0x6e10 + index as u128);
                    let path = home.join(format!("project-{index}"));
                    std::fs::create_dir(&path).unwrap();
                    let declaration = b"[tools]\npython='3.15.0'\n";
                    std::fs::write(path.join("pakro.toml"), declaration).unwrap();
                    drop(
                        pakro_store::control::lease::StableLock::create_new(
                            writer.build_handle(),
                            &format!("project-{project}.lock"),
                            nix::unistd::geteuid().as_raw(),
                        )
                        .unwrap(),
                    );
                    repository
                        .model_mut()
                        .register_project(
                            project,
                            path.to_str().unwrap(),
                            &hex::encode(Sha256::digest(declaration)),
                        )
                        .unwrap();
                    let key = SelectionKey::new(
                        project,
                        "default",
                        language.key().target(),
                        repository.model().limits(),
                    )
                    .unwrap();
                    let expected = repository.model().snapshot(&key);
                    assert_eq!(selected.key(), layout.key());
                    repository
                        .model_mut()
                        .apply_selection(&key, &expected, Some(allocation), uuid::Uuid::now_v7())
                        .unwrap();
                    keys.push(key);
                }
                repository.commit_model().unwrap();
                drop((writer, repository));
                let repository = ProjectRepository::open_readonly(&selection_context).unwrap();
                for key in keys {
                    let selected = repository.model().snapshot(&key);
                    let record = repository
                        .model()
                        .generation(selected.generation().unwrap())
                        .unwrap();
                    assert_eq!(record.shared_layout(), Some(&layout.key()));
                }
                assert_eq!(
                    repository.read_views().reclamation_roots().unwrap().len(),
                    2
                );
                println!("P6_TWO_PROJECT_CATALOG_REUSE={}", hex::encode(layout.key()));
            }
            let output = python(
                &interpreter,
                home,
                "bootstrap",
                &again.prefix().join("bootstrap"),
                &reference,
            );
            assert!(output.status.success());
            assert!(!layout.prefix().join("lib").exists());
            assert!(!layout.prefix().join("include").exists());
            let runtimes = std::fs::read_dir(context.requested_data_root().join("languages"))
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("language-")
                })
                .count();
            assert_eq!(runtimes, 1);
            layout.check_binding().unwrap();
            again.check_binding().unwrap();
            language.check_binding().unwrap();
            println!(
                "P6_REAL_ELF_PROJECTION_PASSED method={expected} shared_layout={}",
                hex::encode(layout.key())
            );
        }
    }
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
