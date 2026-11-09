//! Explicit Python project preparation for install and deps install.

use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::libraries::project::{identity::ProjectIdentity, state::ExpectedProjectState};
use crate::libraries::language::source::PythonRuntimeSource;
use crate::packages::PackageInstallRequest;
use crate::programs::OperationContext;
use crate::programs::shared::python_roots::{self, PythonLockIntent, PythonRoot};
use eyre::{Context, ContextCompat, Result, ensure};

pub use super::shared::python_entry::{
    PythonWriteOperation, is_python_backend, is_python_provider, reject_python_provider_write,
    reject_python_write, validate_python_query_options,
};

/// Inspect the legacy inventory before unrestricted prune can enter its
/// mutation path. Unreadable inventory is an error, never an empty target set.
pub fn preflight_legacy_python_inventory() -> Result<()> {
    for installed in crate::toolset::install_state::try_list_tools()?.values() {
        reject_python_write(
            PythonWriteOperation::Prune,
            &crate::args::BackendArg::from(installed.clone()),
        )?;
    }
    eyre::ensure!(
        !crate::dirs::DATA.join("languages").exists(),
        "Python Prune is not supported by the shared Python transaction path yet; specify explicit non-Python tools before pruning"
    );
    Ok(())
}

pub struct PythonProjectInstall {
    context: OperationContext,
    observed: ObservedProjectInputs,
    version: String,
    source: PythonRuntimeSource,
    roots: Vec<PythonRoot>,
    conda_requests: Vec<PackageInstallRequest>,
    conda_admission: Option<super::install::conda_project::CondaAdmission>,
    intent: PythonLockIntent,
    expected_project: ExpectedProjectState,
    extra: Vec<PackageInstallRequest>,
}

impl PythonProjectInstall {
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn has_packages(&self) -> bool {
        !self.roots.is_empty() || !self.conda_requests.is_empty()
    }
    pub fn only_python_tools(&self) -> bool {
        self.observed.declarations().all(|(_, declarations)| {
            declarations
                .tools
                .iter()
                .all(|tool| super::query::snapshot::python_tool_name(&self.observed, &tool.name))
        })
    }

    pub fn preview_update(&self) -> Result<Vec<String>> {
        ensure!(
            self.intent == PythonLockIntent::Update,
            "lock update preview needs explicit update intent"
        );
        self.context.checkpoint()?;
        self.observed.recheck()?;
        super::shared::lock_transaction::ensure_project_consistent(&self.context, &self.observed)?;
        self.expected_project.ensure_current(
            crate::libraries::project::repository::ProjectRepository::open_readonly(&self.context)?
                .model(),
        )?;
        let path = project_lock_path(&self.observed)?;
        let model = self
            .observed
            .document(&path)
            .map(|bytes| {
                crate::packages::python_resolution::lock_session::decode_root(
                    bytes,
                    Default::default(),
                )
            })
            .transpose()?
            .flatten();
        if self.source == PythonRuntimeSource::Conda {
            let mut lines = vec![format!("Lock update preview for Conda Python {}", self.version)];
            if let Some(model) = &model {
                let locked = model.conda().first().context("existing Conda graph is missing")?;
                lines.push(format!("Locked direct requests: {}", locked.roots.join(", ")));
            } else {
                lines.push("No existing Conda lock".into());
            }
            lines.push(format!("Current direct packages: {}", self.conda_requests.iter()
                .map(|request| format!("{}=={}", request.name, request.version))
                .collect::<Vec<_>>().join(", ")));
            lines.push("Unresolved: channel/build selection, transitive dependencies and artifacts; preview does not resolve".into());
            self.observed.recheck()?;
            self.context.checkpoint()?;
            return Ok(lines);
        }
        let previous: std::collections::BTreeSet<_> = model
            .as_ref()
            .into_iter()
            .flat_map(|model| model.graphs())
            .flat_map(|graph| graph.root_declarations().iter().cloned())
            .collect();
        let current: std::collections::BTreeSet<_> = python_roots::declarations(&self.roots)
            .into_iter()
            .collect();
        let mut lines = vec![format!("Lock update preview for Python {}", self.version)];
        for removed in previous.difference(&current) {
            lines.push(format!("- {removed}"));
        }
        for added in current.difference(&previous) {
            lines.push(format!("+ {added}"));
        }
        if previous == current {
            lines.push("Direct package declarations unchanged".into());
        }
        let request = super::shared::exact_version::exact_python_request(&self.version)?;
        let pin = runtime_pin_for_install(&self.observed, &request, self.intent)?;
        let repository = crate::libraries::language::repository::LanguageRepository::open_readonly(
            &self.context,
        )?;
        let mut ready = Vec::new();
        for language in repository
            .enumerate_published()
            .map_err(|error| eyre::eyre!("language library is unavailable: {error:?}"))?
        {
            if !language.matches_host_platform()
                || language.source_kind() != self.source
                || language.version() != request.version_only()
                || request
                    .build_pin()
                    .is_some_and(|build| language.build() != Some(build))
            {
                continue;
            }
            if let Some(pin) = &pin {
                let expected = crate::plugins::core::python::pinned_shared_language_spec(
                    &self.version,
                    pin,
                    language.spec().patch_sysconfig(),
                    self.context.settings(),
                )?;
                if expected.key() != language.key() {
                    continue;
                }
            }
            let lease = repository
                .acquire_by_key(&language.key())?
                .context("local Python disappeared during preview")?;
            lease.check_binding()?;
            ready.push(language.key().filename());
        }
        if ready.is_empty() {
            lines.push("Local Python: missing".into());
        } else if ready.len() == 1 {
            lines.push(format!("Local Python: verified {}", ready[0]));
        } else {
            lines.push(format!(
                "Local Python: ambiguous ({} verified instances)",
                ready.len()
            ));
        }
        lines.push("Unresolved: target-active roots, transitive dependencies, artifacts and builds; preview does not resolve".into());
        self.observed.recheck()?;
        self.context.checkpoint()?;
        Ok(lines)
    }

