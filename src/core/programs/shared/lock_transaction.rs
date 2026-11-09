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

mod package_recovery;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparing_packages: Option<PreparingPackages>,
}

/// Original prepared allocations. These facts describe a pending operation;
/// recovery must reopen the original locks before they authorize publication.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparingPackages {
    declaration_digest: String,
    store: [u64; 2],
    leases: [u64; 2],
    build: [u64; 2],
    members: Vec<crate::libraries::package::manifest::PackageManifest>,
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
        matches!(journal.schema, 1 | 2)
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
    match (&journal.preparing_packages, journal.schema) {
        (None, 1) => {}
        (Some(preparing), 2) => {
            ensure!(
                preparing.declaration_digest.len() == 64
                    && preparing
                        .declaration_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    && journal.new_bytes.is_empty()
                    && journal.staged.is_none()
                    && journal.rollback_staged.is_none()
                    && !preparing.members.is_empty()
                    && preparing.members.len() <= 65536,
                "PendingRecovery: invalid package preparation stage"
            );
            let mut keys = std::collections::BTreeSet::new();
            let mut allocations = std::collections::BTreeSet::new();
            for member in &preparing.members {
                member.validate_metadata()?;
                ensure!(
                    member.conda.is_some()
                        && keys.insert(member.key())
                        && allocations.insert(&member.storage.allocation),
                    "PendingRecovery: duplicate or unsupported prepared package"
                );
            }
        }
        _ => eyre::bail!("PendingRecovery: unsupported project journal stage"),
    }
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

