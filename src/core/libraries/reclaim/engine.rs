use super::{
    journal::{Cursor, Session, durable},
    object::*,
    *,
};
use crate::programs::uninstall::reachability;
use eyre::{ContextCompat, ensure};
use pakro_store::control::StoreWriter;
use pakro_store::control::lease::ObjectLease;
use std::collections::BTreeSet;
use std::sync::Arc;
struct Maintenance {
    writer: Arc<StoreWriter>,
    _held: ObjectLease,
}
fn maintenance(context: &OperationContext, fresh: bool) -> Result<Option<Maintenance>> {
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids reclaim writes"
    );
    let writer = if fresh {
        crate::libraries::project::repository::ProjectRepository::open_reclaim_journal_writer(
            context,
        )?
    } else {
        Arc::new(StoreWriter::open_existing(
            &context.requested_data_root().join("projects"),
            uid(),
            Default::default(),
            context.policy().deadline,
        )?)
    };
    let Some(held) = writer.try_maintenance_exclusive()? else {
        return Ok(None);
    };
    Ok(Some(Maintenance {
        writer,
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
fn report(
    operation_id: Option<Uuid>,
    dry_run: bool,
    resumed: bool,
    complete: bool,
    entries: Vec<ReclaimEntry>,
) -> ReclaimReport {
    ReclaimReport {
        operation_id,
        dry_run,
        resumed,
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
    cursors: &[Cursor],
    stores: &mut Stores,
    writable: bool,
) -> Result<Option<Vec<OwnedObject>>> {
    let mut owners = Vec::new();
    let mut order = (0..records.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| records[*index].object);
    for index in order {
        let record = &records[index];
        let cursor = &cursors[index];
        let store = store(context, stores, record.object, writable)?;
        let Some(owner) = OwnedObject::acquire(context, store, record.clone(), cursor)? else {
            return Ok(None);
        };
        owner.verify(context, cursor)?;
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
        ids.len() <= 1024 && ids.iter().copied().collect::<BTreeSet<_>>().len() == ids.len(),
        "reclaim candidate set exceeds capacity or has duplicate objects"
    );
    if let Some(group) = groups.iter().find(|group| group.len() > MAX_GROUP_OBJECTS) {
        return Ok(report(
            None,
            dry_run,
            false,
            false,
            failed(
                group,
                ReclaimStatus::Capacity,
                "whole dependency group exceeds the 16-object protection capacity",
            ),
        ));
    }
    if ids.is_empty() {
        return Ok(report(None, dry_run, false, true, Vec::new()));
    }
    ensure!(
        groups.iter().all(|group| !group.is_empty()),
        "empty reclaim group"
    );
    let mut stores = Stores::new();
    let mut records = Vec::new();
    let mut entries = Vec::new();
    let mut manifest_bytes = 0usize;
    for group in groups {
        let mut group_records = Vec::new();
        for id in group {
            let store = store(context, &mut stores, *id, false)?;
            if store
                .snapshot(
                    pakro_store::control::ControlNamespace::Journals,
                    &claim_name(*id),
                )?
                .is_some()
            {
                return Ok(report(
                    None,
                    dry_run,
                    false,
                    false,
                    failed(
                        group,
                        ReclaimStatus::PendingRecovery,
                        "an original candidate has a pending deletion",
                    ),
                ));
            }
            group_records.push(describe(context, &store, *id)?);
        }
        match verify_group(
            context,
            &group_records,
            &vec![Cursor::default(); group.len()],
            &mut stores,
            false,
        ) {
            Ok(Some(_owners)) => {}
            Ok(None) => {
                return Ok(report(
                    None,
                    dry_run,
                    false,
                    false,
                    failed(
                        group,
                        ReclaimStatus::Busy,
                        "the complete group could not obtain actual EX protection",
                    ),
                ));
            }
            Err(error) => {
                return Ok(report(
                    None,
                    dry_run,
                    false,
                    false,
                    failed(group, failure_status(&error), &format!("{error:#}")),
                ));
            }
        }
        for record in &group_records {
            manifest_bytes = manifest_bytes
                .checked_add(serde_json::to_vec(record)?.len())
                .filter(|n| *n <= 64 * 1024 * 1024)
                .context("reclaim fixed original-manifest capacity exceeded")?;
        }
        entries.extend(group.iter().map(|id| {
            super::entry(
                *id,
                ReclaimStatus::Eligible,
                "complete candidate; execution still rechecks references",
            )
        }));
        records.extend(group_records);
    }
    if let Err(error) = reachability::recheck_group(context, mode, &members(&records), None) {
        return Ok(report(
            None,
            dry_run,
            false,
            false,
            failed(
                &ids,
                reference_status(&error),
                &format!("fresh reclaim references refused: {error:#}"),
            ),
        ));
    }
    if dry_run {
        return Ok(report(None, true, false, true, entries));
    }
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids reclaim writes"
    );
    drop(stores);
    // Cap the complete immutable set before writing any operation metadata.
    let mut total = 0usize;
    for record in &records {
        total = total
            .checked_add(serde_json::to_vec(record)?.len())
            .filter(|n| *n <= 64 * 1024 * 1024)
            .context("reclaim original-manifest set exceeds capacity")?;
    }
    if manifest_bytes
        .checked_add(ids.len() * 1024 + 1024)
        .is_none_or(|bytes| bytes > 64 * 1024 * 1024)
    {
        return Ok(report(
            None,
            false,
            false,
            false,
            failed(
                &ids,
                ReclaimStatus::Capacity,
                "fixed manifests and initial progress exceed operation capacity",
            ),
        ));
    }
    let Some(coordinator) = maintenance(context, true)? else {
        return Ok(report(
            None,
            false,
            false,
            false,
            failed(
                &ids,
                ReclaimStatus::Busy,
                "another reclaim operation is running; no operation record was created",
            ),
        ));
    };
    maintenance_test_gate(context)?;
    let session = match Session::create(context, coordinator.writer.clone(), mode, groups, &records)
    {
        Ok(session) => session,
        Err(error) => {
            if let Some(setup) = error.downcast_ref::<super::journal::CreationInterrupted>() {
                return Ok(report(
                    Some(setup.operation),
                    false,
                    false,
                    false,
                    failed(
                        &ids,
                        ReclaimStatus::PendingRecovery,
                        &format!(
                            "no candidate was deleted; initialization requires the original operation: {error:#}"
                        ),
                    ),
                ));
            }
            return Err(error);
        }
    };
    drop(records);
    run(context, session, false)
}
pub(super) fn resume(
    context: &OperationContext,
    operation: Uuid,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let result = resume_inner(context, operation, dry_run);
    match result {
        Ok(report) => Ok(report),
        Err(error) => Ok(report(
            Some(operation),
            dry_run,
            true,
            false,
            vec![ReclaimEntry {
                object: None,
                label: format!("reclaim-{operation}"),
                status: if error
                    .downcast_ref::<super::journal::OperationBusy>()
                    .is_some()
                {
                    ReclaimStatus::Busy
                } else {
                    ReclaimStatus::Unknown
                },
                reasons: vec![format!(
                    "requested original operation could not be validated: {error:#}"
                )],
            }],
        )),
    }
}
fn resume_inner(
    context: &OperationContext,
    operation: Uuid,
    dry_run: bool,
) -> Result<ReclaimReport> {
    ensure!(!operation.is_nil(), "reclaim operation ID must not be nil");
    let coordinator = if dry_run {
        None
    } else {
        let Some(coordinator) = maintenance(context, false)? else {
            return Ok(report(
                Some(operation),
                false,
                true,
                false,
                vec![ReclaimEntry {
                    object: None,
                    label: format!("reclaim-{operation}"),
                    status: ReclaimStatus::Busy,
                    reasons: vec!["another reclaim operation is running".into()],
                }],
            ));
        };
        Some(coordinator)
    };
    if let Some(report) = super::preparing::abort(context, operation, dry_run)? {
        return Ok(report);
    }
    if dry_run {
        let pending = super::pending(context)?
            .into_iter()
            .find(|p| p.operation_id == operation)
            .context("reclaim operation is unknown")?;
        return Ok(report(
            Some(operation),
            true,
            true,
            true,
            pending
                .objects
                .iter()
                .map(|id| {
                    super::entry(
                        *id,
                        if pending.complete {
                            ReclaimStatus::Removed
                        } else {
                            ReclaimStatus::PendingRecovery
                        },
                        if pending.complete {
                            "operation is already complete"
                        } else {
                            "resume is limited to this original candidate"
                        },
                    )
                })
                .collect(),
        ));
    }
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids reclaim writes"
    );
    let session = Session::open(
        context,
        operation,
        coordinator
            .as_ref()
            .context("reclaim coordination is missing")?
            .writer
            .clone(),
    )?;
    run(context, session, true)
}
fn run(context: &OperationContext, mut session: Session, resumed: bool) -> Result<ReclaimReport> {
    let operation = session.header.operation;
    let ids = session
        .header
        .records
        .iter()
        .map(|r| r.object)
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut stores = Stores::new();
    // Recover already durably removed rows without taking an object lease or
    // inspecting a replacement instance's files.
    for index in 0..ids.len() {
        let progress: Result<bool> = (|| {
            let record = session.record(index)?;
            let (cursor, _) = session.cursor(index)?;
            cursor.validate(record.inventory.entries().len(), record.payload.is_some())?;
            if !cursor.allocation_removed {
                return Ok(false);
            }
            let store = store(context, &mut stores, record.object, false)?;
            ensure!(
                pair(&store.root)? == record.store,
                "reclaim result store changed"
            );
            Ok(
                stat_identity(&store.root, &record.allocation)? != Some(record.allocation_identity)
                    && stat_identity(&store.root, &record.captured)?.is_none(),
            )
        })();
        if let Ok(true) = progress {
            entries.push(super::entry(
                ids[index],
                ReclaimStatus::Removed,
                "original allocation was durably removed",
            ));
        }
    }
    drop(stores);
    match run_inner(context, &mut session, resumed, &mut entries) {
        Ok(report) => Ok(report),
        Err(error) => {
            let removed = entries
                .iter()
                .filter(|row| row.status == ReclaimStatus::Removed)
                .filter_map(|row| row.object)
                .collect::<BTreeSet<_>>();
            entries.retain(|row| row.status == ReclaimStatus::Removed);
            entries.extend(ids.iter().filter(|id| !removed.contains(id)).map(|id| {
                super::entry(
                    *id,
                    ReclaimStatus::PendingRecovery,
                    "retained while original operation metadata requires recovery",
                )
            }));
            entries.push(ReclaimEntry {
                object: None,
                label: format!("reclaim-{operation}"),
                status: ReclaimStatus::PendingRecovery,
                reasons: vec![format!("original operation interrupted: {error:#}")],
            });
            Ok(report(Some(operation), false, resumed, false, entries))
        }
    }
}
fn run_inner(
    context: &OperationContext,
    session: &mut Session,
    resumed: bool,
    entries: &mut Vec<ReclaimEntry>,
) -> Result<ReclaimReport> {
    let operation = session.header.operation;
    let ids = session
        .header
        .records
        .iter()
        .map(|r| r.object)
        .collect::<Vec<_>>();
    let mut stores = Stores::new();
    if session.complete() {
        session.clean_claims(context, &mut stores)?;
        return Ok(report(
            Some(operation),
            false,
            resumed,
            true,
            failed(
                &ids,
                ReclaimStatus::Removed,
                "original deletion already completed; no object was reacquired",
            ),
        ));
    }
    let records = (0..ids.len())
        .map(|i| session.record(i))
        .collect::<Result<Vec<_>>>()?;
    let all_members = members(&records);
    let mut offset = 0;
    for group in session.header.groups.clone() {
        let group_records = &records[offset..offset + group.len()];
        let cursors = (offset..offset + group.len())
            .map(|i| session.cursor(i).map(|(c, _)| c))
            .collect::<Result<Vec<_>>>()?;
        let owners = match verify_group(context, group_records, &cursors, &mut stores, true) {
            Ok(Some(owners)) => owners,
            Ok(None) => {
                return Ok(report(
                    Some(operation),
                    false,
                    resumed,
                    false,
                    with_remaining(
                        entries,
                        &ids,
                        ReclaimStatus::Busy,
                        "a complete original group is busy; later dependencies were retained",
                    ),
                ));
            }
            Err(error) => {
                return Ok(report(
                    Some(operation),
                    false,
                    resumed,
                    false,
                    with_remaining(entries, &ids, failure_status(&error), &format!("{error:#}")),
                ));
            }
        };
        if let Err(error) =
            reachability::recheck_group(context, session.header.mode, &all_members, Some(operation))
        {
            return Ok(report(
                Some(operation),
                false,
                resumed,
                false,
                with_remaining(
                    entries,
                    &ids,
                    reference_status(&error),
                    &format!("original candidate references changed: {error:#}"),
                ),
            ));
        }
        for (index, owner) in owners.iter().enumerate() {
            session.claim(offset + index, owner)?;
        }
        offset += group.len();
    }
    if !session.claims_ready() {
        session.mark_claims_ready()?;
    }
    offset = 0;
    for group in session.header.groups.clone() {
        let group_records = &records[offset..offset + group.len()];
        let cursors = (offset..offset + group.len())
            .map(|i| session.cursor(i).map(|(c, _)| c))
            .collect::<Result<Vec<_>>>()?;
        let owners = match verify_group(context, group_records, &cursors, &mut stores, true) {
            Ok(Some(owners)) => owners,
            Ok(None) => {
                return Ok(report(
                    Some(operation),
                    false,
                    resumed,
                    false,
                    with_remaining(
                        entries,
                        &ids[offset..],
                        ReclaimStatus::Busy,
                        "whole group protection is busy; remaining original candidates retained",
                    ),
                ));
            }
            Err(error) => {
                return Ok(report(
                    Some(operation),
                    false,
                    resumed,
                    false,
                    with_remaining(
                        entries,
                        &ids[offset..],
                        failure_status(&error),
                        &format!("{error:#}"),
                    ),
                ));
            }
        };
        // A fully removed upper group has no live outgoing references. Its
        // original EX, cursor and physical absence were checked above; adding
        // its old edges again would make a later partially removed dependency
        // appear missing. Unfinished groups still use every original member.
        if cursors.iter().all(|cursor| cursor.allocation_removed) {
            ensure!(
                owners
                    .iter()
                    .all(|owner| owner.allocation.is_none() && owner.root.is_none()),
                "completed reclamation group still has original physical content"
            );
            for owner in &owners {
                entries.retain(|row| row.object != Some(owner.record.object));
                entries.push(super::entry(
                    owner.record.object,
                    ReclaimStatus::Removed,
                    "original allocation was durably removed",
                ));
            }
            offset += group.len();
            continue;
        }
        if let Err(error) = reachability::recheck_group(
            context,
            session.header.mode,
            &members(group_records),
            Some(operation),
        ) {
            return Ok(report(
                Some(operation),
                false,
                resumed,
                false,
                with_remaining(
                    entries,
                    &ids[offset..],
                    reference_status(&error),
                    &format!("fresh group references refused: {error:#}"),
                ),
            ));
        }
        for (index, owner) in owners.iter().enumerate() {
            let actual = offset + index;
            let (mut cursor, mut pin) = session.cursor(actual)?;
            if let Err(error) =
                super::tree::remove(&session, actual, owner, context, &mut cursor, &mut pin)
            {
                entries.push(super::entry(
                    owner.record.object,
                    ReclaimStatus::PendingRecovery,
                    format!("controlled deletion interrupted: {error:#}"),
                ));
                entries.extend(failed(
                    &ids[actual + 1..],
                    ReclaimStatus::Retained,
                    "retained after an earlier original deletion stopped",
                ));
                return Ok(report(
                    Some(operation),
                    false,
                    resumed,
                    false,
                    entries.clone(),
                ));
            }
            entries.retain(|row| row.object != Some(owner.record.object));
            entries.push(super::entry(
                owner.record.object,
                ReclaimStatus::Removed,
                "original allocation removed",
            ));
        }
        offset += group.len();
    }
    for index in 0..ids.len() {
        ensure!(
            session.cursor(index)?.0.allocation_removed,
            "reclaim physical deletion is incomplete"
        );
    }
    reachability::cleanup_retired(context, &ids, Some(operation))?;
    session.mark_retired_cleaned()?;
    session.mark_complete()?;
    session.clean_claims(context, &mut stores)?;
    Ok(report(
        Some(operation),
        false,
        resumed,
        true,
        entries.clone(),
    ))
}