    /// Explicit intent may produce a lock with the same catalog commit.
    /// Existing precise locks replay without edits; declarations stay intact.
    pub fn install(
        self,
        config: std::sync::Arc<crate::config::Config>,
    ) -> futures_util::future::BoxFuture<'static, Result<()>> {
        let version = self.version.clone();
        self.install_version(version, config)
    }

    fn install_version(
        mut self,
        version: String,
        config: std::sync::Arc<crate::config::Config>,
    ) -> futures_util::future::BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            self.context.checkpoint()?;
            // Preparation also serves dry-run and admission. Recovery belongs
            // only here, after the CLI has accepted actual execution, and it
            // runs with the plain, unfixed observation semantics.
            if let Some(refreshed) =
                super::shared::lock_transaction::recover_project_lock_and_observe(
                    &self.context,
                    &self.observed,
                )?
            {
                ensure!(
                    super::query::snapshot::effective_python_declaration(&refreshed).as_deref()
                        == Some(self.version.as_str())
                        && refreshed.python_request().map_err(|error| eyre::eyre!("{error}"))?.is_some_and(|request| request.source == self.source),
                    "project Python declaration changed during recovery"
                );
                if self.source == PythonRuntimeSource::Conda {
                    ensure!(super::install::conda_project::requests(&refreshed, &self.version, &self.extra)? == self.conda_requests,
                        "project Conda declarations changed during recovery");
                } else {
                    ensure!(project_roots(&refreshed, &self.version, &self.extra)? == self.roots,
                        "project direct roots changed during recovery");
                }
                self.observed = refreshed;
            }
            // The observation that survived recovery is this installation's
            // initial project lock input, present or missing. Fixing it here
            // keeps configuration and project facts checked while an
            // externally changed lock file can no longer refuse the install.
            self = self.with_fixed_lock_input()?;
            self.observed.recheck()?;
            super::shared::lock_transaction::ensure_project_consistent(
                &self.context,
                &self.observed,
            )?;
            if self.source == PythonRuntimeSource::Conda {
                return super::install::conda_project::install(
                    &self.context, &self.observed, &self.conda_requests, &self.expected_project,
                    self.conda_admission.as_ref().expect("Conda project admission"),
                ).await;
            }
            preflight_project_lock(
                &self.context,
                &self.observed,
                &self.version,
                &self.roots,
                self.intent,
            )?;
            self.expected_project.ensure_current(
                crate::libraries::project::repository::ProjectRepository::open_readonly(
                    &self.context,
                )?
                .model(),
            )?;
            let request = super::shared::exact_version::exact_python_request(&version)?;
            let pin = runtime_pin_for_install(&self.observed, &request, self.intent)?;
            let pin = pin.as_ref().filter(|pin| pin.source_python.is_some() == (self.source == PythonRuntimeSource::Source));
            let language = match self.source {
                PythonRuntimeSource::Prebuilt => crate::plugins::core::python::PythonPlugin::new()
                    .install_shared_exact_in_context(&self.context, &version, config, pin).await?,
                PythonRuntimeSource::Source => crate::libraries::language::source::install_source(
                    &self.context, &version, pin, self.intent == PythonLockIntent::Update).await?,
                PythonRuntimeSource::Conda => eyre::bail!("Conda runtime installation is not implemented"),
            };
            install_project_packages_with_intent(
                &self.context,
                &self.observed,
                &self.roots,
                self.intent,
                &self.expected_project,
                Some(language),
            )
            .await
        })
    }
}

pub(crate) fn project_plan(
    context: OperationContext,
    observed: ObservedProjectInputs,
    extra: &[PackageInstallRequest],
) -> Result<Option<PythonProjectInstall>> {
    project_plan_with_intent(context, observed, extra, PythonLockIntent::Preserve)
}

pub(crate) fn project_plan_with_intent(
    context: OperationContext,
    observed: ObservedProjectInputs,
    extra: &[PackageInstallRequest],
    intent: PythonLockIntent,
) -> Result<Option<PythonProjectInstall>> {
    context.checkpoint()?;
    observed.recheck()?;
    let identity = ProjectIdentity::from_observation(&observed)?;
    let expected_project = ExpectedProjectState::capture(&context, &identity)?;
    project_plan_with_state(context, observed, extra, intent, expected_project)
}

fn project_plan_with_state(
    context: OperationContext,
    observed: ObservedProjectInputs,
    extra: &[PackageInstallRequest],
    intent: PythonLockIntent,
    expected_project: ExpectedProjectState,
) -> Result<Option<PythonProjectInstall>> {
    context.checkpoint()?;
    observed.recheck()?;
    ensure!(
        intent != PythonLockIntent::Update || !context.policy().locked,
        "--update-lock conflicts with strict locked installation"
    );
    let identity = ProjectIdentity::from_observation(&observed)?;
    expected_project.ensure_identity(&identity)?;
    expected_project.ensure_current(
        crate::libraries::project::repository::ProjectRepository::open_readonly(&context)?.model(),
    )?;
    let Some(request) = observed.python_request().map_err(|error| eyre::eyre!("{error}"))?.cloned() else {
        ensure!(
            !extra.iter().any(|request| request.runtime_name == "python"),
            "Python packages need an exact Python declaration in the project"
        );
        return Ok(None);
    };
    let version = request.version;
    super::shared::exact_version::exact_python_request(&version)?;
    if request.source == PythonRuntimeSource::Conda {
        let conda_requests = super::install::conda_project::requests(&observed, &version, extra)?;
        let conda_admission = super::install::conda_project::preflight(&context, &observed, intent)?;
        return Ok(Some(PythonProjectInstall {
            context, observed, version, source: request.source, roots: Vec::new(), conda_requests,
            conda_admission: Some(conda_admission), intent, expected_project, extra: extra.to_vec(),
        }));
    }
    let roots = project_roots(&observed, &version, extra)?;
    preflight_project_lock(&context, &observed, &version, &roots, intent)?;
    expected_project.ensure_current(
        crate::libraries::project::repository::ProjectRepository::open_readonly(&context)?.model(),
    )?;
    Ok(Some(PythonProjectInstall {
        context,
        observed,
        version,
        source: request.source,
        roots,
        conda_requests: Vec::new(),
        conda_admission: None,
        intent,
        expected_project,
        extra: extra.to_vec(),
    }))
}