/// Describe a validated unfinished commit owned by this exact project/lock
/// observation. This is execution admission, not recovery permission: the
/// mutation path must still reacquire protection and check original evidence.
/// Reading this result creates no writer, project guard, or document session.
pub(crate) fn own_pending_lock_commit(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<bool> {
    context.checkpoint()?;
    let Some(read) = read_store(context)? else {
        // No journal store: there is nothing to recover. An early strict
        // recheck here would refuse an installation whose lock file changed
        // after its own observation; the caller's input checks remain.
        context.checkpoint()?;
        return Ok(false);
    };
    observed.recheck()?;
    let project = ProjectIdentity::from_observation(observed)?.as_uuid();
    let mut lock_targets = std::collections::BTreeSet::new();
    for path in observed.paths().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "lock")
    }) {
        context.checkpoint()?;
        // The fixed installation lock keeps the real target its initial
        // observation resolved; re-resolving the current entry would make a
        // deleted or replaced lock refuse an install that already owns its
        // input, or move the target to a later re-pointed link.
        let target = if observed.fixed_lock_path() == Some(path) {
            observed.fixed_lock_target().unwrap_or(path).to_path_buf()
        } else {
            std::fs::canonicalize(path).map_err(|_| {
                eyre::eyre!("observed lock target cannot be verified for pending recovery")
            })?
        };
        lock_targets.insert(target);
    }
    observed.recheck()?;
    let control = read.control(ControlLimits::default())?;
    let mut pending = false;
    for entry in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
        if !entry.starts_with(PREFIX) {
            continue;
        }
        context.checkpoint()?;
        let snapshot = control
            .read(ControlNamespace::Journals, &entry)?
            .context("PendingRecovery: lock journal disappeared during admission")?;
        let journal = decode(&snapshot, None)?;
        ensure!(
            entry == name(journal.project),
            "PendingRecovery: lock journal project name changed"
        );
        if journal.project != project {
            ensure!(
                !observed.observed_auxiliary(&journal.path)
                    && !lock_targets.contains(&journal.path),
                "PendingRecovery: an observed lock document belongs to another unfinished project transaction"
            );
            continue;
        }
        ensure!(
            observed.observed_auxiliary(&journal.path),
            "PendingRecovery: this project's journal lock target is outside observed inputs"
        );
        let parent = journal
            .path
            .parent()
            .context("PendingRecovery: journal lock parent is missing")?;
        ensure!(
            std::fs::canonicalize(parent)? == parent
                && parent_identity(&journal.path)? == journal.parent,
            "PendingRecovery: journal lock namespace changed"
        );
        if observed.document(&journal.path).is_some() {
            ensure!(
                std::fs::canonicalize(&journal.path)? == journal.path,
                "PendingRecovery: journal lock target was replaced by an alias"
            );
        }
        ensure!(
            journal.rollback_staged.is_none() || journal.old_bytes.is_some(),
            "PendingRecovery: rollback journal has no original lock document"
        );
        for (staged, _) in journal.staged.iter().chain(journal.rollback_staged.iter()) {
            ensure!(
                staged.parent() == Some(parent)
                    && staged
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(".pakro.lock.txn-")),
                "PendingRecovery: staged lock namespace differs from its journal target"
            );
        }
        pending = true;
    }
    read.check_binding()?;
    observed.recheck()?;
    context.checkpoint()?;
    Ok(pending)
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
        // As above: the fixed installation lock keeps the real target its
        // initial observation resolved, not a later re-pointed entry.
        let target = if observed.fixed_lock_path() == Some(path) {
            observed.fixed_lock_target().unwrap_or(path).to_path_buf()
        } else {
            std::fs::canonicalize(path).map_err(|_| {
                eyre::eyre!("observed lock target cannot be verified for pending recovery")
            })?
        };
        lock_targets.insert(target);
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
pub(crate) fn ensure_identity_consistent(context: &OperationContext, project: Uuid) -> Result<()> {
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

/// Record prepared Conda package owners in the project's existing P2 journal.
/// The caller must already hold the project guard and retain all package owners.
/// This prepares recovery only; it does not publish a package or change the lock.
#[allow(dead_code)]
pub(crate) fn prepare_package_publication(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    expected: &crate::libraries::project::state::ExpectedProjectState,
    writer: &StoreWriter,
    project_lock: &pakro_store::control::lease::StableLock,
    project_guard: &pakro_store::control::lease::ObjectLease,
    transaction: Uuid,
    lock_path: &Path,
    members: &mut [(
        &mut crate::packages::python_environment::prefix::UnpublishedPrefix,
        &crate::libraries::package::manifest::PackageManifest,
    )],
    dependencies: &[crate::libraries::package::lease::PackageLease],
    language: Option<&crate::libraries::language::lease::LanguageLease>,
) -> Result<()> {
    use crate::packages::python_environment::prefix::verify::{
        InventoryRoot, PrefixInventoryLimits, observe_tree_inventory,
    };
    use pakro_store::control::lease::LeaseMode;
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries && context.policy().writes.project_selection,
        "operation policy forbids package publication"
    );
    ensure!(
        !transaction.is_nil() && !members.is_empty() && members.len() <= 65536,
        "invalid package publication group"
    );
    let identity = ProjectIdentity::from_observation(observed)?;
    expected.ensure_identity(&identity)?;
    project_lock.check_location(
        writer.build_handle(),
        &format!("project-{}.lock", identity.as_uuid().hyphenated()),
    )?;
    project_guard.check_binding()?;
    ensure!(
        project_guard.mode() == LeaseMode::Exclusive
            && project_guard.identity() == project_lock.identity(),
        "package preparation requires the original project guard"
    );
    ensure_project_consistent(context, observed)?;
    observed.recheck()?;
    let repository = ProjectRepository::open_readonly(context)?;
    expected.ensure_current(repository.model())?;
    let actual_store = pakro_store::control::open_trusted_store_with_deadline(
        &context.requested_data_root().join("projects"),
        repository_uid(),
        false,
        context.policy().deadline,
    )?;
    ensure!(
        pakro_store::control::FileIdentity::of(&actual_store)?
            == pakro_store::control::FileIdentity::of(writer.root_handle())?,
        "package preparation project store differs from the operation"
    );
    let expected_project = project_state(repository.model(), identity.as_uuid());
    ensure!(
        observed.observed_auxiliary(lock_path)
            && lock_path.extension().is_some_and(|name| name == "lock"),
        "package preparation lock target was not observed"
    );
    let session = ProjectLockSession::acquire(lock_path, MAX_LOCK, context.policy().deadline)?;
    ensure!(
        session.canonical_target() == lock_path,
        "package preparation lock target is an alias"
    );
    let old = session.snapshot()?;
    ensure!(
        old.bytes() == observed.document(lock_path),
        "package preparation lock changed"
    );
    let mut keys = std::collections::BTreeSet::new();
    let mut references = std::collections::BTreeSet::new();
    for dependency in dependencies {
        dependency.ensure_library_context(context)?;
        dependency.check_binding()?;
        ensure!(
            keys.insert(dependency.manifest().key()),
            "duplicate held package"
        );
        references.insert(pakro_store::Digest::Sha256(dependency.manifest().key()).to_string());
    }
    if let Some(language) = language {
        language.ensure_library_context(context)?;
        language.check_binding()?;
        references.insert(pakro_store::Digest::Sha256(language.key().fingerprint()).to_string());
    }
    let mut manifests = Vec::with_capacity(members.len());
    let mut namespace = None;
    for (prefix, manifest) in members.iter_mut() {
        manifest.validate_metadata()?;
        ensure!(
            manifest.conda.is_some() && keys.insert(manifest.key()),
            "invalid or duplicate prepared Conda package"
        );
        ensure!(
            manifest
                .language
                .is_none_or(|key| language.is_some_and(|held| held.key().fingerprint() == key)),
            "prepared package's Python is not held"
        );
        let facts = prefix.generation_identity()?.facts()?;
        ensure!(
            prefix.final_path().parent()
                == Some(crate::libraries::package::manifest::store_path(context).as_path())
                && facts.allocation == manifest.storage.allocation
                && facts.generation == manifest.storage.root_identity
                && facts.lock == manifest.storage.lock,
            "prepared package differs from its actual owner"
        );
        let directories = (facts.store, facts.leases, facts.build);
        ensure!(
            namespace.is_none_or(|expected| expected == directories),
            "prepared packages belong to different stores"
        );
        namespace = Some(directories);
        ensure!(
            observe_tree_inventory(&**prefix, PrefixInventoryLimits::default())?
                == manifest.storage.inventory,
            "prepared package inventory changed"
        );
        crate::libraries::package::lease::validate_package_facts(prefix.root_file(), manifest)?;
        references.insert(pakro_store::Digest::Sha256(manifest.key()).to_string());
        manifests.push((**manifest).clone());
    }
    for manifest in &manifests {
        ensure!(
            manifest
                .facts
                .dependencies
                .iter()
                .all(|key| keys.contains(key)),
            "prepared package dependency is not held by this group"
        );
    }
    let (store, leases, build) = namespace.context("package preparation has no store")?;
    let journal = Journal {
        schema: 2,
        project: identity.as_uuid(),
        transaction,
        expected_revision: 0,
        expected_project,
        path: lock_path.to_owned(),
        parent: parent_identity(lock_path)?,
        old_bytes: old.bytes().map(<[u8]>::to_vec),
        old_identity: old.file_identity(),
        new_bytes: Vec::new(),
        staged: None,
        rollback_staged: None,
        references: references.into_iter().collect(),
        preparing_packages: Some(PreparingPackages {
            declaration_digest: super::commit::declaration_digest(observed),
            store,
            leases,
            build,
            members: manifests,
        }),
    };
    let bytes = serde_json::to_vec(&journal)?;
    ensure!(
        bytes.len() as u64 <= ControlLimits::default().max_file_bytes,
        "package preparation journal exceeds its budget"
    );
    observed.recheck()?;
    session.recheck(&old)?;
    project_guard.check_binding()?;
    // On an uncertain CAS outcome every original allocation stays available to
    // recovery. If absence is confirmed, cleanup can be re-enabled.
    for index in 0..members.len() {
        if let Err(error) = members[index].0.retain_for_publication_journal() {
            for (prefix, _) in &mut members[..index] {
                prefix.release_publication_journal_retention();
            }
            return Err(error);
        }
    }
    let result = writer.control(ControlLimits::default())?.replace(
        ControlNamespace::Journals,
        &name(identity.as_uuid()),
        None,
        &bytes,
    );
    if let Err(error) = result {
        if writer
            .control(ControlLimits::default())?
            .read(ControlNamespace::Journals, &name(identity.as_uuid()))?
            .is_none()
        {
            for (prefix, _) in members.iter_mut() {
                prefix.release_publication_journal_retention();
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Unfinished preparation protects its exact new objects as well as the old
/// selection retained by the catalog. Malformed journals are unknown, not zero.
pub(crate) struct PendingProjectProtection {
    pub journal: String,
    pub project: Uuid,
    pub transaction: Uuid,
    pub references: Vec<[u8; 32]>,
}

/// One bounded scan of the original P2 journals. References remain projections
/// of those journals; no additional persistent reference authority is created.
pub(crate) fn pending_protections(
    context: &OperationContext,
) -> Result<Vec<PendingProjectProtection>> {
    let Some(read) = read_store(context)? else {
        return Ok(Vec::new());
    };
    let control = read.control(ControlLimits::default())?;
    let mut result = Vec::new();
    let mut total = 0usize;
    for entry in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
        if !entry.starts_with(PREFIX) {
            continue;
        }
        context.checkpoint()?;
        let snapshot = control
            .read(ControlNamespace::Journals, &entry)?
            .context("PendingRecovery: lock journal disappeared")?;
        total = total
            .checked_add(snapshot.bytes().len())
            .filter(|value| *value <= 64 * 1024 * 1024)
            .context("pending project journal byte budget exceeded")?;
        let journal = decode(&snapshot, None)?;
        ensure!(
            entry == name(journal.project),
            "PendingRecovery: lock journal name differs from its project"
        );
        let mut references = Vec::new();
        for reference in journal.references {
            if let pakro_store::Digest::Sha256(key) = reference.parse()? {
                references.push(key);
            }
        }
        if let Some(preparing) = journal.preparing_packages {
            references.extend(preparing.members.iter().map(|member| member.key()));
        }
        references.sort_unstable();
        references.dedup();
        result.push(PendingProjectProtection {
            journal: entry,
            project: journal.project,
            transaction: journal.transaction,
            references,
        });
    }
    Ok(result)
}

pub(crate) fn pending_reference_chains(
    context: &OperationContext,
    target: [u8; 32],
) -> Result<Vec<String>> {
    let mut users = Vec::new();
    for journal in pending_protections(context)? {
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
        // One write-time backup of the current entry: bytes, identity and
        // mode from this single observation. It is never compared against the
        // installation's initial input and never parsed as dependency data.
        let original = session.snapshot()?;
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
            preparing_packages: None,
        };
        let key = name(project);
        let mut snapshot = writer.control(ControlLimits::default())?.replace(
            ControlNamespace::Journals,
            &key,
            None,
            &serde_json::to_vec(&journal)?,
        )?;
        let installed =
            session.replace_journaled_install(&original, bytes, |temporary, identity| {
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
            path: path.to_owned(),
            snapshot,
        })
    }
    pub(super) fn recheck_inputs(&self, observed: &ObservedProjectInputs) -> Result<()> {
        self.session.check_binding()?;
        observed.recheck_except_owned_auxiliary_lock(&self.path)?;
        self.session.check_binding()
    }
    pub(super) fn finalize(self) -> Result<()> {
        let mut control = self.writer.control(ControlLimits::default())?;
        self.session.check_binding()?;
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
    let lock = super::commit::open_existing_project_lock(&writer, &project)?;
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
    if let Some(preparing) = journal.preparing_packages.as_ref() {
        ensure!(
            project_state(repository.model(), journal.project) == journal.expected_project
                && super::commit::declaration_digest(observed) == preparing.declaration_digest
                && current.file_identity() == journal.old_identity
                && current.bytes() == journal.old_bytes.as_deref(),
            "PendingRecovery: package preparation project inputs changed"
        );
        observed.recheck()?;
        session.recheck(&current)?;
        package_recovery::resume(context, &journal)?;
        observed.recheck()?;
        session.recheck(&current)?;
        writer
            .control(ControlLimits::default())?
            .remove(&snapshot)?;
        return Ok(Some(RecoveredProjectLock {
            writer,
            lock,
            guard,
            session,
            image: current,
            path: journal.path,
            project: project.as_uuid(),
        }));
    }
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
