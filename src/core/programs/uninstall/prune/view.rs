//! One bounded descriptive view. Only the deletion kernel can grant authority.
use super::graph::Graph;
use crate::libraries::package::manifest::{self, LayoutManifest, PackageManifest};
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::reclaim::{self, ReclaimEntry, ReclaimId, ReclaimStatus};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, ReadStore, RootLimits, open_directory,
    open_trusted_store_with_deadline,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use uuid::Uuid;

pub(in crate::programs::uninstall) const MAX_NODES: usize = 65536;
pub(in crate::programs::uninstall) const MAX_EDGES: usize = 1_048_576;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub(in crate::programs::uninstall) struct View {
    pub objects: BTreeMap<ReclaimId, Vec<ReclaimId>>,
    pub entries: BTreeMap<ReclaimId, ReclaimEntry>,
    pub issues: Vec<ReclaimEntry>,
    pub roots: BTreeMap<ReclaimId, (ReclaimStatus, String)>,
    pub complete: bool,
}
impl View {
    fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            entries: BTreeMap::new(),
            issues: Vec::new(),
            roots: BTreeMap::new(),
            complete: true,
        }
    }
    fn unknown(&mut self, label: impl Into<String>, error: impl std::fmt::Display) {
        self.complete = false;
        self.issues.push(ReclaimEntry {
            object: None,
            label: label.into(),
            status: ReclaimStatus::Unknown,
            reasons: vec![error.to_string()],
        });
    }
    fn insert(&mut self, id: ReclaimId, mut edges: Vec<ReclaimId>, label: String) -> Result<()> {
        ensure!(
            self.objects.len() < MAX_NODES || self.objects.contains_key(&id),
            "reclamation graph node budget exceeded"
        );
        edges.sort();
        edges.dedup();
        if let Some(previous) = self.objects.get(&id) {
            ensure!(
                *previous == edges,
                "conflicting recorded reclamation edges for {}",
                id.label()
            );
        }
        self.objects.insert(id, edges);
        self.entries.entry(id).or_insert(ReclaimEntry {
            object: Some(id),
            label,
            status: ReclaimStatus::Eligible,
            reasons: Vec::new(),
        });
        Ok(())
    }
    pub(in crate::programs::uninstall) fn graph(&self) -> Result<Graph<ReclaimId>> {
        Graph::new(self.objects.clone(), MAX_NODES, MAX_EDGES)
    }
    pub(in crate::programs::uninstall) fn protect(&mut self) -> Result<BTreeSet<ReclaimId>> {
        let mut pending = VecDeque::new();
        let mut reached = BTreeSet::new();
        for (&id, (status, reason)) in &self.roots {
            ensure!(
                self.objects.contains_key(&id),
                "protected {} has no verified object record",
                id.label()
            );
            reached.insert(id);
            pending.push_back((id, id));
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.status = *status;
                entry.reasons.push(reason.clone());
            }
        }
        while let Some((id, source)) = pending.pop_front() {
            for &dependency in &self.objects[&id] {
                if reached.insert(dependency) {
                    let entry = self
                        .entries
                        .get_mut(&dependency)
                        .context("dependency has no description")?;
                    entry.status = ReclaimStatus::Protected;
                    entry.reasons.push(format!(
                        "required by {}: {}",
                        source.label(),
                        self.roots[&source].1
                    ));
                    pending.push_back((dependency, source));
                }
            }
        }
        Ok(reached)
    }
}