pub(crate) fn project_roots(
    observed: &ObservedProjectInputs,
    version: &str,
    extra: &[PackageInstallRequest],
) -> Result<Vec<PythonRoot>> {
    let runtime = super::shared::exact_version::exact_python_request(version)?;
    let mut roots = std::collections::BTreeMap::<String, PythonRoot>::new();
    for request in observed
        .effective_packages()?
        .iter()
        .map(|owned| &owned.request)
        .chain(extra)
    {
        ensure!(
            request.runtime_name == "python",
            "only Python package declarations are supported"
        );
        let binding = super::shared::exact_version::exact_python_request(&request.runtime_version)?;
        ensure!(
            binding.version_only() == runtime.version_only()
                && binding
                    .build_pin()
                    .is_none_or(|pin| Some(pin) == runtime.build_pin()),
            "Python package binding {} differs from selected Python {version}",
            request.group_key()
        );
        let root = PythonRoot::from_request(request)?;
        ensure!(
            roots.len() < 4096,
            "Python direct dependency budget exceeded"
        );
        if let Some(previous) = roots.insert(root.name.clone(), root.clone()) {
            ensure!(
                previous == root,
                "conflicting Python package declarations for {}",
                root.name
            );
        }
    }
    observed.recheck()?;
    Ok(roots.into_values().collect())
}

/// Observe and validate all direct inputs before language or package fetching.
pub fn prepare_current_project(
    extra: &[PackageInstallRequest],
) -> Result<Option<PythonProjectInstall>> {
    let context = capture_install_context()?;
    let observed = super::library::observe_current_project(&context)?;
    project_plan(context, observed, extra)
}

pub fn prepare_current_project_update() -> Result<Option<PythonProjectInstall>> {
    let context = capture_install_context()?;
    ensure!(
        !context.policy().locked,
        "--update-lock conflicts with strict locked installation"
    );
    let observed = super::library::observe_current_project(&context)?;
    project_plan_with_intent(context, observed, &[], PythonLockIntent::Update)
}

fn runtime_pin_for_install(
    observed: &ObservedProjectInputs,
    request: &super::shared::exact_version::ExactPythonRequest,
    intent: PythonLockIntent,
) -> Result<Option<crate::lockfile::PlatformInfo>> {
    match intent {
        PythonLockIntent::Preserve => {
            crate::libraries::project::runtime_pin::observed_runtime_pin(observed, request)
        }
        PythonLockIntent::Update => {
            crate::libraries::project::runtime_pin::observed_runtime_pin_for_update(
                observed, request,
            )
        }
    }
}

fn preflight_project_lock(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    version: &str,
    roots: &[PythonRoot],
    intent: PythonLockIntent,
) -> Result<()> {
    let request = super::shared::exact_version::exact_python_request(version)?;
    if intent == PythonLockIntent::Update
        && super::shared::lock_transaction::own_pending_lock_commit(context, observed)?
    {
        // Admission is readonly. Execution must recover its owned journal
        // before any acquisition, then validate the complete recovered result.
        return Ok(());
    }
    let native = runtime_pin_for_install(observed, &request, intent)?;
    if intent == PythonLockIntent::Preserve && let Some(pin) = &native {
        let source = observed.python_request().map_err(|error| eyre::eyre!("{error}"))?.context("project Python declaration is missing")?.source;
        ensure!(pin.source_python.is_some() == (source == PythonRuntimeSource::Source),
            "the Python runtime source differs from pakro.lock; explicitly update the lock after changing source");
    }
    let path = project_lock_path(observed)?;
    let model = observed
        .document(&path)
        .map(|bytes| {
            crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())
        })
        .transpose()?
        .flatten();
    if context.policy().locked {
        ensure!(
            native.is_some() && model.is_some(),
            "strict Python restore requires the complete runtime source and package graph in pakro.lock"
        );
    }
    if let Some(model) = model {
        if intent == PythonLockIntent::Update {
            ensure!(
                !model.graphs().is_empty() && model.declaration_fingerprint().is_some(),
                "the existing precise Python lock is incomplete; --update-lock cannot replace damaged lock facts"
            );
            let bytes = observed
                .document(&path)
                .context("observed lock is missing")?;
            let previous = crate::lockfile::Lockfile::parse_observed(bytes)?;
            ensure!(
                previous
                    .tools()
                    .iter()
                    .any(|(name, entries)| entries.iter().any(|entry| {
                        (name == "python"
                            || super::query::snapshot::python_tool_name(observed, name)
                            || matches!(entry.backend.as_deref(), Some("python" | "core:python")))
                            && !entry.platforms.is_empty()
                    })),
                "the existing precise Python lock is missing its runtime source; --update-lock cannot repair it"
            );
        }
        if intent == PythonLockIntent::Preserve {
            super::install::lock_document::validate_declarations(observed, &model)?;
        }
        let sources = context.python_sources()?;
        for pin in model.pins() {
            ensure!(sources.registries.iter().any(|registry| {
                registry.id().as_str() == pin.file().registry_id()
                    && crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(registry.canonical_base()) == *pin.file().canonical_base_fingerprint()
            }), "strict Python lock requires an unsupported or changed registry source");
        }
        if intent == PythonLockIntent::Update {
            super::install::update::preferred_runtime_pins_from_observation(
                context, observed, &model,
            )?;
            return Ok(());
        }
        let raw = python_roots::declarations(roots);
        ensure!(
            model
                .graphs()
                .iter()
                .any(|graph| graph.root_declarations() == raw),
            "strict Python lock differs from the complete current direct roots"
        );
        ensure!(
            native.is_some(),
            "strict Python lock is missing its exact runtime source"
        );
    }
    Ok(())
}

fn capture_install_context() -> Result<OperationContext> {
    let settings = crate::config::Settings::get();
    capture_install_context_using(
        &settings,
        crate::dirs::DATA.to_path_buf(),
        crate::dirs::CACHE.to_path_buf(),
    )
}

