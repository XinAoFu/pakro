//! Current project chains and retained physical content protect targets.
use crate::libraries::package::manifest::{self, LayoutManifest, PackageManifest};
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::reclaim::{self, ReclaimId, ReclaimMode};
use crate::programs::OperationContext;
use eyre::{Result, ensure};
use pakro_store::control::{ControlLimits, ControlNamespace, ReadStore, WriterControl};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

fn project_references(context: &OperationContext, target: [u8; 32]) -> Result<BTreeSet<String>> {
    let repository = ProjectRepository::open_readonly(context)?;
    project_references_from_snapshot(context, target, &repository)
}
fn project_references_from_snapshot(
    context: &OperationContext,
    target: [u8; 32],
    repository: &ProjectRepository,
) -> Result<BTreeSet<String>> {
    let mut references: BTreeSet<String> = repository
        .read_views()
        .reference_chains(target)?
        .into_iter()
        .collect();
    references.extend(
        crate::programs::shared::lock_transaction::pending_reference_chains(context, target)?,
    );
    Ok(references)
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
    let repository = ProjectRepository::open_readonly(context)?;
    references_with_project_snapshot(context, target, &repository)
}
/// Inventory rows use one committed catalog rather than combining a
/// project's old and new selections across separate per-object reads.
pub(crate) fn references_with_project_snapshot(
    context: &OperationContext,
    target: [u8; 32],
    repository: &ProjectRepository,
) -> Result<Vec<String>> {
    let mut users = project_references_from_snapshot(context, target, repository)?;
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

/// A typed, definite reference refusal, distinct from unknown/damaged facts.
#[derive(Debug)]
pub(crate) struct GroupReferenced {
    pub reasons: Vec<String>,
}
impl std::fmt::Display for GroupReferenced {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "object is still referenced:\n{}",
            self.reasons.join("\n")
        )
    }
}
impl std::error::Error for GroupReferenced {}

/// With all group members EX, this is the final reference gate. The kernel
/// also uses the same descriptive check for its whole fixed set at admission;
/// that earlier check grants no permission. No member SH is acquired here.
pub(crate) fn recheck_group(
    context: &OperationContext,
    _mode: ReclaimMode,
    members: &[(ReclaimId, Vec<ReclaimId>)],
    own_op: Option<Uuid>,
) -> Result<()> {
    ensure!(!members.is_empty(), "empty reclamation group");
    let targets: BTreeSet<_> = members.iter().map(|(id, _)| *id).collect();
    ensure!(
        targets.len() == members.len(),
        "duplicate reclamation group member"
    );
    let view = super::prune::view::read(context, own_op, members, false)?;
    ensure!(
        view.complete,
        "object references are unknown; fresh reclamation view is incomplete: {}",
        view.issues
            .iter()
            .flat_map(|entry| entry.reasons.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
    let graph = view.graph()?;
    let rooted = graph.closure(view.roots.keys().copied())?;
    if !targets.is_disjoint(&rooted) {
        let reasons = targets
            .intersection(&rooted)
            .take(16)
            .map(|id| format!("{}: {}", id.label(), view.entries[id].reasons.join("; ")))
            .collect();
        return Err(GroupReferenced { reasons }.into());
    }
    // Everything outside this currently locked group remains an upper-level
    // referrer, even if it was absent from an earlier preview or is itself
    // planned for a later group. This also protects new derived layouts.
    let external = view
        .objects
        .keys()
        .copied()
        .filter(|id| !targets.contains(id));
    let retained = graph.closure(external)?;
    if !targets.is_disjoint(&retained) {
        let mut reasons = Vec::new();
        'referrers: for (id, edges) in &view.objects {
            if targets.contains(id) {
                continue;
            }
            for edge in edges {
                if targets.contains(edge) {
                    reasons.push(format!("retained {} -> {}", id.label(), edge.label()));
                    if reasons.len() == 16 {
                        break 'referrers;
                    }
                }
            }
        }
        return Err(GroupReferenced { reasons }.into());
    }
    context.checkpoint()?;
    Ok(())
}

/// Forget only indexes for physically removed layouts. Catalog CAS and the
/// model's own active-slot check preserve revisions, tombstones and replay.
pub(crate) fn cleanup_retired(
    context: &OperationContext,
    removed: &[ReclaimId],
    own_operation: Option<Uuid>,
) -> Result<()> {
    let layouts: BTreeSet<_> = removed
        .iter()
        .filter_map(|id| {
            if let ReclaimId::Layout(key) = id {
                Some(*key)
            } else {
                None
            }
        })
        .collect();
    if layouts.is_empty() {
        return Ok(());
    }
    let mut protected = BTreeSet::new();
    for pending in crate::programs::shared::lock_transaction::pending_protections(context)? {
        protected.extend(pending.references);
    }
    protected.extend(
        crate::libraries::language::recovery::pending_protections(context)?
            .into_iter()
            .map(|pending| pending.language),
    );
    for pending in reclaim::pending(context)? {
        if !pending.complete && Some(pending.operation_id) != own_operation {
            protected.extend(pending.objects.into_iter().map(ReclaimId::key));
            for (_, edges) in pending.members {
                protected.extend(edges.into_iter().map(ReclaimId::key));
            }
        }
    }
    let read = ProjectRepository::open_readonly(context)?;
    let mut allocations = Vec::new();
    for allocation in read.model().retired_generations() {
        let generation = read
            .model()
            .generation(allocation)
            .expect("enumerated generation");
        let Some(layout) = generation.shared_layout() else {
            continue;
        };
        if !layouts.contains(layout) || protected.contains(layout) {
            continue;
        }
        let pending_input = generation.inputs().iter().any(|input| {
            match input.digest().parse::<pakro_store::Digest>() {
                Ok(pakro_store::Digest::Sha256(key)) => protected.contains(&key),
                _ => false,
            }
        });
        if !pending_input {
            allocations.push(allocation.to_owned());
        }
    }
    if allocations.is_empty() {
        return Ok(());
    }
    // Only retirement is admitted under the original library-write authority.
    // The fresh active-slot guard and CAS never grant project-selection IO.
    ProjectRepository::forget_reclaimed_retired(context, &allocations)
}
