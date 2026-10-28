//! Single-catalog commit of one project registration and its selection.
//!
//! Reuses the R05 lock and CAS core over one internal catalog. P2 adds an
//! optional durable lock-document journal around that same catalog commit;
//! declaration editing remains outside this phase. Protections are taken in
//! project → object SH → document guard → catalog order and rechecked after
//! waiting. A document written before catalog publication is PendingRecovery.

use eyre::{Context, ContextCompat, Result, ensure};
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
use crate::libraries::project::state::ExpectedProjectState;
use crate::packages::python_environment::model::SelectionKey;
use crate::programs::query::snapshot::LanguageCandidate;
use crate::programs::shared::context::OperationContext;

/// The selection group of a plain directory selection.
pub(crate) const DEFAULT_GROUP: &str = "default";

pub(crate) struct PreparedSelectionCommit<'a> {
    observed: &'a ObservedProjectInputs,
    context: OperationContext,
    repository: ProjectRepository,
    identity: ProjectIdentity,
    root: String,
    declaration: String,
    key: SelectionKey,
    expected: crate::packages::python_environment::model::SelectionStamp,
    expected_project: ExpectedProjectState,
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
#[cfg(test)]
pub(crate) fn prepare_selection_commit<'a>(
    context: &OperationContext,
    observed: &'a ObservedProjectInputs,
    candidate: &LanguageCandidate,
) -> Result<PreparedSelectionCommit<'a>> {
    let identity = ProjectIdentity::from_observation(observed)?;
    let state = ExpectedProjectState::capture(context, &identity)?;
    prepare_selection_commit_with_state(context, observed, candidate, &state)
}

pub(crate) fn prepare_selection_commit_with_state<'a>(
    context: &OperationContext,
    observed: &'a ObservedProjectInputs,
    candidate: &LanguageCandidate,
    state: &ExpectedProjectState,
) -> Result<PreparedSelectionCommit<'a>> {
    context.checkpoint()?;
    observed.recheck()?;
    ensure!(
        context.policy().writes.project_selection,
        "operation policy forbids project selection writes"
    );
    let identity = ProjectIdentity::from_observation(observed)?;
    state.ensure_identity(&identity)?;
    let root = crate::libraries::project::location::ProjectLocation::bind(observed)?
        .root()
        .to_string_lossy()
        .into_owned();
    let declaration = declaration_digest(observed);
    let repository = ProjectRepository::open_writer(context)?;
    state.ensure_current(repository.model())?;
    let key = SelectionKey::new(
        identity.as_uuid(),
        DEFAULT_GROUP,
        candidate.key().target(),
        repository.model().limits(),
    )?;
    let expected = repository.model().snapshot(&key);
    let expected_project = state.clone();
    Ok(PreparedSelectionCommit {
        observed,
        context: context.clone(),
        repository,
        identity,
        root,
        declaration,
        key,
        expected,
        expected_project,
        language: language_digest(candidate.key().fingerprint()),
        language_key: *candidate.key(),
    })
}