fn capture_install_context_using(
    settings: &crate::config::Settings,
    data_root: std::path::PathBuf,
    cache_root: std::path::PathBuf,
) -> Result<OperationContext> {
    use crate::config::SettingsExt;
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    use std::time::Instant;
    let deadline = Instant::now()
        .checked_add(settings.http_download_timeout())
        .context("shared Python installation deadline overflow")?;
    Ok(capture_operation_context(OperationContextInputs {
        settings,
        policy: OperationPolicy {
            locked: settings.locked,
            offline: settings.offline(),
            deadline,
            budget: RequestBudget {
                max_arguments: 4096,
                max_total_argument_bytes: 1024 * 1024,
                max_declarations: 4096,
                max_runtime_candidates: 64,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: !settings.locked,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root,
        cache_root,
        python_sources: Some(public_sources()?),
        python_target_description: None,
    })?)
}

/// One frozen business observation, captured before CLI package classification.
/// Consuming it never reloads declarations, settings or a new catalog baseline.
pub struct PythonInstallAdmission {
    context: OperationContext,
    observed: ObservedProjectInputs,
    expected_project: ExpectedProjectState,
}

pub fn capture_install_admission() -> Result<PythonInstallAdmission> {
    let context = capture_install_context()?;
    let observed = super::library::observe_current_project(&context)?;
    let identity = ProjectIdentity::from_observation(&observed)?;
    let expected_project = ExpectedProjectState::capture(&context, &identity)?;
    observed.recheck()?;
    Ok(PythonInstallAdmission {
        context,
        observed,
        expected_project,
    })
}

impl PythonInstallAdmission {
    pub fn project(
        self,
        extra: &[PackageInstallRequest],
        update_lock: bool,
    ) -> Result<Option<PythonProjectInstall>> {
        let intent = if update_lock {
            PythonLockIntent::Update
        } else {
            PythonLockIntent::Preserve
        };
        project_plan_with_state(
            self.context,
            self.observed,
            extra,
            intent,
            self.expected_project,
        )
    }

    pub fn exact(
        self,
        version: &str,
        extra: &[PackageInstallRequest],
    ) -> Result<PythonExactInstall> {
        let requested = super::shared::exact_version::exact_python_request(version)?;
        let Self {
            context,
            observed,
            expected_project,
        } = self;
        context.checkpoint()?;
        observed.recheck()?;
        expected_project.ensure_current(
            crate::libraries::project::repository::ProjectRepository::open_readonly(&context)?
                .model(),
        )?;
        let choice = if super::query::snapshot::effective_python_declaration(&observed).is_some() {
            let project = project_plan_with_state(
                context.clone(),
                observed,
                extra,
                PythonLockIntent::Preserve,
                expected_project,
            )?
            .context("the admitted Python declaration disappeared")?;
            let declared = super::shared::exact_version::exact_python_request(project.version())?;
            ensure!(project.source != PythonRuntimeSource::Source || requested.build_pin().is_none(),
                "source CPython does not accept a PBS distribution-build suffix");
            if declared.version_only() == requested.version_only()
                && declared
                    .build_pin()
                    .is_none_or(|pin| Some(pin) == requested.build_pin())
            {
                ExactInstallChoice::Project(project)
            } else {
                ensure!(
                    !context.policy().locked,
                    "strict Python restore cannot select an explicit runtime outside the locked project declaration"
                );
                ensure!(
                    extra.is_empty(),
                    "explicit package bindings require the matching declared project Python"
                );
                ExactInstallChoice::Independent {
                    context,
                    observed: project.observed,
                }
            }
        } else {
            ensure!(
                extra.is_empty(),
                "explicit package bindings require the matching declared project Python"
            );
            ExactInstallChoice::Independent { context, observed }
        };
        Ok(PythonExactInstall {
            version: version.to_owned(),
            choice,
        })
    }
}

enum ExactInstallChoice {
    Project(PythonProjectInstall),
    Independent {
        context: OperationContext,
        observed: ObservedProjectInputs,
    },
}

pub struct PythonExactInstall {
    version: String,
    choice: ExactInstallChoice,
}

impl PythonExactInstall {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn recheck_admission(&self) -> Result<()> {
        match &self.choice {
            ExactInstallChoice::Project(project) => project.recheck_admission(),
            ExactInstallChoice::Independent { context, observed } => {
                context.checkpoint()?;
                Ok(observed.recheck()?)
            }
        }
    }

    pub fn install(
        self,
        config: std::sync::Arc<crate::config::Config>,
    ) -> futures_util::future::BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            match self.choice {
                // The project installation entry performs its recovery and
                // fixes its lock input in one place; an admission recheck here
                // would refuse an externally changed lock file before the
                // installation's own semantics start.
                ExactInstallChoice::Project(project) => {
                    project.install_version(self.version, config).await
                }
                ExactInstallChoice::Independent { context, observed } => {
                    context.checkpoint()?;
                    observed.recheck()?;
                    crate::plugins::core::python::PythonPlugin::new()
                        .install_shared_exact_in_context(&context, &self.version, config, None)
                        .await
                        .map(|_| ())
                }
            }
        })
    }
}

impl PythonProjectInstall {
    /// Fix this installation's project lock input, present or missing, to its
    /// initial observation. Only the actual installation entry calls this;
    /// preview, dry-run, queries and directory selection keep the plain
    /// observation with its original recheck semantics.
    fn with_fixed_lock_input(self) -> Result<Self> {
        let path = project_lock_path(&self.observed)?;
        let observed = self.observed.with_fixed_project_lock(&path)?;
        Ok(Self { observed, ..self })
    }

    pub fn recheck_admission(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.observed.recheck()?;
        self.expected_project.ensure_current(
            crate::libraries::project::repository::ProjectRepository::open_readonly(&self.context)?
                .model(),
        )
    }
}

pub fn prepare_exact_python(
    version: &str,
    extra: &[PackageInstallRequest],
) -> Result<PythonExactInstall> {
    capture_install_admission()?.exact(version, extra)
}

/// Compatibility entry: captures once, then executes that same admitted owner.
pub fn install_exact_python<'a>(
    version: &'a str,
    config: std::sync::Arc<crate::config::Config>,
    extra: &'a [PackageInstallRequest],
) -> futures_util::future::BoxFuture<'a, Result<()>> {
    Box::pin(async move { prepare_exact_python(version, extra)?.install(config).await })
}

#[cfg(test)]
async fn install_project_packages(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    roots: &[PythonRoot],
) -> Result<()> {
    let identity = ProjectIdentity::from_observation(observed)?;
    let expected = ExpectedProjectState::capture(context, &identity)?;
    install_project_packages_with_intent(
        context,
        observed,
        roots,
        PythonLockIntent::Preserve,
        &expected,
        None,
    )
    .await
}

