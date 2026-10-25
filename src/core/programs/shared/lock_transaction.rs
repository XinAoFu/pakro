//! P2's recoverable lock/catalog commit. The catalog transaction is the
//! logical commit point; an unfinished document write is never a read view.

use crate::libraries::project::{
    identity::ProjectIdentity,
    repository::{ProjectRepository, repository_uid},
    snapshot::ObservedProjectInputs,
};
use crate::lockfile::transaction::{LockImage, ProjectLockSession};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::{
    ControlLimits, ControlNamespace, ControlSnapshot, ReadStore, RootLimits, StoreWriter,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const PREFIX: &str = "p2-lock-";
const MAX_LOCK: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    #[serde(with = "canonical_uuid")]
    project: Uuid,
    #[serde(with = "canonical_uuid")]
    transaction: Uuid,
    expected_revision: u64,
    expected_project: [u8; 32],
    path: PathBuf,
    parent: (u64, u64),
    old_bytes: Option<Vec<u8>>,
    old_identity: Option<(u64, u64)>,
    new_bytes: Vec<u8>,
    staged: Option<(PathBuf, (u64, u64))>,
    rollback_staged: Option<(PathBuf, (u64, u64))>,
    references: Vec<String>,
}

// Journal UUIDs have one fixed wire spelling without enabling additional
// features on the repository-wide UUID dependency. The visitor borrows the
// JSON string and rejects any length other than the canonical 36 bytes.
mod canonical_uuid {
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    use uuid::Uuid;

    pub(super) fn serialize<S: Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        let mut bytes = [0; 36];
        serializer.serialize_str(value.hyphenated().encode_lower(&mut bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Uuid, D::Error> {
        struct CanonicalUuid;
        impl Visitor<'_> for CanonicalUuid {
            type Value = Uuid;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a canonical lowercase hyphenated UUID")
            }
            fn visit_str<E: Error>(self, value: &str) -> Result<Uuid, E> {
                if value.len() != 36 {
                    return Err(E::invalid_length(value.len(), &self));
                }
                let parsed =
                    Uuid::parse_str(value).map_err(|_| E::custom("invalid journal UUID"))?;
                let mut bytes = [0; 36];
                if parsed.hyphenated().encode_lower(&mut bytes) != value {
                    return Err(E::custom("journal UUID is not canonical"));
                }
                Ok(parsed)
            }
        }
        deserializer.deserialize_str(CanonicalUuid)
    }
}

fn name(project: Uuid) -> String {
    format!("{PREFIX}{}", project.simple())
}
fn parent_identity(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path.parent().context("lock parent is missing")?)?;
    ensure!(metadata.is_dir(), "lock parent is not a directory");
    Ok((metadata.dev(), metadata.ino()))
}
fn decode(snapshot: &ControlSnapshot, expected: Option<Uuid>) -> Result<Journal> {
    let journal: Journal = serde_json::from_slice(snapshot.bytes())
        .map_err(|_| eyre::eyre!("PendingRecovery: project lock journal is damaged"))?;
    ensure!(
        journal.schema == 1
            && !journal.project.is_nil()
            && !journal.transaction.is_nil()
            && expected.is_none_or(|project| project == journal.project)
            && journal.path.is_absolute()
            && journal.path.extension().is_some_and(|name| name == "lock")
            && !journal
                .path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            && journal.old_bytes.is_some() == journal.old_identity.is_some()
            && journal
                .old_bytes
                .as_ref()
                .is_none_or(|bytes| bytes.len() <= MAX_LOCK)
            && journal.new_bytes.len() <= MAX_LOCK
            && journal.references.len() <= 65536,
        "PendingRecovery: project lock journal has unsupported facts"
    );
    for reference in &journal.references {
        let digest: pakro_store::Digest = reference
            .parse()
            .map_err(|_| eyre::eyre!("PendingRecovery: journal input reference is invalid"))?;
        ensure!(
            digest.to_string() == *reference,
            "PendingRecovery: journal input reference is not canonical"
        );
    }
    Ok(journal)
}
fn read_store(context: &OperationContext) -> Result<Option<ReadStore>> {
    ReadStore::open_readonly(
        &context.requested_data_root().join("projects"),
        repository_uid(),
        RootLimits::default(),
        context.policy().deadline,
    )
}

