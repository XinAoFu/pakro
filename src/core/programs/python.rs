//! Explicit Python project preparation for install and deps install.

use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::packages::PackageInstallRequest;
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};

pub struct PythonProjectInstall {
    context: OperationContext,
    observed: ObservedProjectInputs,
    version: String,
    roots: Vec<(String, String)>,
}

impl PythonProjectInstall {
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn has_packages(&self) -> bool {
        !self.roots.is_empty()
    }
    pub fn only_python_tools(&self) -> bool {
        self.observed
            .declarations()
            .all(|(_, declarations)| declarations.tools.iter().all(|tool| tool.name == "python"))
    }

    /// Called only after explicit installation intent. Preparation never edits
    /// project declarations or its lock; all associations use the one catalog.
    pub fn install(
        self,
        config: std::sync::Arc<crate::config::Config>,
    ) -> futures_util::future::BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let request = super::shared::exact_version::exact_python_request(&self.version)?;
            let pin = crate::libraries::project::runtime_pin::observed_runtime_pin(
                &self.observed,
                &request,
            )?;
            crate::plugins::core::python::PythonPlugin::new()
                .install_shared_exact_in_context(&self.context, &self.version, config, pin.as_ref())
                .await?;
            install_project_packages(&self.context, &self.observed, &self.roots).await
        })
    }
}

fn project_plan(
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
    ensure!(
        !context.policy().locked || roots.is_empty(),
        "locked shared package installation is not supported yet; refusing to resolve again"
    );
    Ok(Some(PythonProjectInstall {
        context,
        observed,
        version,
        roots,
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
    use crate::config::FrozenConfigDiscovery;
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    let context = capture_install_context()?;
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![std::path::PathBuf::from("/")];
    let observed = observe_project(
        &std::env::current_dir()?,
        ReadPolicy::new(discovery, context.policy().deadline),
    )?;
    project_plan(context, observed, extra)
}

fn capture_install_context() -> Result<OperationContext> {
    use crate::config::{Settings, SettingsExt};
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(600);
    let settings = Settings::get();
    Ok(capture_operation_context(OperationContextInputs {
        settings: &settings,
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
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: crate::dirs::DATA.to_path_buf(),
        cache_root: crate::dirs::CACHE.to_path_buf(),
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
        if let Some(project) = prepare_current_project(extra)? {
            let declared = super::shared::exact_version::exact_python_request(project.version())?;
            if declared.version_only() == requested.version_only()
                && declared
                    .build_pin()
                    .is_none_or(|pin| Some(pin) == requested.build_pin())
            {
                let pin = crate::libraries::project::runtime_pin::observed_runtime_pin(
                    &project.observed,
                    &requested,
                )?;
                crate::plugins::core::python::PythonPlugin::new()
                    .install_shared_exact_in_context(
                        &project.context,
                        version,
                        config,
                        pin.as_ref(),
                    )
                    .await?;
                return install_project_packages(
                    &project.context,
                    &project.observed,
                    &project.roots,
                )
                .await;
            }
        }
        ensure!(
            extra.is_empty(),
            "explicit package bindings require the matching declared project Python"
        );
        let context = capture_install_context()?;
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
        commit_package_selection, prepare_package_selection_commit,
    };
    context.checkpoint()?;
    observed.recheck()?;
    let query = open_query_context(context, observed)?;
    let SelectionReport::Selected(candidate) = select_existing(&query)? else {
        eyre::bail!("the exact project Python is missing, ambiguous or damaged after installation");
    };
    if roots.is_empty() {
        let language = LanguageRepository::open_readonly(context)?
            .acquire_by_key(candidate.key())?
            .context("project Python is not Ready")?;
        check_requires_python(observed, &language)?;
        drop(language);
        super::install::selection::apply_directory_selection(context, observed, &candidate)?;
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
    let lock_path = project_lock_path(observed)?;
    let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(
        &lock_path,
        Default::default(),
        context.policy().deadline,
    )?;
    observed.recheck()?;
    let requested: Vec<_> = roots
        .iter()
        .map(|(name, version)| (name.as_str(), version.as_str()))
        .collect();
    let existing = ProjectRepository::open_readonly(context)?
        .read_views()
        .matching_package_graphs(candidate.key().fingerprint(), &requested)?;
    let mut ready = Vec::new();
    for graph in existing {
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
        drop(lease);
        commit_package_selection(prepare_package_selection_commit(
            context,
            observed,
            *candidate.key(),
            *graph.layout(),
            record,
        )?)?;
        root.recheck()?;
        return Ok(());
    }
    let language = LanguageRepository::open_readonly(context)?
        .acquire_by_key(candidate.key())?
        .context("project Python is not Ready")?;
    check_requires_python(observed, &language)?;
    resolve_and_commit(context, observed, language, &root, &requested).await
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
    root: &crate::packages::python_resolution::lock_session::BoundPythonLock,
    roots: &[(&str, &str)],
) -> Result<()> {
    use crate::packages::python_resolution::candidate_policy::{
        CandidatePolicy, Prereleases, Sdists, Yanked,
    };
    use crate::packages::python_resolution::source::registry_depot::{
        RegistryDepotContext, RegistryEndpoint, RegistryRootScope,
    };
    use crate::programs::shared::commit::{
        commit_package_selection, prepare_package_selection_commit,
    };
    use pakro_index::artifact::NetworkPolicy;
    use pakro_index::python_source::AuthScopeKey;
    use pakro_index::simple::{
        AuthOriginGrants, PythonSourceSession, SimpleFetchPolicy, SimpleHttpConfig,
        SimpleIndexClient, SourceFileSession,
    };
    observed.recheck()?;
    let cache = context.requested_cache_root().join("shared-python");
    let scratch = cache.join("scratch");
    let client = SimpleIndexClient::new(cache.join("simple"), SimpleHttpConfig::default())?;
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
            sdists: Sdists::Exclude,
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
        root,
        roots,
        registry_context,
    )
    .await?;
    let facts = graph.package_facts(&language)?;
    let instances: Vec<_> = facts.iter().map(|fact| fact.key).collect();
    let record = graph.projection(&instances)?;
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
    let prepared =
        prepare_package_selection_commit(context, observed, language_key, layout_key, record)?;
    evidence.recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
    commit_package_selection(prepared)?;
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
        .find(|(_, declarations)| declarations.tools.iter().any(|tool| tool.name == "python"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrozenConfigDiscovery;
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    use std::time::{Duration, Instant};

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
}

#[cfg(test)]
#[path = "python/integration_tests.rs"]
mod integration_tests;
