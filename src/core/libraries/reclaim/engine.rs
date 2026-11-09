use super::{object::*, *};
use crate::programs::uninstall::reachability;
use eyre::{ContextCompat, ensure};
use pakro_store::control::StoreWriter;
use pakro_store::control::lease::ObjectLease;
use pakro_store::control::{ControlNamespace, rename_noclobber};
use std::collections::BTreeSet;
use std::sync::Arc;
struct Maintenance {
    _writer: Arc<StoreWriter>,
    _held: ObjectLease,
}
fn maintenance(context: &OperationContext) -> Result<Option<Maintenance>> {
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids reclaim writes"
    );
    let writer =
        crate::libraries::project::repository::ProjectRepository::open_reclaim_journal_writer(
            context,
        )?;
    let Some(held) = writer.try_maintenance_exclusive()? else {
        return Ok(None);
    };
    Ok(Some(Maintenance {
        _writer: writer,
        _held: held,
    }))
}
#[cfg(test)]
fn maintenance_test_gate(context: &OperationContext) -> Result<()> {
    if let Some(path) = std::env::var_os("PAKRO_TEST_RECLAIM_HOLD_GATE") {
        let path = std::path::PathBuf::from(path);
        ensure!(
            context.requested_data_root().parent() == Some(path.as_path())
                && std::fs::read(path.join(".reclaim-test-owner"))?
                    == b"pakro-owned-reclaim-fixture-v1",
            "maintenance test gate is outside its owned fixture"
        );
        std::fs::write(path.join("coordinator-held"), b"held")?;
        while !path.join("coordinator-release").exists() {
            context.checkpoint()?;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    Ok(())
}
#[cfg(not(test))]
fn maintenance_test_gate(_: &OperationContext) -> Result<()> {
    Ok(())
}
fn report(dry_run: bool, complete: bool, entries: Vec<ReclaimEntry>) -> ReclaimReport {
    ReclaimReport {
        dry_run,
        complete,
        entries,
    }
}
fn failed(ids: &[ReclaimId], status: ReclaimStatus, reason: &str) -> Vec<ReclaimEntry> {
    ids.iter()
        .map(|id| super::entry(*id, status, reason))
        .collect()
}
fn reference_status(error: &eyre::Report) -> ReclaimStatus {
    if error
        .downcast_ref::<reachability::GroupReferenced>()
        .is_some()
    {
        ReclaimStatus::Protected
    } else {
        ReclaimStatus::Unknown
    }
}
fn with_remaining(
    entries: &[ReclaimEntry],
    ids: &[ReclaimId],
    status: ReclaimStatus,
    reason: &str,
) -> Vec<ReclaimEntry> {
    let removed = entries
        .iter()
        .filter(|row| row.status == ReclaimStatus::Removed)
        .filter_map(|row| row.object)
        .collect::<BTreeSet<_>>();
    let mut result = entries
        .iter()
        .filter(|row| row.status == ReclaimStatus::Removed)
        .cloned()
        .collect::<Vec<_>>();
    result.extend(
        ids.iter()
            .filter(|id| !removed.contains(id))
            .map(|id| super::entry(*id, status, reason)),
    );
    result
}
fn verify_group(
    context: &OperationContext,
    records: &[Record],
    stores: &mut Stores,
    writable: bool,
) -> Result<Option<Vec<OwnedObject>>> {
    let mut owners = Vec::new();
    let mut order = (0..records.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| records[*index].object);
    for index in order {
        let record = &records[index];
        let store = store(context, stores, record.object, writable)?;
        let Some(owner) = OwnedObject::acquire(context, store, record.clone())? else {
            return Ok(None);
        };
        let started = std::time::Instant::now();
        owner.verify(context)?;
        debug!(
            "reclaim full verification {}: {:.6}s",
            record.object.label(),
            started.elapsed().as_secs_f64()
        );
        owners.push((index, owner));
    }
    owners.sort_by_key(|(index, _)| *index);
    Ok(Some(owners.into_iter().map(|(_, owner)| owner).collect()))
}
fn members(records: &[Record]) -> Vec<(ReclaimId, Vec<ReclaimId>)> {
    records
        .iter()
        .map(|r| (r.object, r.dependencies.clone()))
        .collect()
}
pub(super) fn execute(
    context: &OperationContext,
    mode: ReclaimMode,
    groups: &[Vec<ReclaimId>],
    dry_run: bool,
) -> Result<ReclaimReport> {
    context.checkpoint()?;
    let ids = groups.iter().flatten().copied().collect::<Vec<_>>();
    ensure!(
        ids.iter().copied().collect::<BTreeSet<_>>().len() == ids.len(),
        "duplicate reclaim objects"
    );
    if let Some(group) = groups.iter().find(|group| group.len() > MAX_GROUP_OBJECTS) {
        return Ok(report(
            dry_run,
            false,
            failed(
                group,
                ReclaimStatus::Capacity,
                "whole dependency group exceeds the 16-object protection capacity",
            ),
        ));
    }
    if ids.is_empty() {
        return Ok(report(dry_run, true, Vec::new()));
    }
    ensure!(
        groups.iter().all(|group| !group.is_empty()),
        "empty reclaim group"
    );
    let _coordinator = if dry_run {
        None
    } else {
        let Some(coordinator) = maintenance(context)? else {
            return Ok(report(
                false,
                false,
                failed(
                    &ids,
                    ReclaimStatus::Busy,
                    "another reclaim operation is running",
                ),
            ));
        };
        maintenance_test_gate(context)?;
        Some(coordinator)
    };
    let mut stores = Stores::new();
    let mut entries = Vec::new();
    for group in groups {
        let mut records = Vec::new();
        for id in group {
            let store = store(context, &mut stores, *id, !dry_run)?;
            records.push(describe(context, &store, *id)?);
        }
        let mut owners = match verify_group(context, &records, &mut stores, !dry_run) {
            Ok(Some(owners)) => owners,
            Ok(None) => {
                return Ok(report(
                    dry_run,
                    false,
                    with_remaining(
                        &entries,
                        &ids,
                        ReclaimStatus::Busy,
                        "whole dependency group is busy; later dependencies retained",
                    ),
                ));
            }
            Err(error) => {
                return Ok(report(
                    dry_run,
                    false,
                    with_remaining(
                        &entries,
                        &ids,
                        failure_status(&error),
                        &format!("{error:#}"),
                    ),
                ));
            }
        };
        if let Err(error) = reachability::recheck_group(context, mode, &members(&records)) {
            return Ok(report(
                dry_run,
                false,
                with_remaining(
                    &entries,
                    &ids,
                    reference_status(&error),
                    &format!("fresh group references refused: {error:#}"),
                ),
            ));
        }
        if dry_run {
            entries.extend(failed(
                group,
                ReclaimStatus::Eligible,
                "complete candidate; deletion rechecks references",
            ));
            continue;
        }
        // Make every member unavailable before deleting any member of the group.
        for owner in &mut owners {
            let control = owner.store.writer()?.control(Default::default())?;
            if !owner.record.pending {
                let name = manifest_name(owner.record.object);
                let ready = control
                    .read(ControlNamespace::Catalog, &name)?
                    .context("reclaim Ready manifest disappeared")?;
                ensure!(
                    ready.bytes() == owner.record.manifest
                        && pair_snapshot(&ready) == owner.record.manifest_identity,
                    "reclaim original manifest changed before deletion"
                );
                rename_noclobber(
                    control.namespace_handle(ControlNamespace::Catalog),
                    name.as_ref(),
                    control.namespace_handle(ControlNamespace::Journals),
                    claim_name(owner.record.object).as_ref(),
                )?;
                #[cfg(test)]
                super::tests::mark_event(context, owner, "renamed")?;
            }
            control.sync_namespace(ControlNamespace::Journals)?;
            control.sync_namespace(ControlNamespace::Catalog)?;
            owner.record.pending = true;
            #[cfg(test)]
            super::tests::mark_event(context, owner, "synced")?;
        }
        for owner in &owners {
            super::tree::remove(owner, context)?;
            entries.push(super::entry(
                owner.record.object,
                ReclaimStatus::Removed,
                "original allocation removed",
            ));
        }
    }
    if !dry_run {
        reachability::cleanup_retired(context, &ids)?;
        for id in &ids {
            let store = store(context, &mut stores, *id, true)?;
            let mut control = store.writer()?.control(Default::default())?;
            if let Some(marker) = control.read(ControlNamespace::Journals, &claim_name(*id))? {
                control.remove(&marker)?;
            }
        }
    }
    Ok(report(dry_run, true, entries))
}