/// Commits the prepared registration and selection in one CAS step.
/// Protections follow the contract order: project EX first, then the real
/// language SH (re-verifying Ready; the query report alone is not
/// publication evidence), then the catalog CAS.
pub(crate) fn commit_selection(
    prepared: PreparedSelectionCommit<'_>,
) -> Result<SelectionCommitReceipt> {
    commit_selection_inner(prepared, None)
}
pub(crate) fn commit_selection_with_lock(
    prepared: PreparedSelectionCommit<'_>,
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<SelectionCommitReceipt> {
    commit_selection_inner(prepared, Some((path, bytes)))
}
fn commit_selection_inner(
    prepared: PreparedSelectionCommit<'_>,
    document: Option<(&std::path::Path, &[u8])>,
) -> Result<SelectionCommitReceipt> {
    let PreparedSelectionCommit {
        observed,
        context,
        mut repository,
        identity,
        root,
        declaration,
        key,
        expected,
        expected_project,
        language,
        language_key,
    } = prepared;
    context.checkpoint()?;
    // Project protection first; nothing below may precede it.
    let writer = repository.writer()?;
    let lock =
        open_project_lock_with_requirement(&writer, &identity, expected_project.was_registered())?;
    let _project_guard: ObjectLease = {
        let control = writer.control(ControlLimits::default())?;
        control.acquire_project(&lock)?
    };
    super::lock_transaction::ensure_project_consistent(&context, observed)?;
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
    expected_project.ensure_current(repository.model())?;
    let fresh = repository.model().snapshot(&key);
    ensure!(
        fresh == expected,
        "the project selection changed while the commit was being prepared"
    );
    observed.recheck()?;
    let transaction = Uuid::now_v7();
    if let Some((_, bytes)) = document {
        validate_lock_document(
            observed,
            bytes,
            &lease,
            &crate::packages::python_environment::model::PackageGraphRecord {
                roots: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        )?;
    } else {
        validate_observed_lock_documents(
            observed,
            &lease,
            &crate::packages::python_environment::model::PackageGraphRecord {
                roots: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        )?;
    }
    let pending = document
        .map(|(path, bytes)| {
            super::lock_transaction::PendingLockCommit::apply(
                &context,
                observed,
                writer.clone(),
                identity.as_uuid(),
                transaction,
                expected.revision(),
                super::lock_transaction::project_state(repository.model(), identity.as_uuid()),
                vec![language_digest(language_key.fingerprint())],
                path,
                bytes,
            )
        })
        .transpose()?;
    repository
        .model_mut()
        .register_project(identity.as_uuid(), &root, &declaration)?;
    clear_other_default_targets(repository.model_mut(), &key)?;
    repository
        .model_mut()
        .apply_language_selection(&key, &expected, &language, transaction)?;
    if pending.is_some() {
        super::lock_transaction::commit_cutpoint("before-catalog");
    }
    if let Some(pending) = &pending {
        pending
            .recheck_inputs(observed)
            .wrap_err("PendingRecovery: project inputs changed after lock publication")?;
    } else {
        observed.recheck()?;
    }
    repository.commit_model()?;
    if pending.is_some() {
        super::lock_transaction::commit_cutpoint("after-catalog");
    }
    if let Some(pending) = pending {
        pending.finalize().wrap_err(
            "project selection committed; PendingRecovery: lock transaction finalization failed",
        )?;
    }
    let revision = repository.model().snapshot(&key).revision();
    lease.check_binding()?;
    Ok(SelectionCommitReceipt { revision })
}

/// Opens the per-project stable lock: an established lock is reopened by its
/// real inode, a first registration creates it exactly once, and an
/// interrupted or foreign file is never silently recreated.
#[cfg(test)]
pub(super) fn open_project_lock(
    writer: &pakro_store::control::StoreWriter,
    identity: &ProjectIdentity,
) -> Result<StableLock> {
    open_project_lock_with_requirement(writer, identity, false)
}

pub(crate) fn open_existing_project_lock(
    writer: &pakro_store::control::StoreWriter,
    identity: &ProjectIdentity,
) -> Result<StableLock> {
    open_project_lock_with_requirement(writer, identity, true)
}

fn open_project_lock_with_requirement(
    writer: &pakro_store::control::StoreWriter,
    identity: &ProjectIdentity,
    require_existing: bool,
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
        Err(nix::errno::Errno::ENOENT) => {
            ensure!(
                !require_existing,
                "established project protection is missing; project state is unknown"
            );
            StableLock::create_new(build, &name, repository_uid())
        }
        Err(error) => Err(error.into()),
    }
}

/// SHA256 over every observed declaration document (path and bytes), giving
/// the minimal persisted declaration observation of one selection.
pub(crate) fn declaration_digest(observed: &ObservedProjectInputs) -> String {
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new();
    for path in observed.paths() {
        if path
            .extension()
            .is_some_and(|extension| extension == "lock")
        {
            continue;
        }
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

fn clear_other_default_targets(
    model: &mut crate::packages::python_environment::model::CatalogModel,
    current: &SelectionKey,
) -> Result<()> {
    let retired: Vec<_> = model
        .selection_slots()
        .filter(|(key, stamp)| {
            key.project() == current.project()
                && key.group() == DEFAULT_GROUP
                && *key != current
                && (stamp.language().is_some() || stamp.generation().is_some())
        })
        .map(|(key, stamp)| (key.clone(), stamp.clone()))
        .collect();
    for (key, stamp) in retired {
        model.apply_selection(&key, &stamp, None, Uuid::now_v7())?;
    }
    Ok(())
}

/// A graph description is never publication evidence. Commit reacquires the
/// actual Ready layout after taking the project lock and compares every fact.
pub(crate) struct PreparedPackageSelectionCommit<'a> {
    context: OperationContext,
    observed: &'a ObservedProjectInputs,
    repository: ProjectRepository,
    identity: ProjectIdentity,
    root: String,
    declaration: String,
    key: SelectionKey,
    expected: crate::packages::python_environment::model::SelectionStamp,
    expected_project: ExpectedProjectState,
    language_key: LanguageKey,
    layout_key: [u8; 32],
    graph: crate::packages::python_environment::model::PackageGraphRecord,
}
#[cfg(test)]
pub(crate) fn prepare_package_selection_commit<'a>(
    context: &OperationContext,
    observed: &'a ObservedProjectInputs,
    language_key: LanguageKey,
    layout_key: [u8; 32],
    graph: crate::packages::python_environment::model::PackageGraphRecord,
) -> Result<PreparedPackageSelectionCommit<'a>> {
    let identity = ProjectIdentity::from_observation(observed)?;
    let state = ExpectedProjectState::capture(context, &identity)?;
    prepare_package_selection_commit_with_state(
        context,
        observed,
        language_key,
        layout_key,
        graph,
        &state,
    )
}

pub(crate) fn prepare_package_selection_commit_with_state<'a>(
    context: &OperationContext,
    observed: &'a ObservedProjectInputs,
    language_key: LanguageKey,
    layout_key: [u8; 32],
    graph: crate::packages::python_environment::model::PackageGraphRecord,
    state: &ExpectedProjectState,
) -> Result<PreparedPackageSelectionCommit<'a>> {
    context.checkpoint()?;
    observed.recheck()?;
    ensure!(
        context.policy().writes.project_selection,
        "operation policy forbids project selection writes"
    );
    let identity = ProjectIdentity::from_observation(observed)?;
    state.ensure_identity(&identity)?;
    let root = crate::libraries::project::location::ProjectLocation::bind(observed)?
        .root()
        .to_string_lossy()
        .into_owned();
    let declaration = declaration_digest(observed);
    let repository = ProjectRepository::open_writer(context)?;
    state.ensure_current(repository.model())?;
    let key = SelectionKey::new(
        identity.as_uuid(),
        DEFAULT_GROUP,
        language_key.target(),
        repository.model().limits(),
    )?;
    let expected = repository.model().snapshot(&key);
    let expected_project = state.clone();
    Ok(PreparedPackageSelectionCommit {
        context: context.clone(),
        observed,
        repository,
        identity,
        root,
        declaration,
        key,
        expected,
        expected_project,
        language_key,
        layout_key,
        graph,
    })
}