async fn install_project_packages_with_intent(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    roots: &[PythonRoot],
    intent: PythonLockIntent,
    expected_project: &ExpectedProjectState,
    prepared_language: Option<crate::libraries::language::identity::LanguageKey>,
) -> Result<()> {
    use crate::libraries::language::repository::LanguageRepository;
    use crate::libraries::project::repository::ProjectRepository;
    use crate::programs::query::snapshot::{
        LanguageCandidate, SelectionReport, open_query_context, select_existing,
    };
    use crate::programs::shared::commit::{
        commit_package_selection, commit_package_selection_with_lock, commit_selection,
        commit_selection_with_lock, prepare_package_selection_commit_with_state,
        prepare_selection_commit_with_state,
    };
    context.checkpoint()?;
    observed.recheck()?;
    let query = open_query_context(context, observed)?;
    let candidate = if let Some(key) = prepared_language {
        let language = LanguageRepository::open_readonly(context)?
            .acquire_by_key(&key)?
            .context("prepared project Python is not Ready")?;
        LanguageCandidate::new(
            key,
            language.root().to_owned(),
            language.entrypoint().to_owned(),
            language.descriptor().language_version.full_version(),
        )
    } else {
        let SelectionReport::Selected(candidate) = select_existing(&query)? else {
            eyre::bail!(
                "the exact project Python is missing, ambiguous or damaged after installation"
            );
        };
        candidate
    };
    let lock_path = project_lock_path(observed)?;
    // Bind and guard the real target the initial observation resolved; a
    // later re-pointed link must not move the installation's input or its
    // synchronization to a different directory.
    let bind_target = observed
        .fixed_lock_target()
        .map(|target| target.to_path_buf())
        .unwrap_or_else(|| lock_path.clone());
    let root = crate::packages::python_resolution::lock_session::BoundPythonLock::bind_fixed(
        &bind_target,
        observed.document(&lock_path),
        Default::default(),
        context.policy().deadline,
    )?;
    observed.recheck()?;
    expected_project.ensure_current(ProjectRepository::open_readonly(context)?.model())?;
    let preferred_pins = if intent == PythonLockIntent::Update {
        super::install::update::preferred_runtime_pins(context, observed, &root)?
    } else {
        Vec::new()
    };
    let strict =
        intent == PythonLockIntent::Preserve && (context.policy().locked || root.model().is_some());
    let language = LanguageRepository::open_readonly(context)?
        .acquire_by_key(candidate.key())?
        .context("project Python is not Ready")?;
    let active = python_roots::active_roots(roots, &language)?;
    if active.is_empty() {
        let bytes = if strict {
            super::install::lock::validate_project_lock_roots(&root, &language, roots)?;
            None
        } else {
            let graph = crate::packages::python_environment::model::PackageGraphRecord {
                roots: vec![],
                nodes: vec![],
                edges: vec![],
            };
            let model = super::install::lock::project_lock_model_roots(
                &root,
                &language,
                &graph,
                roots,
                &[],
                intent,
            )?;
            Some(prepare_project_document(
                &root, observed, &language, model, intent,
            )?)
        };
        let changes = if intent == PythonLockIntent::Update {
            Some(update_change_report(
                observed,
                root.model(),
                bytes
                    .as_deref()
                    .context("update did not prepare a lock document")?,
                &language,
            )?)
        } else {
            None
        };
        drop(language);
        root.recheck()?;
        drop(root);
        let prepared =
            prepare_selection_commit_with_state(context, observed, &candidate, expected_project)?;
        if let Some(bytes) = bytes {
            commit_selection_with_lock(prepared, &lock_path, &bytes)?;
        } else {
            commit_selection(prepared)?;
        }
        if let Some(changes) = changes {
            print_update_changes(context, changes)?;
        }
        return Ok(());
    }
    let sources = context.python_sources()?;
    ensure!(
        sources.registries.len() == 1,
        "shared wheels require one public registry source"
    );
    let registry = &sources.registries[0];
    let allowed = [(
        registry.id().as_str(),
        crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(
            registry.canonical_base(),
        ),
    )];
    let expected_graph = if strict {
        let graph = super::install::lock::validate_project_lock_roots(&root, &language, roots)?;
        Some(super::install::lock::portable_graph(
            root.model().context("strict graph model is missing")?,
            graph,
        )?)
    } else {
        None
    };
    drop(language);
    // Name/version only finds candidates. Actual held METADATA must prove the
    // complete current intent and least closure before any graph is reused.
    if intent == PythonLockIntent::Preserve {
        let requested = python_roots::pairs(&active);
        let existing = ProjectRepository::open_readonly(context)?
            .read_views()
            .matching_package_graphs(candidate.key().fingerprint(), &requested)?;
        let expected_fingerprint = expected_graph
            .as_ref()
            .map(|graph| graph.fingerprint())
            .transpose()?;
        let mut ready = Vec::new();
        for graph in existing {
            if expected_fingerprint.is_some_and(|expected| graph.fingerprint() != expected) {
                continue;
            }
            if !strict && graph.nodes().iter().any(|node| node.pin.build.is_some()) {
                continue;
            }
            let validation = graph
                .validate_pins(&root, &allowed)
                .and_then(|()| graph.validate_observed_pins(observed, &allowed));
            if let Err(error) = validation {
                if error
                    .downcast_ref::<crate::libraries::project::package_graph::GraphPinMismatch>()
                    .is_some()
                {
                    continue;
                }
                return Err(error);
            }
            if let Some(lease) =
                crate::libraries::package::lease::acquire_layout(context, *graph.layout())?
            {
                if !super::install::reuse::matches_complete_roots(context, &graph, &lease, roots)? {
                    continue;
                }
                ready.push((graph, lease));
            }
        }
        ensure!(
            ready.len() <= 1,
            "multiple shared package layouts match; refusing to select an arbitrary instance"
        );
        if let Some((graph, lease)) = ready.pop() {
            let record = crate::packages::python_environment::model::PackageGraphRecord {
                roots: graph.roots().to_vec(),
                nodes: graph.nodes().to_vec(),
                edges: graph.edges().to_vec(),
            };
            let bytes = if strict {
                None
            } else {
                let model = super::install::lock::project_lock_model_roots(
                    &root,
                    lease.language_lease(),
                    &record,
                    roots,
                    &[],
                    intent,
                )?;
                let model = if let Some(urls) = graph.source_locators() {
                    model.with_locators(urls.clone(), Default::default())?
                } else {
                    model
                };
                Some(prepare_project_document(
                    &root,
                    observed,
                    lease.language_lease(),
                    model,
                    intent,
                )?)
            };
            drop(lease);
            root.recheck()?;
            drop(root);
            let prepared = prepare_package_selection_commit_with_state(
                context,
                observed,
                *candidate.key(),
                *graph.layout(),
                record,
                expected_project,
            )?;
            if let Some(bytes) = bytes {
                commit_package_selection_with_lock(prepared, &lock_path, &bytes)?;
            } else {
                commit_package_selection(prepared)?;
            }
            return Ok(());
        }
    }
    let language = LanguageRepository::open_readonly(context)?
        .acquire_by_key(candidate.key())?
        .context("project Python is not Ready")?;
    resolve_and_commit(
        context,
        observed,
        language,
        root,
        roots,
        intent,
        expected_project,
        preferred_pins,
    )
    .await
}

