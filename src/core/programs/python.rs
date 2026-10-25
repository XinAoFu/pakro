//! Explicit Python project preparation for install and deps install.

use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::packages::PackageInstallRequest;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};

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
    roots: Vec<(String, String)>,
    extra: Vec<PackageInstallRequest>,
}

impl PythonProjectInstall {
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn has_packages(&self) -> bool {
        !self.roots.is_empty()
    }
    pub fn only_python_tools(&self) -> bool {
        self.observed.declarations().all(|(_, declarations)| {
            declarations
                .tools
                .iter()
                .all(|tool| super::query::snapshot::python_tool_name(&self.observed, &tool.name))
        })
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
            self.observed.recheck()?;
            // Preparation also serves dry-run and admission. Recovery belongs
            // only here, after the CLI has accepted actual execution.
            if let Some(refreshed) =
                super::shared::lock_transaction::recover_project_lock_and_observe(
                    &self.context,
                    &self.observed,
                )?
            {
                ensure!(
                    super::query::snapshot::effective_python_declaration(&refreshed).as_deref()
                        == Some(self.version.as_str()),
                    "project Python declaration changed during recovery"
                );
                ensure!(
                    project_roots(&refreshed, &self.version, &self.extra)? == self.roots,
                    "project direct roots changed during recovery"
                );
                preflight_project_lock(&self.context, &refreshed, &self.version, &self.roots)?;
                self.observed = refreshed;
            }
            super::shared::lock_transaction::ensure_project_consistent(
                &self.context,
                &self.observed,
            )?;
            let request = super::shared::exact_version::exact_python_request(&version)?;
            let pin = crate::libraries::project::runtime_pin::observed_runtime_pin(
                &self.observed,
                &request,
            )?;
            crate::plugins::core::python::PythonPlugin::new()
                .install_shared_exact_in_context(&self.context, &version, config, pin.as_ref())
                .await?;
            install_project_packages(&self.context, &self.observed, &self.roots).await
        })
    }
}

pub(crate) fn project_plan(
    context: OperationContext,
    observed: ObservedProjectInputs,
    extra: &[PackageInstallRequest],
) -> Result<Option<PythonProjectInstall>> {
    context.checkpoint()?;
    observed.recheck()?;
    let version = super::query::snapshot::effective_python_declaration(&observed);
    let Some(version) = version else {
        ensure!(
            !extra.iter().any(|request| request.runtime_name == "python"),
            "Python packages need an exact Python declaration in the project"
        );
        return Ok(None);
    };
    super::shared::exact_version::exact_python_request(&version)?;
    let roots = project_roots(&observed, &version, extra)?;
    preflight_project_lock(&context, &observed, &version, &roots)?;
    Ok(Some(PythonProjectInstall {
        context,
        observed,
        version,
        roots,
        extra: extra.to_vec(),
    }))
}