/// Supplemental members are supplied only from the kernel's checked original
/// manifests. No SH is reacquired for them during a group EX recheck.
pub(in crate::programs::uninstall) fn read(
    context: &OperationContext,
    own_op: Option<Uuid>,
    supplemental: &[(ReclaimId, Vec<ReclaimId>)],
    probe: bool,
) -> Result<View> {
    context.checkpoint()?;
    let mut view = View::new();
    let mut known: BTreeMap<&str, BTreeSet<String>> = BTreeMap::from([
        ("languages", BTreeSet::new()),
        ("packages", BTreeSet::new()),
    ]);
    let mut journals: BTreeMap<&str, BTreeSet<String>> = BTreeMap::from([
        ("projects", BTreeSet::new()),
        ("languages", BTreeSet::new()),
        ("packages", BTreeSet::new()),
    ]);
    let pending = match reclaim::pending(context) {
        Ok(value) => value,
        Err(error) => {
            context.checkpoint()?;
            view.unknown("pending deletion records", format!("{error:#}"));
            Vec::new()
        }
    };
    for operation in &pending {
        if operation.complete || Some(operation.operation_id) == own_op {
            continue;
        }
        for &(id, ref edges) in &operation.members {
            if let Err(error) = view.insert(id, edges.clone(), id.label()) {
                view.unknown(id.label(), error);
            }
        }
        for &id in &operation.objects {
            view.roots.insert(
                id,
                (
                    ReclaimStatus::PendingRecovery,
                    format!("pending deletion operation {}", operation.operation_id),
                ),
            );
        }
    }
    // Include our own operation's directories in namespace accounting, while
    // excluding only its roots. Its checked members arrive as supplemental.
    for operation in &pending {
        if operation.complete {
            continue;
        }
        for (id, name) in &operation.directories {
            known
                .get_mut(namespace(*id))
                .expect("fixed namespace")
                .insert(name.clone());
        }
    }
    for operation in &pending {
        for (namespace, name) in &operation.journals {
            if let Some(known) = journals.get_mut(namespace.as_str()) {
                known.insert(name.clone());
            } else {
                view.unknown(
                    "pending deletion namespace",
                    "deletion projection names an unsupported library",
                );
            }
        }
    }
    for (id, edges) in supplemental {
        if let Err(error) = view.insert(*id, edges.clone(), id.label()) {
            view.unknown(id.label(), error);
        }
    }
    match ProjectRepository::open_readonly(context)
        .and_then(|repository| repository.read_views().reclamation_roots())
    {
        Ok(roots) => {
            for root in roots {
                let reason = format!(
                    "project {} at {} group {} target {} revision {}",
                    root.project,
                    root.root.display(),
                    root.group,
                    hex::encode(root.target),
                    root.revision
                );
                let mut objects = vec![ReclaimId::Language(root.language)];
                if let Some(layout) = root.layout {
                    objects.push(ReclaimId::Layout(layout));
                }
                objects.extend(root.packages.into_iter().map(ReclaimId::Package));
                for id in objects {
                    view.roots
                        .entry(id)
                        .or_insert((ReclaimStatus::Protected, reason.clone()));
                }
            }
        }
        Err(error) => {
            context.checkpoint()?;
            view.unknown("project selections", format!("{error:#}"));
        }
    }
    let p2 = match crate::programs::shared::lock_transaction::pending_protections(context) {
        Ok(value) => value,
        Err(error) => {
            context.checkpoint()?;
            view.unknown("pending project commits", format!("{error:#}"));
            Vec::new()
        }
    };
    for pending in &p2 {
        journals
            .get_mut("projects")
            .expect("fixed namespace")
            .insert(pending.journal.clone());
    }
    match crate::libraries::language::recovery::pending_protections(context) {
        Ok(roots) => {
            for pending in roots {
                journals
                    .get_mut("languages")
                    .expect("fixed namespace")
                    .extend(pending.journals);
                view.roots.insert(
                    ReclaimId::Language(pending.language),
                    (
                        ReclaimStatus::PendingRecovery,
                        "pending bytecode recovery".into(),
                    ),
                );
            }
        }
        Err(error) => {
            context.checkpoint()?;
            view.unknown("pending bytecode recovery", format!("{error:#}"));
        }
    }
    for name in ["languages", "packages"] {
        if let Err(error) = snapshot_namespace(
            context,
            name,
            &mut view,
            known.get_mut(name).expect("fixed namespace"),
            &journals[name],
        ) {
            context.checkpoint()?;
            view.unknown(format!("{name} library"), format!("{error:#}"));
        }
    }
    if let Err(error) = check_project_journals(context, &journals["projects"], &mut view) {
        context.checkpoint()?;
        view.unknown("project journals", format!("{error:#}"));
    }
    for operation in p2 {
        let digests: BTreeSet<_> = operation.references.into_iter().collect();
        for &id in view.objects.keys() {
            if digests.contains(&id.key()) {
                view.roots.insert(
                    id,
                    (
                        ReclaimStatus::PendingRecovery,
                        format!(
                            "pending project {} transaction {}",
                            operation.project, operation.transaction
                        ),
                    ),
                );
            }
        }
    }
    for &root in view.roots.keys() {
        if !view.objects.contains_key(&root) {
            // Preserve a useful row even when the required Ready record is absent.
            view.issues.push(ReclaimEntry {
                object: Some(root),
                label: root.label(),
                status: ReclaimStatus::Unknown,
                reasons: vec!["protected object has no verified manifest".into()],
            });
            view.complete = false;
        }
    }
    if let Err(error) = view.graph() {
        view.unknown("reclamation dependency graph", error);
    }
    if view.complete {
        let rooted = view.graph()?.closure(view.roots.keys().copied())?;
        if probe {
            let ids: Vec<_> = view
                .objects
                .keys()
                .copied()
                .filter(|id| !rooted.contains(id))
                .collect();
            for id in ids {
                context.checkpoint()?;
                let row = match reclaim::probe(context, id) {
                    Ok(row) => row,
                    Err(error) => {
                        context.checkpoint()?;
                        ReclaimEntry {
                            object: Some(id),
                            label: id.label(),
                            status: ReclaimStatus::Unknown,
                            reasons: vec![format!("{error:#}")],
                        }
                    }
                };
                match row.status {
                    ReclaimStatus::Eligible => {}
                    ReclaimStatus::Busy
                    | ReclaimStatus::Damaged
                    | ReclaimStatus::PendingRecovery
                    | ReclaimStatus::Protected => {
                        view.roots.insert(id, (row.status, row.reasons.join("; ")));
                    }
                    _ => {
                        view.complete = false;
                    }
                }
                view.entries.insert(id, row);
            }
        }
        if view.complete {
            view.protect()?;
        }
    }
    context.checkpoint()?;
    Ok(view)
}