fn prepare_project_document(
    root: &crate::packages::python_resolution::lock_session::BoundPythonLock,
    observed: &ObservedProjectInputs,
    language: &crate::libraries::language::lease::LanguageLease,
    model: crate::packages::python_resolution::lock_model::PythonLockModel,
    intent: PythonLockIntent,
) -> Result<Vec<u8>> {
    match intent {
        PythonLockIntent::Preserve => {
            super::install::lock_document::prepare_document(root, observed, language, model)
        }
        PythonLockIntent::Update => super::install::lock_document::prepare_document_for_update(
            root, observed, language, model,
        ),
    }
}

async fn resolve_and_commit(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    language: crate::libraries::language::lease::LanguageLease,
    root: crate::packages::python_resolution::lock_session::BoundPythonLock,
    roots: &[PythonRoot],
    intent: PythonLockIntent,
    expected_project: &ExpectedProjectState,
    preferred_pins: Vec<crate::packages::python_resolution::lock_model::ContentPin>,
) -> Result<()> {
    use crate::packages::python_resolution::candidate_policy::{
        CandidatePolicy, Prereleases, Sdists, Yanked,
    };
    use crate::packages::python_resolution::source::registry_depot::{
        RegistryDepotContext, RegistryEndpoint, RegistryRootScope,
    };
    use crate::programs::shared::commit::{
        commit_package_selection, commit_package_selection_with_lock,
        prepare_package_selection_commit_with_state,
    };
    use pakro_index::artifact::NetworkPolicy;
    use pakro_index::python_source::AuthScopeKey;
    use pakro_index::simple::{
        AuthOriginGrants, PythonSourceSession, SimpleFetchPolicy, SimpleIndexClient,
        SourceFileSession,
    };
    observed.recheck()?;
    let cache = context.requested_cache_root().join("shared-python");
    let scratch = cache.join("scratch");
    let client = SimpleIndexClient::new(
        cache.join("simple"),
        shared_registry_http_config(context.settings()),
    )?;
    let sources = context.python_sources()?;
    ensure!(
        sources.registries.len() == 1,
        "shared wheels require one public registry source"
    );
    let registry = &sources.registries[0];
    let scope = AuthScopeKey::from_local_revision([0; 32]);
    let credentials = PublicCredentials;
    let grants = AuthOriginGrants::default();
    let route = FrozenRouter {
        settings: context.settings(),
    };
    let endpoints = [RegistryEndpoint {
        project: PythonSourceSession {
            registry,
            auth_scope: &scope,
            credentials: &credentials,
            auth_origins: &grants,
            transport: &route,
        },
        file: SourceFileSession {
            credential_ref: None,
            auth_scope: &scope,
            credentials: &credentials,
            auth_origins: &grants,
            transport: &route,
        },
    }];
    let registry_context = RegistryDepotContext {
        preferred_pins,
        client: &client,
        endpoints: &endpoints,
        source_policy: &sources.policy,
        candidate_policy: CandidatePolicy {
            prereleases: Prereleases::IfNoMatchingFinal,
            yanked: Yanked::Disallow,
            sdists: Sdists::Include,
            installed_version: None,
        },
        supported: language.target().supported(),
        environment: language.target().environment(),
        root_scope: RegistryRootScope::Environment,
        semantics: language.target().semantics(),
        fetch: SimpleFetchPolicy {
            network: if context.policy().offline {
                NetworkPolicy::Offline
            } else {
                NetworkPolicy::Allowed
            },
            refresh: intent == PythonLockIntent::Update,
            fresh_for: std::time::Duration::from_secs(3600),
            now: std::time::SystemTime::now(),
        },
        limits: Default::default(),
        cache_root: &cache,
        scratch_root: &scratch,
        deadline: context.policy().deadline,
    };
    let graph = super::install::resolve::resolve_project_roots(
        context,
        &language,
        &root,
        roots,
        intent,
        registry_context,
    )
    .await?;
    let facts = graph.package_facts(&language)?;
    let instances: Vec<_> = facts.iter().map(|fact| fact.key).collect();
    let record = graph.projection(&instances)?;
    let strict =
        intent == PythonLockIntent::Preserve && (context.policy().locked || root.model().is_some());
    let bytes = if strict {
        None
    } else {
        let model = super::install::lock::project_lock_model_roots(
            &root,
            &language,
            &record,
            roots,
            graph.builds(),
            intent,
        )?;
        let urls = graph.locators_for_lock(&root, &model)?;
        let model = model.with_locators(urls, Default::default())?;
        Some(prepare_project_document(
            &root, observed, &language, model, intent,
        )?)
    };
    let changes = if intent == PythonLockIntent::Update {
        Some(update_change_report(
            observed,
            root.model(),
            bytes
                .as_deref()
                .context("update did not prepare a lock document")?,
            &language,
        )?)
    } else {
        None
    };
    let graph_key = graph.fingerprint()?;
    let (sources, evidence) = graph.into_package_sources()?;
    ensure!(
        sources.len() == facts.len(),
        "closed package sources differ from graph"
    );
    let mut by_name: std::collections::BTreeMap<_, _> = facts
        .into_iter()
        .map(|fact| (fact.name.clone(), fact))
        .collect();
    let mut inputs = Vec::new();
    for source in sources {
        use crate::packages::python_resolution::source::registry_depot::closed::SharedRegistryPackageSource;
        let name = match &source {
            SharedRegistryPackageSource::Archive(archive) => archive.manifest().name(),
            SharedRegistryPackageSource::Existing(package) => package.manifest().name(),
        };
        let fact = by_name
            .remove(name)
            .context("closed package source has no matching package identity")?;
        inputs.push((fact, source));
    }
    ensure!(by_name.is_empty(), "closed package source is missing");
    let layout = crate::libraries::package::layouts::prepare_layout_with_sources(
        context, &language, graph_key, inputs,
    )?;
    evidence.recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
    observed.recheck()?;
    let layout_key = layout.key();
    drop(layout);
    let language_key = language.key();
    drop(language);
    root.recheck()?;
    drop(evidence);
    drop(root);
    let prepared = prepare_package_selection_commit_with_state(
        context,
        observed,
        language_key,
        layout_key,
        record,
        expected_project,
    )?;
    if let Some(bytes) = bytes {
        commit_package_selection_with_lock(prepared, &project_lock_path(observed)?, &bytes)?;
    } else {
        commit_package_selection(prepared)?;
    }
    if let Some(changes) = changes {
        print_update_changes(context, changes)?;
    }
    Ok(())
}