pub(crate) fn project_roots(
    observed: &ObservedProjectInputs,
    version: &str,
    extra: &[PackageInstallRequest],
) -> Result<Vec<(String, String)>> {
    use pakro_index::python_source::NormalizedPythonName;
    use pakro_resolver::solve::{Pep440, VersionAdapter};
    let runtime = super::shared::exact_version::exact_python_request(version)?;
    let mut roots = std::collections::BTreeMap::<String, String>::new();
    let mut add = |name: &str, version: &str| -> Result<()> {
        ensure!(
            roots.len() < 4096 && name.len() <= 4096 && version.len() <= 4096,
            "Python direct dependency budget exceeded"
        );
        let name = NormalizedPythonName::new(name)?.as_str().to_owned();
        ensure!(
            version.trim() == version
                && !version.is_empty()
                && Pep440::parse_version(version).is_some(),
            "Python package {name} needs a complete exact upstream version"
        );
        if let Some(previous) = roots.insert(name.clone(), version.to_owned()) {
            ensure!(
                previous == version,
                "conflicting exact Python package declarations for {name}"
            );
        }
        Ok(())
    };
    for owned in observed
        .effective_packages()?
        .iter()
        .map(|owned| &owned.request)
        .chain(extra)
    {
        if owned.runtime_name != "python" {
            continue;
        }
        let binding = super::shared::exact_version::exact_python_request(&owned.runtime_version)?;
        ensure!(
            binding.version_only() == runtime.version_only()
                && binding
                    .build_pin()
                    .is_none_or(|pin| Some(pin) == runtime.build_pin()),
            "Python package binding {} differs from selected Python {version}",
            owned.group_key()
        );
        add(&owned.name, &owned.version)?;
    }
    if let Some(file) = observed.python_project() {
        let selection = file.select(
            crate::packages::python_declarations::PythonProjectSelection {
                include_project_dependencies: true,
                extras: &[],
                groups: &[],
            },
            Default::default(),
        )?;
        ensure!(
            selection.declarations().metadata_need().is_none(),
            "dynamic Python dependency metadata is outside the shared wheel path"
        );
        for requirement in selection.declarations().known().requirements() {
            let syntax = requirement.syntax();
            ensure!(
                syntax.requested_extras().next().is_none()
                    && syntax.marker().is_none()
                    && syntax.unresolved_reference().is_none(),
                "shared wheel direct dependencies currently require plain exact registry pins"
            );
            let raw = syntax
                .raw_version_specifiers()
                .context("Python direct dependency needs an exact == version")?;
            let exact = raw
                .trim()
                .strip_prefix("==")
                .filter(|value| !value.starts_with('='))
                .context("Python direct dependency needs an exact == version")?;
            add(syntax.name(), exact.trim())?;
        }
    }
    observed.recheck()?;
    Ok(roots.into_iter().collect())
}

/// Observe and validate all direct inputs before language or package fetching.
pub fn prepare_current_project(
    extra: &[PackageInstallRequest],
) -> Result<Option<PythonProjectInstall>> {
    let context = capture_install_context()?;
    let observed = super::library::observe_current_project(&context)?;
    project_plan(context, observed, extra)
}

fn preflight_project_lock(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    version: &str,
    roots: &[(String, String)],
) -> Result<()> {
    let request = super::shared::exact_version::exact_python_request(version)?;
    let native = crate::libraries::project::runtime_pin::observed_runtime_pin(observed, &request)?;
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
        super::install::lock_document::validate_declarations(observed, &model)?;
        let sources = context.python_sources()?;
        for pin in model.pins() {
            ensure!(sources.registries.iter().any(|registry| {
                registry.id().as_str() == pin.file().registry_id()
                    && crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(registry.canonical_base()) == *pin.file().canonical_base_fingerprint()
            }), "strict Python lock requires an unsupported or changed registry source");
        }
        let raw = roots
            .iter()
            .map(|(name, version)| format!("{name}=={version}"))
            .collect::<Vec<_>>();
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

/// One explicit complete Python request, independent of legacy tool resolution.
pub fn install_exact_python<'a>(
    version: &'a str,
    config: std::sync::Arc<crate::config::Config>,
    extra: &'a [PackageInstallRequest],
) -> futures_util::future::BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let requested = super::shared::exact_version::exact_python_request(version)?;
        // Capture once, including the global/no-project branch. Reusing this
        // invocation preserves the original deadline, source policy and settings.
        let context = capture_install_context()?;
        let observed = super::library::observe_current_project(&context)?;
        if let Some(project) = project_plan(context.clone(), observed, extra)? {
            let declared = super::shared::exact_version::exact_python_request(project.version())?;
            if declared.version_only() == requested.version_only()
                && declared
                    .build_pin()
                    .is_none_or(|pin| Some(pin) == requested.build_pin())
            {
                return project.install_version(version.to_owned(), config).await;
            }
            ensure!(
                !context.policy().locked,
                "strict Python restore cannot select an explicit runtime outside the locked project declaration"
            );
        }
        ensure!(
            extra.is_empty(),
            "explicit package bindings require the matching declared project Python"
        );
        crate::plugins::core::python::PythonPlugin::new()
            .install_shared_exact_in_context(&context, version, config, None)
            .await
    })
}