pub(crate) fn commit_package_selection(
    prepared: PreparedPackageSelectionCommit<'_>,
) -> Result<SelectionCommitReceipt> {
    commit_package_selection_inner(prepared, None)
}
pub(crate) fn commit_package_selection_with_lock(
    prepared: PreparedPackageSelectionCommit<'_>,
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<SelectionCommitReceipt> {
    commit_package_selection_inner(prepared, Some((path, bytes)))
}
fn commit_package_selection_inner(
    prepared: PreparedPackageSelectionCommit<'_>,
    document: Option<(&std::path::Path, &[u8])>,
) -> Result<SelectionCommitReceipt> {
    use crate::packages::python_environment::model::{GenerationRecord, InputKind, InputReference};
    use pakro_store::Digest;
    let PreparedPackageSelectionCommit {
        context,
        observed,
        mut repository,
        identity,
        root,
        declaration,
        key,
        expected,
        expected_project,
        language_key,
        layout_key,
        graph,
    } = prepared;
    context.checkpoint()?;
    let writer = repository.writer()?;
    let lock =
        open_project_lock_with_requirement(&writer, &identity, expected_project.was_registered())?;
    let _project_guard = {
        let control = writer.control(ControlLimits::default())?;
        control.acquire_project(&lock)?
    };
    super::lock_transaction::ensure_project_consistent(&context, observed)?;
    observed.recheck()?;
    let layout = crate::libraries::package::lease::acquire_layout(&context, layout_key)?
        .context("selected package layout is not Ready")?;
    ensure!(
        layout.language() == language_key.fingerprint()
            && layout.language_lease().key() == language_key,
        "selected layout language changed"
    );
    ensure!(
        graph.fingerprint()? == layout.graph(),
        "selected graph facts differ from the Ready layout graph identity"
    );
    let manifest: crate::libraries::package::manifest::LayoutManifest =
        crate::libraries::package::manifest::read_json(
            &context,
            &crate::libraries::package::manifest::layout_name(&layout_key),
        )?
        .context("selected layout manifest disappeared")?;
    ensure!(
        manifest.storage.key == layout_key
            && manifest.language == layout.language()
            && manifest.graph == layout.graph(),
        "layout manifest differs from its real lease"
    );
    let mut packages = std::collections::BTreeMap::new();
    for package in layout.packages() {
        packages.insert(package.manifest().facts.key, &package.manifest().facts);
    }
    ensure!(
        packages.len() == graph.nodes.len(),
        "selected graph package count differs from Ready layout"
    );
    for (index, node) in graph.nodes.iter().enumerate() {
        let facts = packages
            .get(&node.instance)
            .context("graph package is absent from Ready layout")?;
        ensure!(
            facts.name == node.name
                && facts.version == node.version
                && facts.artifact == node.artifact
                && facts.source == node.source_pin,
            "graph node differs from its verified package manifest"
        );
        let mut dependencies: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.from == index)
            .map(|edge| {
                graph
                    .nodes
                    .get(edge.to)
                    .map(|node| node.instance)
                    .context("graph dependency target is missing")
            })
            .collect::<Result<_>>()?;
        dependencies.sort_unstable();
        dependencies.dedup();
        ensure!(
            dependencies == facts.dependencies,
            "graph edges differ from verified package dependencies"
        );
    }
    repository.reload()?;
    expected_project.ensure_current(repository.model())?;
    ensure!(
        repository.model().snapshot(&key) == expected,
        "project selection changed while preparing package commit"
    );
    observed.recheck()?;
    let limits = repository.model().limits();
    let transaction = Uuid::now_v7();
    if let Some((_, bytes)) = document {
        validate_lock_document(observed, bytes, layout.language_lease(), &graph)?;
    } else {
        validate_observed_lock_documents(observed, layout.language_lease(), &graph)?;
    }
    let pending = document
        .map(|(path, bytes)| {
            let mut references = vec![
                language_digest(language_key.fingerprint()),
                language_digest(layout_key),
            ];
            for node in &graph.nodes {
                references.push(language_digest(node.instance));
                references.push(language_digest(
                    *crate::packages::python_resolution::lock_model::PinId::parse_hex(
                        &node.source_pin,
                    )?
                    .as_bytes(),
                ));
                references.push(node.artifact.clone());
                if let Some(build) = &node.pin.build {
                    references.push(language_digest(build.request_id));
                    references.push(build.source_digest.clone());
                    references.push(build.output_digest.clone());
                    for input in build.inputs.iter().chain(
                        build
                            .observation
                            .iter()
                            .flat_map(|observation| &observation.initial_inputs),
                    ) {
                        references.push(language_digest(
                            *crate::packages::python_resolution::lock_model::PinId::parse_hex(
                                &input.source_pin,
                            )?
                            .as_bytes(),
                        ));
                        references.push(input.source_digest.clone());
                        references.push(input.output_digest.clone());
                    }
                }
            }
            references.sort_unstable();
            references.dedup();
            super::lock_transaction::PendingLockCommit::apply(
                &context,
                observed,
                writer.clone(),
                identity.as_uuid(),
                transaction,
                expected.revision(),
                super::lock_transaction::project_state(repository.model(), identity.as_uuid()),
                references,
                path,
                bytes,
            )
        })
        .transpose()?;
    let mut inputs = vec![InputReference::new(
        InputKind::Language,
        Digest::Sha256(language_key.fingerprint()),
    )];
    for node in &graph.nodes {
        inputs.push(InputReference::new(
            InputKind::Package,
            Digest::Sha256(node.instance),
        ));
        inputs.push(InputReference::new(
            InputKind::Artifact,
            node.artifact.parse()?,
        ));
    }
    let record = GenerationRecord::new_shared(
        &manifest.storage.allocation,
        language_key.target(),
        layout.graph(),
        *manifest.storage.inventory.inventory_sha256(),
        layout_key,
        inputs,
        limits,
    )?
    .with_package_graph(graph, limits)?;
    let allocation = repository.model_mut().register_shared_generation(record)?;
    repository
        .model_mut()
        .register_project(identity.as_uuid(), &root, &declaration)?;
    clear_other_default_targets(repository.model_mut(), &key)?;
    repository
        .model_mut()
        .apply_selection(&key, &expected, Some(&allocation), transaction)?;
    layout.check_binding()?;
    if pending.is_some() {
        super::lock_transaction::commit_cutpoint("before-catalog");
    }
    if let Some(pending) = &pending {
        pending
            .recheck_inputs(observed)
            .wrap_err("PendingRecovery: project inputs changed after lock publication")?;
    } else {
        observed.recheck()?;
    }
    repository.commit_model()?;
    if pending.is_some() {
        super::lock_transaction::commit_cutpoint("after-catalog");
    }
    if let Some(pending) = pending {
        pending.finalize().wrap_err(
            "project selection committed; PendingRecovery: lock transaction finalization failed",
        )?;
    }
    Ok(SelectionCommitReceipt {
        revision: repository.model().snapshot(&key).revision(),
    })
}