/// Scoped query admission. It never creates a guard, repairs, or changes a
/// journal. Other projects can continue reading their complete snapshots.
pub(crate) fn ensure_project_consistent(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<()> {
    observed.recheck()?;
    let project = ProjectIdentity::from_observation(observed)?.as_uuid();
    ensure_identity_consistent(context, project)?;
    let mut lock_targets = std::collections::BTreeSet::new();
    for path in observed.paths().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "lock")
    }) {
        context.checkpoint()?;
        lock_targets.insert(std::fs::canonicalize(path).map_err(|_| {
            eyre::eyre!("observed lock target cannot be verified for pending recovery")
        })?);
    }
    observed.recheck()?;
    if let Some(read) = read_store(context)? {
        let control = read.control(ControlLimits::default())?;
        for entry in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
            if !entry.starts_with(PREFIX) {
                continue;
            }
            let snapshot = control
                .read(ControlNamespace::Journals, &entry)?
                .context("PendingRecovery: lock journal disappeared during query")?;
            let journal = decode(&snapshot, None)?;
            ensure!(
                entry == name(journal.project),
                "PendingRecovery: lock journal project name changed"
            );
            ensure!(
                !observed.observed_auxiliary(&journal.path)
                    && !lock_targets.contains(&journal.path),
                "PendingRecovery: an observed lock document belongs to an unfinished project transaction"
            );
        }
    }
    observed.recheck()?;
    Ok(())
}
pub(super) fn ensure_identity_consistent(context: &OperationContext, project: Uuid) -> Result<()> {
    if let Some(read) = read_store(context)? {
        let control = read.control(ControlLimits::default())?;
        if let Some(snapshot) = control.read(ControlNamespace::Journals, &name(project))? {
            let _ = decode(&snapshot, Some(project))?;
            eyre::bail!(
                "PendingRecovery: this project's lock/catalog transaction needs an explicit installation to recover"
            );
        }
    }
    Ok(())
}
pub(super) fn project_state(
    model: &crate::packages::python_environment::model::CatalogModel,
    project: Uuid,
) -> [u8; 32] {
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new_with_prefix(b"pakro.project-selection-state.v1");
    for (key, stamp) in model
        .selection_slots()
        .filter(|(key, _)| key.project() == project)
    {
        hash.update(key.group().as_bytes());
        hash.update([0]);
        hash.update(key.target());
        hash.update(stamp.revision().to_le_bytes());
        hash.update(stamp.transaction().as_bytes());
        hash.update(stamp.generation().unwrap_or("").as_bytes());
        hash.update([0]);
        hash.update(stamp.language().unwrap_or("").as_bytes());
        hash.update([0]);
    }
    hash.finalize().into()
}

/// Unfinished preparation protects its exact new objects as well as the old
/// selection retained by the catalog. Malformed journals are unknown, not zero.
pub(crate) fn pending_reference_chains(
    context: &OperationContext,
    target: [u8; 32],
) -> Result<Vec<String>> {
    let Some(read) = read_store(context)? else {
        return Ok(Vec::new());
    };
    let control = read.control(ControlLimits::default())?;
    let mut users = Vec::new();
    for entry in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
        if !entry.starts_with(PREFIX) {
            continue;
        }
        let snapshot = control
            .read(ControlNamespace::Journals, &entry)?
            .context("pending lock journal disappeared")?;
        let journal = decode(&snapshot, None)?;
        ensure!(
            entry == name(journal.project),
            "PendingRecovery: lock journal name differs from its project"
        );
        let target = pakro_store::Digest::Sha256(target).to_string();
        if journal.references.contains(&target) {
            users.push(format!(
                "pending project {} transaction {}",
                journal.project, journal.transaction
            ));
        }
    }
    Ok(users)
}

