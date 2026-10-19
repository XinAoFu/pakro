//! Single-catalog commit of one project registration and its selection.
//!
//! Reuses the R05 lock and CAS core over one internal catalog. F1 deliberately
//! creates no general multi-file WAL subsystem: a registration and selection
//! become durable in one CAS step, and the previous selection stays intact
//! until that step succeeds. Protections are taken in project → object SH →
//! catalog order and rechecked after waiting.

use eyre::{ContextCompat, Result, ensure};
use pakro_store::control::lease::ObjectLease;
use pakro_store::control::lease::StableLock;
use pakro_store::control::{ControlLimits, FileIdentity};
use uuid::Uuid;

use crate::libraries::language::identity::LanguageKey;
use crate::libraries::language::repository::LanguageRepository;
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::repository::language_digest;
use crate::libraries::project::repository::repository_uid;
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::packages::python_environment::model::SelectionKey;
use crate::programs::query::snapshot::LanguageCandidate;
use crate::programs::shared::context::OperationContext;

/// The selection group of a plain directory selection.
pub(crate) const DEFAULT_GROUP: &str = "default";

pub(crate) struct PreparedSelectionCommit {
    context: OperationContext,
    repository: ProjectRepository,
    identity: ProjectIdentity,
    root: String,
    declaration: String,
    key: SelectionKey,
    expected: crate::packages::python_environment::model::SelectionStamp,
    language: String,
    language_key: LanguageKey,
}

pub(crate) struct SelectionCommitReceipt {
    revision: u64,
}

impl SelectionCommitReceipt {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// Rechecks the same observation and prepares the registration and
/// language-only selection for the single catalog. Protections are taken by
/// the commit in project → object SH → catalog order; this stage only
/// validates inputs and expected facts.
pub(crate) fn prepare_selection_commit(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    candidate: &LanguageCandidate,
) -> Result<PreparedSelectionCommit> {
    context.checkpoint()?;
    observed.recheck()?;
    ensure!(
        context.policy().writes.project_selection,
        "operation policy forbids project selection writes"
    );
    let identity = ProjectIdentity::from_observation(observed)?;
    let root = crate::libraries::project::location::ProjectLocation::bind(observed)?
        .root()
        .to_string_lossy()
        .into_owned();
    let declaration = declaration_digest(observed);
    let repository = ProjectRepository::open_writer(context)?;
    let key = SelectionKey::new(
        identity.as_uuid(),
        DEFAULT_GROUP,
        candidate.key().target(),
        repository.model().limits(),
    )?;
    let expected = repository.model().snapshot(&key);
    Ok(PreparedSelectionCommit {
        context: context.clone(),
        repository,
        identity,
        root,
        declaration,
        key,
        expected,
        language: language_digest(candidate.key().fingerprint()),
        language_key: *candidate.key(),
    })
}

/// Commits the prepared registration and selection in one CAS step.
/// Protections follow the contract order: project EX first, then the real
/// language SH (re-verifying Ready; the query report alone is not
/// publication evidence), then the catalog CAS.
pub(crate) fn commit_selection(
    prepared: PreparedSelectionCommit,
) -> Result<SelectionCommitReceipt> {
    let PreparedSelectionCommit {
        context,
        mut repository,
        identity,
        root,
        declaration,
        key,
        expected,
        language,
        language_key,
    } = prepared;
    context.checkpoint()?;
    // Project protection first; nothing below may precede it.
    let writer = repository.writer()?;
    let lock = open_project_lock(&writer, &identity)?;
    let _project_guard: ObjectLease = {
        let control = writer.control(ControlLimits::default())?;
        control.acquire_project(&lock)?
    };
    // Object SH second: re-verify Ready and hold the real lease across the
    // catalog replacement.
    let languages = LanguageRepository::open_readonly(&context)?;
    let lease = languages
        .acquire_by_key(&language_key)?
        .context("the selected language is not Ready in the shared library")?;
    // Recheck after waiting: the earlier protections still hold their facts.
    context.checkpoint()?;
    lease.check_binding()?;
    repository.reload()?;
    let fresh = repository.model().snapshot(&key);
    ensure!(
        fresh == expected,
        "the project selection changed while the commit was being prepared"
    );
    let transaction = Uuid::now_v7();
    repository
        .model_mut()
        .register_project(identity.as_uuid(), &root, &declaration)?;
    repository
        .model_mut()
        .apply_language_selection(&key, &expected, &language, transaction)?;
    repository.commit_model()?;
    let revision = repository.model().snapshot(&key).revision();
    lease.check_binding()?;
    Ok(SelectionCommitReceipt { revision })
}

/// Opens the per-project stable lock: an established lock is reopened by its
/// real inode, a first registration creates it exactly once, and an
/// interrupted or foreign file is never silently recreated.
fn open_project_lock(
    writer: &pakro_store::control::StoreWriter,
    identity: &ProjectIdentity,
) -> Result<StableLock> {
    let name = format!("project-{}.lock", identity.as_uuid().hyphenated());
    let build = writer.build_handle();
    match nix::sys::stat::fstatat(
        build,
        name.as_str(),
        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => {
            let expected = FileIdentity {
                device: stat.st_dev,
                inode: stat.st_ino,
            };
            StableLock::open_existing(build, &name, repository_uid(), expected)
        }
        Err(nix::errno::Errno::ENOENT) => StableLock::create_new(build, &name, repository_uid()),
        Err(error) => Err(error.into()),
    }
}

/// SHA256 over every observed declaration document (path and bytes), giving
/// the minimal persisted declaration observation of one selection.
fn declaration_digest(observed: &ObservedProjectInputs) -> String {
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new();
    for path in observed.paths() {
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update([0]);
        if let Some(bytes) = observed.document(path) {
            hash.update(bytes);
        }
        hash.update([0]);
    }
    let digest: [u8; 32] = hash.finalize().into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