fn update_change_report(
    observed: &ObservedProjectInputs,
    previous: Option<&crate::packages::python_resolution::lock_model::PythonLockModel>,
    bytes: &[u8],
    language: &crate::libraries::language::lease::LanguageLease,
) -> Result<Vec<String>> {
    use crate::packages::python_resolution::lock_model::PythonLockModel;
    type Rows = std::collections::BTreeSet<(String, String, String, String)>;
    fn rows(model: Option<&PythonLockModel>) -> Result<Rows> {
        let mut result = Rows::new();
        if let Some(model) = model {
            for graph in model.graphs() {
                for node in graph.nodes() {
                    let pin = model
                        .pins()
                        .iter()
                        .find(|pin| pin.id() == node.source_pin())
                        .context("locked graph source pin is missing")?;
                    result.insert((
                        node.name().to_owned(),
                        node.version().to_owned(),
                        pin.file().registry_id().to_owned(),
                        format!("{} {}", pin.file().filename(), pin.digest()),
                    ));
                }
            }
        }
        Ok(result)
    }
    let next =
        crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())?
            .context("updated precise Python graph is missing")?;
    let before = rows(previous)?;
    let after = rows(Some(&next))?;
    let mut versions = std::collections::BTreeSet::new();
    if let Some(bytes) = observed.document(&project_lock_path(observed)?) {
        let native = crate::lockfile::Lockfile::parse_observed(bytes)?;
        for (name, entries) in native.tools() {
            for entry in entries {
                if name == "python"
                    || matches!(entry.backend.as_deref(), Some("python" | "core:python"))
                {
                    versions.insert(entry.version.clone());
                }
            }
        }
    }
    let mut lines = vec![
        "Updated pakro.lock and committed project selection".to_owned(),
        format!(
            "Python: {} -> {}",
            if versions.is_empty() {
                "(none)".to_owned()
            } else {
                versions.into_iter().collect::<Vec<_>>().join(", ")
            },
            language.lock_version()?
        ),
    ];
    let changed = before.symmetric_difference(&after).count();
    if changed == 0 {
        lines.push("Locked package artifacts unchanged".into());
    } else {
        lines.push("Locked package artifact changes:".into());
        for (index, (prefix, row)) in before
            .difference(&after)
            .map(|row| ("-", row))
            .chain(after.difference(&before).map(|row| ("+", row)))
            .enumerate()
        {
            if index == 200 {
                break;
            }
            lines.push(format!(
                "{prefix} {}=={} [{}: {}]",
                row.0, row.1, row.2, row.3
            ));
        }
        if changed > 200 {
            lines.push(format!(
                "{} additional locked artifact changes",
                changed - 200
            ));
        }
    }
    Ok(lines)
}

fn print_update_changes(context: &OperationContext, lines: Vec<String>) -> Result<()> {
    let output: Result<()> = (|| {
        if !context.settings().quiet {
            for line in lines {
                pakroprintln!("{line}");
            }
        }
        Ok(())
    })();
    output
        .wrap_err("The project lock and selection were committed, but printing the changes failed")
}

pub(crate) fn public_sources() -> Result<crate::programs::PythonSourceInputs> {
    use pakro_index::python_source::{PythonRegistry, PythonRegistryId, PythonSourcePolicy};
    Ok(crate::programs::PythonSourceInputs {
        registries: vec![PythonRegistry::new(
            "pypi",
            "https://pypi.org/simple/",
            None,
        )?],
        policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("pypi")?),
    })
}

pub(super) fn project_lock_path(observed: &ObservedProjectInputs) -> Result<std::path::PathBuf> {
    let existing: Vec<_> = observed
        .paths()
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("lock")))
        .collect();
    ensure!(
        existing.len() <= 1,
        "shared wheel installation currently requires a single project lock document"
    );
    if let Some(path) = existing.first() {
        return Ok(path.to_path_buf());
    }
    let config = observed
        .declarations()
        .find(|(_, declarations)| {
            declarations.tools.iter().any(|tool| {
                crate::programs::query::snapshot::python_tool_name(observed, &tool.name)
            })
        })
        .map(|(path, _)| path)
        .context("Python declaration has no owning configuration")?;
    Ok(crate::lockfile::lockfile_path_for_config_with_folder_fragment(config, None, false).0)
}

pub(super) struct PublicCredentials;
impl pakro_index::simple::SourceCredentials for PublicCredentials {
    fn authorization_for(
        &self,
        _: &pakro_index::python_source::CredentialRef,
        _: &pakro_index::simple::HttpOrigin,
        _: pakro_index::artifact::NetworkPolicy,
    ) -> pakro_index::simple::SimpleClientResult<Option<pakro_index::simple::OriginAuthorization>>
    {
        Err(pakro_index::simple::SimpleClientError::Authentication)
    }
}
pub(super) struct FrozenRouter<'a> {
    pub(super) settings: &'a crate::config::Settings,
}
impl pakro_index::simple::SourceRequestRouter for FrozenRouter<'_> {
    fn fingerprint(&self) -> pakro_index::python_source::TransportRulesFingerprint {
        let rules: Vec<_> = self
            .settings
            .url_replacements
            .iter()
            .flat_map(|rules| rules.iter())
            .map(|(from, to)| (from.as_str(), to.as_str()))
            .collect();
        pakro_index::python_source::transport_rules_fingerprint(Some(&rules))
    }
    fn request_url(
        &self,
        canonical: &url::Url,
    ) -> pakro_index::simple::SimpleClientResult<url::Url> {
        crate::plugins::core::python::native::transport::request_url(
            canonical.clone(),
            self.settings.url_replacements.as_ref(),
        )
        .map_err(|_| pakro_index::simple::SimpleClientError::InvalidLocator)
    }
}