pub(super) struct PendingLockCommit {
    writer: std::sync::Arc<StoreWriter>,
    session: ProjectLockSession,
    installed: LockImage,
    path: PathBuf,
    snapshot: ControlSnapshot,
}
impl PendingLockCommit {
    pub(super) fn apply(
        context: &OperationContext,
        observed: &ObservedProjectInputs,
        writer: std::sync::Arc<StoreWriter>,
        project: Uuid,
        transaction: Uuid,
        expected_revision: u64,
        expected_project: [u8; 32],
        references: Vec<String>,
        path: &Path,
        bytes: &[u8],
    ) -> Result<Self> {
        ensure!(
            context.policy().writes.project_selection
                && context.policy().writes.lockfile
                && !context.policy().locked,
            "operation policy forbids lock publication"
        );
        ensure!(
            bytes.len() <= MAX_LOCK && references.len() <= 65536,
            "project lock transaction exceeds its document or input budget"
        );
        for reference in &references {
            let digest: pakro_store::Digest = reference.parse()?;
            ensure!(
                digest.to_string() == *reference,
                "project lock transaction input is not canonical"
            );
        }
        ensure!(
            observed.observed_auxiliary(path)
                && path.extension().is_some_and(|name| name == "lock"),
            "lock publication target was not observed"
        );
        ensure_project_consistent(context, observed)?;
        observed.recheck()?;
        let mut session = ProjectLockSession::acquire(path, MAX_LOCK, context.policy().deadline)?;
        ensure!(
            session.canonical_target() == path,
            "P2 lock publication does not support a symlink lock target"
        );
        let original = session.snapshot()?;
        ensure!(
            original.bytes() == observed.document(path),
            "project lock differs from the operation observation"
        );
        observed.recheck()?;
        let mut journal = Journal {
            schema: 1,
            project,
            transaction,
            expected_revision,
            expected_project,
            path: path.to_owned(),
            parent: parent_identity(path)?,
            old_bytes: original.bytes().map(<[u8]>::to_vec),
            old_identity: original.file_identity(),
            new_bytes: bytes.to_owned(),
            staged: None,
            rollback_staged: None,
            references,
        };
        let key = name(project);
        let mut snapshot = writer.control(ControlLimits::default())?.replace(
            ControlNamespace::Journals,
            &key,
            None,
            &serde_json::to_vec(&journal)?,
        )?;
        let installed =
            session.compare_and_replace_journaled(&original, bytes, |temporary, identity| {
                journal.staged = Some((temporary.to_owned(), identity));
                snapshot = writer.control(ControlLimits::default())?.replace(
                    ControlNamespace::Journals,
                    &key,
                    Some(&snapshot),
                    &serde_json::to_vec(&journal)?,
                )?;
                commit_cutpoint("documents-staged");
                Ok(())
            })?;
        ensure!(
            journal.staged.as_ref().map(|(_, identity)| *identity) == installed.file_identity(),
            "published lock identity differs from journal"
        );
        commit_cutpoint("documents-applied");
        Ok(Self {
            writer,
            session,
            installed,
            path: path.to_owned(),
            snapshot,
        })
    }
    pub(super) fn recheck_inputs(&self, observed: &ObservedProjectInputs) -> Result<()> {
        self.session.recheck(&self.installed)?;
        observed.recheck_except_owned_auxiliary_lock(&self.path)?;
        self.session.recheck(&self.installed)
    }
    pub(super) fn finalize(self) -> Result<()> {
        let mut control = self.writer.control(ControlLimits::default())?;
        self.session.recheck(&self.installed)?;
        control.remove(&self.snapshot)
    }
}

#[cfg(test)]
thread_local! {
    static COMMIT_TEST_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&str)>>> = const { std::cell::RefCell::new(None) };
}