fn validate_observed_lock_documents(
    observed: &ObservedProjectInputs,
    language: &crate::libraries::language::lease::LanguageLease,
    graph: &crate::packages::python_environment::model::PackageGraphRecord,
) -> Result<()> {
    let mut count = 0;
    for path in observed.paths().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "lock")
    }) {
        let bytes = observed
            .document(path)
            .context("observed lock document is missing")?;
        if crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())?
            .is_none()
        {
            continue;
        }
        count += 1;
        ensure!(
            count == 1,
            "selection commit requires one applicable exact Python lock"
        );
        validate_lock_document(observed, bytes, language, graph)
            .wrap_err("selection commit differs from the observed precise project lock")?;
    }
    Ok(())
}

fn validate_lock_document(
    observed: &ObservedProjectInputs,
    bytes: &[u8],
    language: &crate::libraries::language::lease::LanguageLease,
    graph: &crate::packages::python_environment::model::PackageGraphRecord,
) -> Result<()> {
    let native = crate::lockfile::Lockfile::parse_observed(bytes)?;
    let model =
        crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())?
            .context("prepared project lock has no exact Python graph")?;
    crate::programs::install::lock_document::validate_declarations(observed, &model)?;
    let declared =
        crate::programs::python::project_roots(observed, &language.lock_version()?, &[])?;
    let mut roots: std::collections::BTreeMap<_, _> = declared
        .into_iter()
        .map(|root| (root.name.clone(), root))
        .collect();
    // Existing explicit package installs can add plain temporary pins. The
    // coordinator admits that intent before preparation; declared extras and
    // markers must keep their complete original syntax here.
    for index in &graph.roots {
        let node = graph
            .nodes
            .get(*index)
            .context("prepared graph root is missing")?;
        if !roots.contains_key(&node.name) {
            roots.insert(
                node.name.clone(),
                crate::programs::shared::python_roots::PythonRoot::plain(
                    &node.name,
                    &node.version,
                )?,
            );
        }
    }
    let roots: Vec<_> = roots.into_values().collect();
    let locked = crate::programs::install::lock::validate_project_lock_model_roots(
        &model, language, &roots,
    )?;
    ensure!(
        crate::programs::install::lock::portable_graph(&model, locked)?.fingerprint()?
            == graph.fingerprint()?,
        "prepared lock differs from the actual selected complete package graph"
    );
    let version = language.lock_version()?;
    let source = language.source_pin()?;
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let mut matching = native
        .tools()
        .get("python")
        .into_iter()
        .flatten()
        .filter(|entry| entry.version == version);
    let entry = matching
        .next()
        .context("prepared lock omits its exact runtime version and build")?;
    ensure!(
        matching.next().is_none(),
        "prepared runtime lock has ambiguous or unsupported entries"
    );
    validate_runtime_lock_entry(entry, &source, &target)
}

