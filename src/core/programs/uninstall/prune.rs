//! Explicit reclamation planning. Descriptions never grant removal authority.
use crate::libraries::reclaim::{self, ReclaimId, ReclaimMode, ReclaimReport, ReclaimStatus};
use crate::programs::OperationContext;
use eyre::{Result, ensure};
use std::collections::BTreeSet;
use uuid::Uuid;

mod graph;
pub(super) mod view;

pub(crate) fn run(
    context: &OperationContext,
    dry_run: bool,
    resume: Option<Uuid>,
) -> Result<ReclaimReport> {
    if let Some(operation) = resume {
        return reclaim::resume(context, operation, dry_run);
    }
    let mut view = view::read(context, None, &[], true)?;
    if !view.complete {
        return Ok(report(view, dry_run, false));
    }
    let graph = view.graph()?;
    let retained = graph.closure(view.roots.keys().copied())?;
    let groups = graph.groups(&retained);
    let mut capacity = false;
    for group in &groups {
        if group.len() > reclaim::MAX_GROUP_OBJECTS {
            capacity = true;
            for &id in group {
                view.roots.insert(
                    id,
                    (
                        ReclaimStatus::Capacity,
                        format!(
                            "complete SCC needs {} object protections; limit is {}",
                            group.len(),
                            reclaim::MAX_GROUP_OBJECTS
                        ),
                    ),
                );
            }
        }
    }
    let retained = view.protect()?;
    let groups = graph.groups(&retained);
    let blocked = capacity
        || view.entries.values().any(|entry| {
            matches!(
                entry.status,
                ReclaimStatus::Busy
                    | ReclaimStatus::Damaged
                    | ReclaimStatus::Unknown
                    | ReclaimStatus::Capacity
            )
        });
    if dry_run || groups.is_empty() {
        return Ok(report(view, dry_run, !blocked));
    }
    // The kernel fixes this exact candidate collection in its own journal,
    // reacquires the whole group EX, then calls the fresh reference gate.
    let mut executed = reclaim::execute(context, ReclaimMode::Prune, &groups, false)?;
    let candidates: BTreeSet<_> = groups.iter().flatten().copied().collect();
    executed.entries.extend(
        view.entries
            .into_values()
            .filter(|entry| entry.object.is_none_or(|id| !candidates.contains(&id))),
    );
    executed.entries.extend(view.issues);
    executed.complete &= !blocked;
    executed.entries.sort_by(|left, right| {
        left.object
            .cmp(&right.object)
            .then_with(|| left.label.cmp(&right.label))
    });
    Ok(executed)
}

fn report(mut view: view::View, dry_run: bool, complete: bool) -> ReclaimReport {
    if !view.complete {
        for entry in view.entries.values_mut() {
            if entry.status == ReclaimStatus::Eligible {
                entry.status = ReclaimStatus::Unknown;
                entry
                    .reasons
                    .push("reclamation view is incomplete; no deletion is authorized".into());
            }
        }
    }
    let mut entries: Vec<_> = view.entries.into_values().collect();
    entries.extend(view.issues);
    ReclaimReport {
        operation_id: None,
        dry_run,
        resumed: false,
        complete: complete && view.complete,
        entries,
    }
}

/// The one explicit target plus adapters that would become invalid. Other
/// packages and languages are retained, including unrooted parents.
pub(crate) fn plan_exact_target(
    context: &OperationContext,
    target: ReclaimId,
) -> Result<Vec<Vec<ReclaimId>>> {
    let view = view::read(context, None, &[], false)?;
    ensure!(
        view.complete,
        "object references are unknown; reclamation view is incomplete: {}",
        view.issues
            .iter()
            .flat_map(|entry| entry.reasons.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
    ensure!(
        view.objects.contains_key(&target),
        "exact removal target has no published manifest"
    );
    let graph = view.graph()?;
    let roots = graph.closure(view.roots.keys().copied())?;
    ensure!(
        !roots.contains(&target),
        "object is still referenced:\n{}",
        view.roots
            .values()
            .map(|(_, reason)| reason.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let parents: Vec<_> = view
        .objects
        .keys()
        .copied()
        .filter(|id| {
            *id != target && matches!(id, ReclaimId::Package(_) | ReclaimId::Language(_))
        })
        .collect();
    let retained = graph.closure(parents.iter().copied())?;
    if retained.contains(&target) {
        let path = graph.reference_path(parents, &target)?.ok_or_else(|| {
            eyre::eyre!("retained package reference has no actual dependency path")
        })?;
        let chain = path
            .iter()
            .map(|id| view.entries[id].label.as_str())
            .collect::<Vec<_>>()
            .join(" -> ");
        eyre::bail!("object is still referenced: retained object {chain}");
    }
    let mut group = vec![target];
    for (&id, edges) in &view.objects {
        if matches!(id, ReclaimId::Layout(_)) && edges.contains(&target) {
            ensure!(
                !roots.contains(&id),
                "object is still referenced: selected or pending {}",
                id.label()
            );
            group.push(id);
        }
    }
    group.sort();
    group.dedup();
    ensure!(
        group.len() <= reclaim::MAX_GROUP_OBJECTS,
        "exact target and invalidated layouts exceed whole-group protection capacity"
    );
    Ok(vec![group])
}

#[cfg(test)]
mod tests;