/// Tests inject edits or abrupt process exit at actual durable boundaries.
/// No environment-triggered cutpoint is compiled into production.
pub(super) fn commit_cutpoint(stage: &str) {
    #[cfg(test)]
    {
        COMMIT_TEST_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(stage);
            }
        });
        if std::env::var("PAKRO_TEST_P2_LOCK_CHILD").as_deref() == Ok("commit")
            && std::env::var("PAKRO_TEST_P2_LOCK_CUTPOINT").as_deref() == Ok(stage)
        {
            println!("PAKRO_P2_DURABLE_CUTPOINT={stage}");
            std::process::exit(86);
        }
    }
    #[cfg(not(test))]
    let _ = stage;
}

#[cfg(test)]
pub(super) fn with_commit_test_hook<T>(
    hook: impl FnMut(&str) + 'static,
    body: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            COMMIT_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
        }
    }
    COMMIT_TEST_HOOK.with(|slot| {
        assert!(slot.borrow().is_none(), "commit test hook already active");
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _reset = Reset;
    body()
}

/// Only an explicit mutation calls recovery. A successful rollback changes
/// the lock inode, so callers must observe the project again when true.
pub(crate) fn recover_project_lock(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<bool> {
    Ok(recover_project_lock_guarded(context, observed)?.is_some())
}

/// Production handoff keeps both the project protection and the actual
/// recovered document image through observation. External edits cannot be
/// accepted as though they were part of this operation's original inputs.
pub(crate) fn recover_project_lock_and_observe(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<Option<ObservedProjectInputs>> {
    let Some(recovered) = recover_project_lock_guarded(context, observed)? else {
        return Ok(None);
    };
    observed.recheck_except_owned_auxiliary_lock(&recovered.path)?;
    recovered.check(context)?;
    commit_cutpoint("recovery-before-observe");
    let fresh = observed.reobserve()?;
    recovered.check(context)?;
    observed.recheck_except_owned_auxiliary_lock(&recovered.path)?;
    fresh.recheck()?;
    ensure!(
        ProjectIdentity::from_observation(&fresh)?.as_uuid() == recovered.project
            && fresh.document(&recovered.path) == recovered.image.bytes(),
        "recovered project lock observation does not match the owned recovery image"
    );
    recovered.check(context)?;
    Ok(Some(fresh))
}

struct RecoveredProjectLock {
    writer: std::sync::Arc<StoreWriter>,
    lock: pakro_store::control::lease::StableLock,
    guard: pakro_store::control::lease::ObjectLease,
    session: ProjectLockSession,
    image: LockImage,
    path: PathBuf,
    project: Uuid,
}
impl RecoveredProjectLock {
    fn check(&self, context: &OperationContext) -> Result<()> {
        context.checkpoint()?;
        self.writer.check_binding()?;
        self.guard.check_binding()?;
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("project-{}.lock", self.project.hyphenated()),
        )?;
        self.session.recheck(&self.image)
    }
}

fn recover_project_lock_guarded(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<Option<RecoveredProjectLock>> {
    ensure!(
        context.policy().writes.project_selection,
        "operation policy forbids project recovery"
    );
    let project = ProjectIdentity::from_observation(observed)?;
    let Some(read) = read_store(context)? else {
        return Ok(None);
    };
    if read
        .control(ControlLimits::default())?
        .read(ControlNamespace::Journals, &name(project.as_uuid()))?
        .is_none()
    {
        return Ok(None);
    }
    drop(read);
    let mut repository = ProjectRepository::open_writer(context)?;
    let writer = repository.writer()?;
    let lock = super::commit::open_project_lock(&writer, &project)?;
    let guard = writer
        .control(ControlLimits::default())?
        .acquire_project(&lock)?;
    let Some(mut snapshot) = writer
        .control(ControlLimits::default())?
        .read(ControlNamespace::Journals, &name(project.as_uuid()))?
    else {
        return Ok(None);
    };
    let mut journal = decode(&snapshot, Some(project.as_uuid()))?;
    ensure!(
        observed.observed_auxiliary(&journal.path),
        "PendingRecovery: journal lock target is outside observed inputs"
    );
    let mut session =
        ProjectLockSession::acquire(&journal.path, MAX_LOCK, context.policy().deadline)?;
    ensure!(
        session.canonical_target() == journal.path
            && parent_identity(&journal.path)? == journal.parent,
        "PendingRecovery: journal lock namespace changed"
    );
    repository.reload()?;
    let current = session.snapshot()?;
    let installed = journal.staged.as_ref().map(|(_, identity)| *identity)
        == current.file_identity()
        && current.bytes() == Some(journal.new_bytes.as_slice());
    let original = current.file_identity() == journal.old_identity
        && current.bytes() == journal.old_bytes.as_deref();
    let rollback = journal
        .rollback_staged
        .as_ref()
        .map(|(_, identity)| *identity)
        == current.file_identity()
        && current.bytes() == journal.old_bytes.as_deref();
    let committed = if let Some((key, expected, _)) =
        repository.model().applied_transaction(journal.transaction)
    {
        ensure!(
            key.project() == journal.project && expected.revision() == journal.expected_revision,
            "PendingRecovery: catalog transaction does not match its lock journal"
        );
        true
    } else {
        false
    };
    // This receipt is either the already validated original journal state or
    // the exact inode/bytes returned by our own write. Never replace it with
    // a fresh observation after a side effect.
    let mut image = current;
    if committed {
        // Never rewind catalog or a later project selection. The document
        // must still be the actual committed inode before finalizing.
        ensure!(
            installed,
            "PendingRecovery: committed lock was externally changed"
        );
    } else {
        ensure!(
            project_state(repository.model(), journal.project) == journal.expected_project,
            "PendingRecovery: a later project selection prevents rollback"
        );
        ensure!(
            original || installed || rollback,
            "PendingRecovery: lock was externally changed; recovery evidence retained"
        );
        if installed {
            ensure!(
                context.policy().writes.lockfile && !context.policy().locked,
                "PendingRecovery: recovering this transaction would modify the lock; run installation without --locked first"
            );
            if let Some(bytes) = journal.old_bytes.clone() {
                image = session.compare_and_replace_journaled(
                    &image,
                    &bytes,
                    |temporary, identity| {
                        journal.rollback_staged = Some((temporary.to_owned(), identity));
                        snapshot = writer.control(ControlLimits::default())?.replace(
                            ControlNamespace::Journals,
                            &name(journal.project),
                            Some(&snapshot),
                            &serde_json::to_vec(&journal)?,
                        )?;
                        Ok(())
                    },
                )?;
                ensure!(
                    image.file_identity()
                        == journal
                            .rollback_staged
                            .as_ref()
                            .map(|(_, identity)| *identity)
                        && image.bytes() == journal.old_bytes.as_deref(),
                    "PendingRecovery: restored lock differs from the journaled owned inode or bytes"
                );
            } else {
                image = session.compare_and_remove(&image)?;
                ensure!(
                    !image.exists() && image.file_identity().is_none(),
                    "PendingRecovery: owned lock removal did not return expected absence"
                );
            }
            commit_cutpoint("recovery-written");
        }
    }
    session.recheck(&image)?;
    if let Some((path, identity)) = &journal.staged {
        session.cleanup_staged(path, *identity, &journal.new_bytes)?;
        session.recheck(&image)?;
    }
    if let Some((path, identity)) = &journal.rollback_staged {
        session.cleanup_staged(
            path,
            *identity,
            journal
                .old_bytes
                .as_deref()
                .context("rollback has no original document")?,
        )?;
        session.recheck(&image)?;
    }
    {
        let mut control = writer.control(ControlLimits::default())?;
        session.recheck(&image)?;
        control.remove(&snapshot)?;
    }
    Ok(Some(RecoveredProjectLock {
        writer,
        lock,
        guard,
        session,
        image,
        path: journal.path,
        project: project.as_uuid(),
    }))
}

#[cfg(test)]
#[path = "lock_transaction/preflight_stage.rs"]
mod preflight_stage;
#[cfg(test)]
mod tests;