async fn install_project_packages(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    roots: &[(String, String)],
) -> Result<()> {
    use crate::libraries::language::repository::LanguageRepository;
    use crate::libraries::project::repository::ProjectRepository;
    use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
    use crate::programs::shared::commit::{
        commit_package_selection, commit_package_selection_with_lock, commit_selection,
        commit_selection_with_lock, prepare_package_selection_commit, prepare_selection_commit,
    };
    context.checkpoint()?;
    observed.recheck()?;
    let query = open_query_context(context, observed)?;
    let SelectionReport::Selected(candidate) = select_existing(&query)? else {
        eyre::bail!("the exact project Python is missing, ambiguous or damaged after installation");
    };
    let lock_path = project_lock_path(observed)?;
    let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(
        &lock_path,
        Default::default(),
        context.policy().deadline,
    )?;
    let strict = context.policy().locked || root.model().is_some();
    if roots.is_empty() {
        let language = LanguageRepository::open_readonly(context)?
            .acquire_by_key(candidate.key())?
            .context("project Python is not Ready")?;
        check_requires_python(observed, &language)?;
        let bytes = if strict {
            super::install::lock::validate_project_lock(&root, &language, roots)?;
            None
        } else {
            let graph = crate::packages::python_environment::model::PackageGraphRecord {
                roots: vec![],
                nodes: vec![],
                edges: vec![],
            };
            let model =
                super::install::lock::project_lock_model(&root, &language, &graph, roots, &[])?;
            Some(super::install::lock_document::prepare_document(
                &root, observed, &language, model,
            )?)
        };
        drop(language);
        root.recheck()?;
        drop(root);
        let prepared = prepare_selection_commit(context, observed, &candidate)?;
        if let Some(bytes) = bytes {
            commit_selection_with_lock(prepared, &lock_path, &bytes)?;
        } else {
            commit_selection(prepared)?;
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
    observed.recheck()?;
    let expected_graph = if strict {
        let language = LanguageRepository::open_readonly(context)?
            .acquire_by_key(candidate.key())?
            .context("project Python is not Ready")?;
        let graph = super::install::lock::validate_project_lock(&root, &language, roots)?;
        Some(super::install::lock::portable_graph(
            root.model().context("strict graph model is missing")?,
            graph,
        )?)
    } else {
        None
    };
    let requested: Vec<_> = roots
        .iter()
        .map(|(name, version)| (name.as_str(), version.as_str()))
        .collect();
    let existing = ProjectRepository::open_readonly(context)?
        .read_views()
        .matching_package_graphs(candidate.key().fingerprint(), &requested)?;
    let mut ready = Vec::new();
    for graph in existing {
        if let Some(expected) = &expected_graph {
            if graph.fingerprint() != expected.fingerprint()? {
                continue;
            }
        } else if graph.nodes().iter().any(|node| node.pin.build.is_some()) {
            // Historical source receipts lack the complete original build
            // observations needed to produce the first portable strict lock.
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
            lease.check_binding()?;
            super::query::snapshot::validate_layout_graph(&graph, &lease)?;
            check_requires_python(observed, lease.language_lease())?;
            ready.push((graph, lease));
        }
    }
    ensure!(
        ready.len() <= 1,
        "multiple shared package layouts match; refusing to select an arbitrary instance"
    );
    if let Some((graph, lease)) = ready.pop() {
        lease.check_binding()?;
        let record = crate::packages::python_environment::model::PackageGraphRecord {
            roots: graph.roots().to_vec(),
            nodes: graph.nodes().to_vec(),
            edges: graph.edges().to_vec(),
        };
        let bytes = if strict {
            None
        } else {
            let model = super::install::lock::project_lock_model(
                &root,
                lease.language_lease(),
                &record,
                roots,
                &[],
            )?;
            Some(super::install::lock_document::prepare_document(
                &root,
                observed,
                lease.language_lease(),
                model,
            )?)
        };
        drop(lease);
        root.recheck()?;
        drop(root);
        let prepared = prepare_package_selection_commit(
            context,
            observed,
            *candidate.key(),
            *graph.layout(),
            record,
        )?;
        if let Some(bytes) = bytes {
            commit_package_selection_with_lock(prepared, &lock_path, &bytes)?;
        } else {
            commit_package_selection(prepared)?;
        }
        return Ok(());
    }
    let language = LanguageRepository::open_readonly(context)?
        .acquire_by_key(candidate.key())?
        .context("project Python is not Ready")?;
    check_requires_python(observed, &language)?;
    resolve_and_commit(context, observed, language, root, &requested).await
}

pub(crate) fn check_requires_python(
    observed: &ObservedProjectInputs,
    language: &crate::libraries::language::lease::LanguageLease,
) -> Result<()> {
    if let Some(specification) = observed
        .python_project()
        .and_then(|file| file.parsed().project())
        .and_then(|project| project.requires_python())
    {
        let marker = crate::packages::python_resolution::metadata_input::checked_metadata_context(
            language.target().environment(),
            Default::default(),
        )?;
        ensure!(
            pakro_index::python_requires_matches_for_context(specification, &marker)?,
            "the selected Python does not satisfy project.requires-python"
        );
    }
    Ok(())
}

async fn resolve_and_commit(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    language: crate::libraries::language::lease::LanguageLease,
    root: crate::packages::python_resolution::lock_session::BoundPythonLock,
    roots: &[(&str, &str)],
) -> Result<()> {
    use crate::packages::python_resolution::candidate_policy::{
        CandidatePolicy, Prereleases, Sdists, Yanked,
    };
    use crate::packages::python_resolution::source::registry_depot::{
        RegistryDepotContext, RegistryEndpoint, RegistryRootScope,
    };
    use crate::programs::shared::commit::{
        commit_package_selection, commit_package_selection_with_lock,
        prepare_package_selection_commit,
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
            refresh: false,
            fresh_for: std::time::Duration::from_secs(3600),
            now: std::time::SystemTime::now(),
        },
        limits: Default::default(),
        cache_root: &cache,
        scratch_root: &scratch,
        deadline: context.policy().deadline,
    };
    let graph = super::install::resolve::resolve_exact_roots(
        context,
        &language,
        &root,
        roots,
        registry_context,
    )
    .await?;
    let facts = graph.package_facts(&language)?;
    let instances: Vec<_> = facts.iter().map(|fact| fact.key).collect();
    let record = graph.projection(&instances)?;
    let strict = context.policy().locked || root.model().is_some();
    let bytes = if strict {
        None
    } else {
        let owned_roots = roots
            .iter()
            .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
            .collect::<Vec<_>>();
        let model = super::install::lock::project_lock_model(
            &root,
            &language,
            &record,
            &owned_roots,
            graph.builds(),
        )?;
        Some(super::install::lock_document::prepare_document(
            &root, observed, &language, model,
        )?)
    };
    let graph_key = graph.fingerprint()?;
    let (sources, evidence) = graph.into_sources()?;
    ensure!(
        sources.len() == facts.len(),
        "closed package sources differ from graph"
    );
    let mut by_name: std::collections::BTreeMap<_, _> = facts
        .into_iter()
        .map(|fact| (fact.name.clone(), fact))
        .collect();
    let mut inputs = Vec::new();
    for archive in sources {
        let fact = by_name
            .remove(archive.manifest().name())
            .context("closed wheel has no matching package identity")?;
        inputs.push((fact, archive));
    }
    ensure!(by_name.is_empty(), "closed package source is missing");
    let layout =
        crate::libraries::package::layouts::prepare_layout(context, &language, graph_key, inputs)?;
    evidence.recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
    observed.recheck()?;
    let layout_key = layout.key();
    drop(layout);
    let language_key = language.key();
    drop(language);
    root.recheck()?;
    drop(evidence);
    drop(root);
    let prepared =
        prepare_package_selection_commit(context, observed, language_key, layout_key, record)?;
    if let Some(bytes) = bytes {
        commit_package_selection_with_lock(prepared, &project_lock_path(observed)?, &bytes)?;
    } else {
        commit_package_selection(prepared)?;
    }
    Ok(())
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

fn project_lock_path(observed: &ObservedProjectInputs) -> Result<std::path::PathBuf> {
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

struct PublicCredentials;
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
struct FrozenRouter<'a> {
    settings: &'a crate::config::Settings,
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

fn shared_registry_http_config(
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
        assert_eq!(request.roots, vec![("demo".into(), "1.0".into())]);
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