pub(super) fn shared_registry_http_config(
    settings: &crate::config::Settings,
) -> pakro_index::simple::SimpleHttpConfig {
    use crate::config::SettingsExt;

    pakro_index::simple::SimpleHttpConfig {
        connect_timeout: settings.http_timeout(),
        read_timeout: Some(settings.http_timeout()),
        request_timeout: settings.http_download_timeout(),
        use_system_proxy: true,
        ..pakro_index::simple::SimpleHttpConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrozenConfigDiscovery;
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    use std::time::{Duration, Instant};

    #[test]
    fn f3_registry_transport_uses_frozen_application_timeouts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut settings = crate::config::Settings::default();
        settings.http_timeout = "2m".into();
        settings.http_download_timeout = "11m".into();
        let context =
            capture_install_context_using(&settings, root.join("data"), root.join("cache"))
                .unwrap();
        settings.http_timeout = "1s".into();
        settings.http_download_timeout = "1s".into();
        let config = shared_registry_http_config(context.settings());
        assert_eq!(config.connect_timeout, Duration::from_secs(120));
        assert_eq!(config.read_timeout, Some(Duration::from_secs(120)));
        assert_eq!(config.request_timeout, Duration::from_secs(660));
        assert!(config.use_system_proxy);
        assert!(!root.join("data").exists());
        assert!(!root.join("cache").exists());
    }

    #[cfg(all(unix, feature = "self_update"))]
    #[tokio::test]
    async fn f2_install_context_configured_deadline_stops_acquire_before_ready() {
        use crate::artifact_snapshot::spool_verified_reader;
        use crate::libraries::language::repository::LanguageRepository;
        use crate::libraries::language::repository::tests::{archive, install_spec};
        use crate::programs::install::runtime::prepare_language;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut settings = crate::config::Settings::default();
        settings.http_download_timeout = "1s".into();
        let context =
            capture_install_context_using(&settings, root.join("data"), root.join("cache"))
                .unwrap();
        let bytes = archive("3.15.0");
        let spec = install_spec(&bytes);
        let entered = AtomicBool::new(false);
        let result = prepare_language(&spec, &context, || async {
            entered.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(1500)).await;
            spool_verified_reader(
                bytes.as_slice(),
                spec.plan().source().filename(),
                spec.plan().source().digest(),
                1024 * 1024,
                context.requested_cache_root(),
            )
        })
        .await;
        // Drop a mistakenly successful lease before independently reopening
        // the authoritative store, so this checks real publication state.
        let failure = result.err();
        assert!(
            entered.load(Ordering::SeqCst),
            "the real acquire closure must run"
        );
        settings.http_download_timeout = "10s".into();
        let inspection =
            capture_install_context_using(&settings, root.join("data"), root.join("cache"))
                .unwrap();
        let published = LanguageRepository::open_readonly(&inspection)
            .unwrap()
            .enumerate_published()
            .unwrap();
        assert!(
            published.is_empty(),
            "an acquire that exceeds the configured deadline must not publish Ready"
        );
        let error = failure.expect("the configured deadline must stop delayed acquisition");
        assert!(
            format!("{error:#}").contains("language preparation timed out"),
            "the real language preparation deadline must fail: {error:#}"
        );
    }

    #[test]
    fn f2_install_context_preserves_longer_configured_deadline_and_frozen_settings() {
        use crate::config::SettingsExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut settings = crate::config::Settings::default();
        settings.http_download_timeout = "11m".into();
        let configured = settings.http_download_timeout();
        let before = Instant::now();
        let context =
            capture_install_context_using(&settings, root.join("data"), root.join("cache"))
                .unwrap();
        let after = Instant::now();
        assert!(
            context.policy().deadline >= before.checked_add(configured).unwrap()
                && context.policy().deadline <= after.checked_add(configured).unwrap(),
            "the configured invocation budget must not be replaced by a 600-second cap"
        );
        settings.http_download_timeout = "1s".into();
        assert_eq!(context.settings().http_download_timeout(), configured);
        let cloned = context.clone();
        assert!(context.same_invocation(&cloned));
        assert_eq!(cloned.policy().deadline, context.policy().deadline);
        context.cancellation_token().cancel();
        assert!(cloned.checkpoint().is_err());
        assert!(!root.join("data").exists());
        assert!(!root.join("cache").exists());
    }

    #[test]
    fn f3_project_inputs_are_exact_before_any_library_write() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            project.join("pakro.toml"),
            "[tools]\npython = \"3.13.16\"\n[packages.\"python@3.13.16\"]\ndemo = \"1.0\"\n",
        )
        .unwrap();
        let context = crate::libraries::language::repository::tests::operation_context(
            &root,
            true,
            Duration::from_secs(60),
        );
        let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        discovery.ceiling_paths = vec![root.clone()];
        let observed = observe_project(
            &project,
            ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60)),
        )
        .unwrap();
        let request = project_plan(context, observed, &[]).unwrap().unwrap();
        assert_eq!(request.version(), "3.13.16");
        assert_eq!(
            request.roots,
            vec![PythonRoot::plain("demo", "1.0").unwrap()]
        );
        assert!(!root.join("data").exists());
    }

    #[test]
    fn p2_locked_project_without_complete_lock_refuses_before_library_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            project.join("pakro.toml"),
            "[tools]\npython='3.13.16'\n[packages.\"python@3.13.16\"]\ndemo='1.0'\n",
        )
        .unwrap();
        let mut settings = crate::config::Settings::default();
        settings.locked = true;
        settings.http_timeout = "10s".into();
        settings.http_download_timeout = "1m".into();
        let context =
            capture_install_context_using(&settings, root.join("data"), root.join("cache"))
                .unwrap();
        let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        discovery.ceiling_paths = vec![root.clone()];
        let observed = observe_project(
            &project,
            ReadPolicy::new(discovery, context.policy().deadline),
        )
        .unwrap();
        let error = project_plan(context, observed, &[]).err().unwrap();
        assert!(format!("{error:#}").contains("complete runtime source"));
        assert!(!root.join("data").exists());
        assert!(!root.join("cache").exists());
        assert_eq!(std::fs::read_dir(&project).unwrap().count(), 1);
    }
}

#[cfg(test)]
#[path = "python/integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[path = "python/p3_recovery_tests.rs"]
mod p3_recovery_tests;

#[cfg(all(test, unix))]
#[path = "python/fixed_lock_install_tests.rs"]
mod fixed_lock_install_tests;