fn validate_runtime_lock_entry(
    entry: &crate::lockfile::LockfileTool,
    source: &crate::lockfile::PlatformInfo,
    target: &str,
) -> Result<()> {
    use crate::libraries::language::source::PythonRuntimeSource;
    let options = entry.options.iter()
        .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
        .collect();
    let expected_source = PythonRuntimeSource::from_options(&options)?;
    let actual_source = if source.source_python.is_some() {
        PythonRuntimeSource::Source
    } else {
        PythonRuntimeSource::Prebuilt
    };
    ensure!(
        expected_source == actual_source
            && entry.backend.as_deref().is_none_or(|backend| matches!(backend, "core:python" | "python")),
        "prepared runtime lock has unsupported entries or a different runtime source kind"
    );
    let pin = entry
        .platforms
        .get(target)
        .context("prepared lock omits its actual runtime target")?;
    ensure!(
        pin.conda_deps.is_none() && pin.additional_artifacts.is_empty(),
        "prepared native runtime lock contains unsupported Conda or additional-artifact inputs"
    );
    ensure!(
        pin.install == source.install
            && pin.source_python == source.source_python
            && pin.url == source.url && pin.checksum == source.checksum,
        "prepared native runtime lock differs from its actual protected source or complete build result"
    );
    Ok(())
}

#[cfg(test)]
#[path = "commit/tests.rs"]
mod tests;