fn namespace(id: ReclaimId) -> &'static str {
    match id {
        ReclaimId::Language(_) => "languages",
        _ => "packages",
    }
}

fn snapshot_namespace(
    context: &OperationContext,
    namespace: &str,
    view: &mut View,
    known: &mut BTreeSet<String>,
    journals: &BTreeSet<String>,
) -> Result<()> {
    let Some(read) = ReadStore::open_readonly(
        &context.requested_data_root().join(namespace),
        nix::unistd::geteuid().as_raw(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(());
    };
    let control = read.control(ControlLimits::default())?;
    let store = open_trusted_store_with_deadline(
        read.root_path(),
        nix::unistd::geteuid().as_raw(),
        false,
        context.policy().deadline,
    )?;
    let identity = FileIdentity::of(&store)?;
    ensure!(
        control.store_identity()? == (identity.device as u64, identity.inode as u64),
        "library namespace identity changed"
    );
    let mut bytes = 0usize;
    for name in control.list_names(ControlNamespace::Catalog, MAX_NODES, 8 * 1024 * 1024)? {
        context.checkpoint()?;
        let snapshot = control
            .read(ControlNamespace::Catalog, &name)?
            .context("reclamation manifest disappeared under catalog protection")?;
        bytes = bytes
            .checked_add(snapshot.bytes().len())
            .filter(|total| *total <= MAX_BYTES)
            .context("reclamation metadata byte budget exceeded")?;
        let result = (|| -> Result<()> {
            if namespace == "languages" {
                let (key, dependencies) = crate::libraries::language::manifest::validate_published_metadata(
                    snapshot.bytes(),
                    &name,
                    read.root_path(),
                )?;
                let id = ReclaimId::Language(key);
                known.insert(id.label());
                view.insert(id, dependencies.into_iter().map(ReclaimId::Package).collect(), id.label())?;
            } else if name.starts_with("package-") {
                let package: PackageManifest = serde_json::from_slice(snapshot.bytes())?;
                package.validate_metadata()?;
                ensure!(
                    name == manifest::package_name(&package.key())
                        && package.storage.key == package.key(),
                    "package manifest identity differs"
                );
                known.insert(package.storage.allocation.clone());
                let id = ReclaimId::Package(package.key());
                // A revoked/partially removed group is described by its checked
                // original manifest. Never acquire its own object SH here.
                if !view.objects.contains_key(&id) {
                    let root = open_directory(&store, package.storage.allocation.as_str())?;
                    crate::libraries::package::lease::validate_package_facts(&root, &package)?;
                }
                let mut edges: Vec<_> = package.language.into_iter().map(ReclaimId::Language).collect();
                edges.extend(
                    package
                        .facts
                        .dependencies
                        .iter()
                        .copied()
                        .map(ReclaimId::Package),
                );
                view.insert(
                    id,
                    edges,
                    format!("{} ({}=={})", id.label(), package.name(), package.version()),
                )?;
            } else if name.starts_with("layout-") {
                let layout: LayoutManifest = serde_json::from_slice(snapshot.bytes())?;
                layout.validate_metadata()?;
                let id = ReclaimId::Layout(layout.storage.key);
                ensure!(
                    name == manifest::layout_name(&layout.storage.key),
                    "layout manifest identity differs"
                );
                known.insert(layout.storage.allocation.clone());
                let mut edges = vec![ReclaimId::Language(layout.language)];
                edges.extend(layout.packages.iter().copied().map(ReclaimId::Package));
                view.insert(id, edges, id.label())?;
            } else {
                eyre::bail!("unknown Ready metadata record");
            }
            Ok(())
        })();
        if let Err(error) = result {
            view.unknown(format!("{namespace}/{name}"), format!("{error:#}"));
        }
    }
    // Metadata protects known namespace entries while they are accounted for;
    // an untracked directory is never promoted into a removal candidate.
    scan_unknown(context, namespace, &store, known, view)?;
    check_journals(context, namespace, &control, journals, view)?;
    read.check_binding()?;
    control.check_binding()?;
    Ok(())
}

fn check_project_journals(
    context: &OperationContext,
    known: &BTreeSet<String>,
    view: &mut View,
) -> Result<()> {
    let Some(read) = ReadStore::open_readonly(
        &context.requested_data_root().join("projects"),
        nix::unistd::geteuid().as_raw(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(());
    };
    let control = read.control(ControlLimits::default())?;
    check_journals(context, "projects", &control, known, view)
}
fn check_journals(
    context: &OperationContext,
    namespace: &str,
    control: &pakro_store::control::ReadControl<'_>,
    known: &BTreeSet<String>,
    view: &mut View,
) -> Result<()> {
    for name in control.list_names(ControlNamespace::Journals, MAX_NODES, 8 * 1024 * 1024)? {
        context.checkpoint()?;
        if !known.contains(&name) {
            view.unknown(
                format!("{namespace}/journals/{name}"),
                "unrecognized operation record; protection roots are unknown",
            );
        }
    }
    Ok(())
}

fn scan_unknown(
    context: &OperationContext,
    namespace: &str,
    store: &File,
    known: &BTreeSet<String>,
    view: &mut View,
) -> Result<()> {
    let mut directory = nix::dir::Dir::openat(
        store,
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let mut count = 0usize;
    let mut total = 0usize;
    for entry in directory.iter() {
        context.checkpoint()?;
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        count += 1;
        total = total
            .checked_add(raw.len())
            .context("namespace byte budget overflow")?;
        ensure!(
            count <= MAX_NODES && total <= 8 * 1024 * 1024,
            "reclamation namespace budget exceeded"
        );
        let name = std::str::from_utf8(raw)
            .map_err(|_| eyre::eyre!("unknown non-UTF8 managed-library entry"))?;
        if name == ".quarantine" {
            let quarantine = open_directory(store, name)?;
            let mut captures = nix::dir::Dir::openat(
                &quarantine,
                ".",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            for capture in captures.iter() {
                context.checkpoint()?;
                let capture = capture?;
                let raw = capture.file_name().to_bytes();
                if matches!(raw, b"." | b"..") {
                    continue;
                }
                count += 1;
                total = total
                    .checked_add(raw.len())
                    .context("namespace byte budget overflow")?;
                ensure!(
                    count <= MAX_NODES && total <= 8 * 1024 * 1024,
                    "reclamation namespace budget exceeded"
                );
                let capture = std::str::from_utf8(raw)
                    .map_err(|_| eyre::eyre!("unknown non-UTF8 quarantine entry"))?;
                let relative = format!(".quarantine/{capture}");
                if !known.contains(&relative) {
                    view.unknown(
                        format!("{namespace}/{relative}"),
                        "quarantined entry has no owned deletion record",
                    );
                }
            }
            continue;
        }
        if matches!(name, ".allocation.lock" | ".control" | ".leases") || known.contains(name) {
            continue;
        }
        view.unknown(
            format!("{namespace}/{name}"),
            "untracked entry has no original manifest or owned deletion record",
        );
    }
    Ok(())
}
