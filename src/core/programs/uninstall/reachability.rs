//! Current project chains and retained physical content protect targets.
use crate::libraries::package::manifest::{self, LayoutManifest, PackageManifest};
use crate::libraries::project::repository::ProjectRepository;
use crate::programs::OperationContext;
use eyre::{Result, ensure};
use pakro_store::control::{ControlLimits, ControlNamespace, ReadStore, WriterControl};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn project_references(context: &OperationContext, target: [u8; 32]) -> Result<BTreeSet<String>> {
    Ok(ProjectRepository::open_readonly(context)?
        .read_views()
        .reference_chains(target)?
        .into_iter()
        .collect())
}

fn retained_references(
    context: &OperationContext,
    target: [u8; 32],
    snapshots: Vec<(String, Vec<u8>)>,
    store: &std::fs::File,
) -> Result<BTreeSet<String>> {
    let mut packages = Vec::new();
    for (name, bytes) in snapshots {
        context.checkpoint()?;
        if name.starts_with("package-") {
            let package: PackageManifest = serde_json::from_slice(&bytes)?;
            package.storage.validate()?;
            ensure!(
                name == manifest::package_name(&package.key())
                    && package.storage.key == package.key(),
                "retained package identity is damaged"
            );
            let root =
                pakro_store::control::open_directory(store, package.storage.allocation.as_str())?;
            crate::libraries::package::lease::validate_package_facts(&root, &package)?;
            packages.push(package);
        } else if name.starts_with("layout-") {
            let layout: LayoutManifest = serde_json::from_slice(&bytes)?;
            layout.storage.validate()?;
            ensure!(
                name == manifest::layout_name(&layout.storage.key),
                "retained layout identity is damaged"
            );
            // Layouts are derived adapters. Current selections protect their
            // actual contents through the project graph; superseded adapters
            // alone must not keep every historical package alive.
        }
    }
    let mut users = BTreeSet::new();
    let by_key: BTreeMap<_, _> = packages
        .iter()
        .map(|package| (package.key(), package))
        .collect();
    for package in &packages {
        context.checkpoint()?;
        if package.language == target {
            users.insert(format!(
                "retained package {}=={} -> Python language-{}",
                package.name(),
                package.version(),
                hex::encode(target)
            ));
        }
        if package.key() == target {
            continue;
        }
        let mut visited = BTreeSet::new();
        let mut pending = VecDeque::from([(package.key(), String::from("retained package"))]);
        while let Some((key, chain)) = pending.pop_front() {
            context.checkpoint()?;
            if !visited.insert(key) {
                continue;
            }
            let node = by_key.get(&key).ok_or_else(|| {
                eyre::eyre!(
                    "retained package dependency package-{} is missing; references are unknown",
                    hex::encode(key)
                )
            })?;
            let chain = format!("{chain} -> {}=={}", node.name(), node.version());
            if key == target {
                users.insert(chain.clone());
            }
            for dependency in &node.facts.dependencies {
                pending.push_back((*dependency, chain.clone()));
            }
        }
    }
    Ok(users)
}

pub(crate) fn references(context: &OperationContext, target: [u8; 32]) -> Result<Vec<String>> {
    let mut users = project_references(context, target)?;
    if let Some(read) = ReadStore::open_readonly(
        &manifest::store_path(context),
        crate::libraries::package::lease::uid(),
        Default::default(),
        context.policy().deadline,
    )? {
        let control = read.control(ControlLimits::default())?;
        let store = pakro_store::control::open_trusted_store_with_deadline(
            read.root_path(),
            crate::libraries::package::lease::uid(),
            false,
            context.policy().deadline,
        )?;
        let identity = pakro_store::control::FileIdentity::of(&store)?;
        ensure!(
            (identity.device as u64, identity.inode as u64) == control.store_identity()?,
            "package reference store identity changed"
        );
        let mut snapshots = Vec::new();
        let mut bytes_read = 0usize;
        for name in control.list_names(ControlNamespace::Catalog, 65536, 8 * 1024 * 1024)? {
            if !name.starts_with("package-") && !name.starts_with("layout-") {
                continue;
            }
            context.checkpoint()?;
            let snapshot = control
                .read(ControlNamespace::Catalog, &name)?
                .ok_or_else(|| {
                    eyre::eyre!("retained content disappeared during reference query")
                })?;
            bytes_read = bytes_read
                .checked_add(snapshot.bytes().len())
                .filter(|total| *total <= 64 * 1024 * 1024)
                .ok_or_else(|| eyre::eyre!("reference query byte budget exceeded"))?;
            snapshots.push((name, snapshot.bytes().to_owned()));
        }
        users.extend(retained_references(context, target, snapshots, &store)?);
    }
    Ok(users.into_iter().collect())
}

/// Reuses the caller's already held packages catalog EX. Opening that same
/// catalog through a second fd here would self-deadlock.
pub(crate) fn ensure_unreferenced_with_package_control(
    context: &OperationContext,
    target: [u8; 32],
    control: &WriterControl<'_>,
) -> Result<()> {
    let mut users = project_references(context, target)?;
    let mut snapshots = Vec::new();
    let mut bytes_read = 0usize;
    for name in control.list_names(ControlNamespace::Catalog, 65536, 8 * 1024 * 1024)? {
        if !name.starts_with("package-") && !name.starts_with("layout-") {
            continue;
        }
        context.checkpoint()?;
        let snapshot = control
            .read(ControlNamespace::Catalog, &name)?
            .ok_or_else(|| eyre::eyre!("retained content disappeared under catalog protection"))?;
        bytes_read = bytes_read
            .checked_add(snapshot.bytes().len())
            .filter(|total| *total <= 64 * 1024 * 1024)
            .ok_or_else(|| eyre::eyre!("reference query byte budget exceeded"))?;
        snapshots.push((name, snapshot.bytes().to_owned()));
    }
    users.extend(retained_references(
        context,
        target,
        snapshots,
        control.root_handle(),
    )?);
    ensure!(
        users.is_empty(),
        "object is still referenced:\n{}",
        users.into_iter().collect::<Vec<_>>().join("\n")
    );
    Ok(())
}

pub(crate) fn ensure_unreferenced(context: &OperationContext, target: [u8; 32]) -> Result<()> {
    let users = references(context, target)?;
    ensure!(
        users.is_empty(),
        "object is still referenced:\n{}",
        users.join("\n")
    );
    Ok(())
}
