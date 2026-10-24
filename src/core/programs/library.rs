//! Public reports for local queries, targeted removal and approved recovery.
use eyre::Result;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct LibraryEntry {
    pub kind: String,
    pub instance: String,
    pub name: String,
    pub version: String,
    pub build: Option<String>,
    pub platform: String,
    pub path: PathBuf,
    pub users: Vec<String>,
    pub language: Option<String>,
    pub dependencies: Vec<String>,
}

pub fn inventory() -> Result<Vec<LibraryEntry>> {
    crate::programs::query::inventory::inventory(&crate::programs::directory::query_context()?)
}

/// This exception is bound to the exact PBS artifact and instance approved
/// for the four-file ISAT recovery. It is not a general repair capability.
#[derive(Debug, Serialize)]
pub struct BytecodeRecoveryReport {
    pub instance: String,
    pub paths: Vec<PathBuf>,
    pub unchanged_entries: usize,
    pub restored: bool,
    pub already_healthy: bool,
    pub resumed: bool,
}

pub fn restore_python_bytecode(
    version: &str,
    instance: &str,
    dry_run: bool,
) -> Result<BytecodeRecoveryReport> {
    use crate::libraries::language::repository::LanguageRepository;
    eyre::ensure!(
        version == "3.11.17+20261013",
        "bytecode recovery supports only the approved Python 3.11.17+20261013"
    );
    eyre::ensure!(
        instance == "language-670a04e9cf8e3636a5fd4a9fcfcd15e2d2153f1b1acb7be6e0ebd66e9018244e",
        "bytecode recovery requires the exact approved shared Python instance"
    );
    let context = context(!dry_run)?;
    let repository = LanguageRepository::open_readonly(&context)?;
    let target = repository
        .enumerate_published()
        .map_err(|error| eyre::eyre!("cannot inspect the recovery target: {error:?}"))?
        .into_iter()
        .find(|target| target.key().filename() == instance)
        .ok_or_else(|| eyre::eyre!("the approved Python instance is not published locally"))?;
    eyre::ensure!(
        target.version() == "3.11.17"
            && target.build() == Some("20261013")
            && target.matches_host_platform()
            && target.spec().plan().source().filename()
                == "cpython-3.11.17+20261013-aarch64-apple-darwin-install_only.tar.gz"
            && target.spec().plan().source().digest().to_string()
                == "sha256-d81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3",
        "the local runtime does not match the approved bytecode recovery artifact"
    );
    let report =
        crate::libraries::language::recovery::restore_bytecode(&context, &target, dry_run)?;
    Ok(BytecodeRecoveryReport {
        instance: report.instance,
        paths: report.paths,
        unchanged_entries: report.unchanged_entries,
        restored: report.restored,
        already_healthy: report.already_healthy,
        resumed: report.resumed,
    })
}

fn context(write: bool) -> Result<crate::programs::OperationContext> {
    use crate::programs::*;
    Ok(capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::get(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            budget: RequestBudget {
                max_arguments: 32,
                max_total_argument_bytes: 16384,
                max_declarations: 128,
                max_runtime_candidates: 4096,
            },
            writes: AllowedWrites {
                libraries: write,
                project_selection: false,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: crate::dirs::DATA.to_path_buf(),
        cache_root: crate::dirs::CACHE.to_path_buf(),
        python_sources: None,
        python_target_description: None,
    })?)
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectEntry {
    pub identity: String,
    pub path: PathBuf,
    pub accessible: bool,
    pub revision: u64,
    pub language: Option<String>,
    pub packages: Vec<String>,
}

pub fn projects() -> Result<Vec<ProjectEntry>> {
    crate::programs::query::projects::projects(&crate::programs::directory::query_context()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentPython {
    pub requested: String,
    /// Interpreter version of the verified language, without an implicit build suffix.
    pub resolved_version: Option<String>,
    pub status: String,
    pub instance: Option<String>,
    pub path: Option<PathBuf>,
    pub detail: Option<String>,
}

pub(crate) fn observe_current_project(
    context: &crate::programs::OperationContext,
) -> Result<crate::libraries::project::snapshot::ObservedProjectInputs> {
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    let loaded = crate::config::Config::maybe_get();
    if let Some(config) = &loaded {
        config.recheck_readonly_inputs(context.policy().deadline)?;
    }
    let discovery = crate::config::FrozenConfigDiscovery::from_process();
    let mut policy = ReadPolicy::new(discovery, context.policy().deadline);
    policy.cancellation = context.cancellation_token();
    policy.environment = crate::env::vars_safe()
        .filter(|(key, _)| crate::toolset::tool_from_env_var_name(key).is_some())
        .collect();
    let observed = observe_project(&std::env::current_dir()?, policy)?;
    if let Some(config) = &loaded {
        config.recheck_readonly_inputs(context.policy().deadline)?;
    }
    Ok(observed)
}

pub fn current_python() -> Result<Option<CurrentPython>> {
    let context = crate::programs::directory::query_context()?;
    let observed = observe_current_project(&context)?;
    current_observed(&context, &observed)
}

/// One readonly observation for the shared Python CLI views. Legacy views are
/// needed only when there are actual non-Python declarations or installations.
pub struct LocalProjectReport {
    pub root: PathBuf,
    pub packages: Vec<crate::packages::PackageInstallRequest>,
    pub current: Option<CurrentPython>,
    pub registered: Option<ProjectEntry>,
    pub has_legacy_tools: bool,
    pub legacy_installs_present: bool,
}

pub fn local_project() -> Result<LocalProjectReport> {
    let context = crate::programs::directory::query_context()?;
    let observed = observe_current_project(&context)?;
    let root = observed.root().to_owned();
    let packages: Vec<_> = observed
        .effective_packages()?
        .into_iter()
        .map(|owned| owned.request)
        .collect();
    let has_legacy_tools = packages
        .iter()
        .any(|request| request.runtime_name != "python")
        || observed.declarations().any(|(_, declarations)| {
            declarations.tools.iter().any(|tool| {
                !crate::programs::query::snapshot::python_tool_name(&observed, &tool.name)
            })
        });
    let mut legacy_installs_present = false;
    match std::fs::read_dir(*crate::dirs::INSTALLS) {
        Ok(entries) => {
            for (index, entry) in entries.enumerate() {
                context.checkpoint()?;
                eyre::ensure!(index < 4096, "legacy installation listing budget exceeded");
                let name = entry?.file_name();
                if name != "python" && !name.as_encoded_bytes().starts_with(b".") {
                    legacy_installs_present = true;
                    break;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let current = current_observed(&context, &observed)?;
    let registered = crate::programs::query::projects::projects(&context)?
        .into_iter()
        .find(|project| project.path == root);
    observed.recheck()?;
    Ok(LocalProjectReport {
        root,
        packages,
        current,
        registered,
        has_legacy_tools,
        legacy_installs_present,
    })
}

pub(crate) fn current_observed(
    context: &crate::programs::OperationContext,
    observed: &crate::libraries::project::snapshot::ObservedProjectInputs,
) -> Result<Option<CurrentPython>> {
    use crate::programs::directory::{PythonQueryReport, PythonQueryRequest, PythonQueryScope};
    let selection = crate::programs::query::environment::query_observed(
        context,
        observed,
        &PythonQueryRequest {
            target: None,
            scope: PythonQueryScope::Environment,
        },
    )?;
    let language = selection.candidate.as_ref();
    let instance = language.map(|candidate| candidate.key().filename());
    let resolved_version = language.map(|candidate| candidate.version().to_owned());
    let path = language.map(|candidate| candidate.root().to_owned());
    let (requested, status, detail) = match selection.report {
        PythonQueryReport::NoDeclaration => return Ok(None),
        PythonQueryReport::Ready(selected) => (selected.version, "ready", None),
        PythonQueryReport::MissingLanguage { requested } => (requested, "missing", None),
        PythonQueryReport::MissingPackages {
            requested,
            packages,
        } => (
            requested,
            "missing-packages",
            Some(format!("missing shared packages: {}", packages.join(", "))),
        ),
        PythonQueryReport::IncompatibleTarget { requested, detail } => {
            (requested, "incompatible", Some(detail))
        }
        PythonQueryReport::InvalidDeclaration { requested } => (
            requested,
            "invalid",
            Some("write a complete exact Python version".to_owned()),
        ),
        PythonQueryReport::Ambiguous {
            requested,
            candidates,
        } => (requested, "ambiguous", Some(candidates.join(", "))),
        PythonQueryReport::Corrupt { detail } => (String::new(), "corrupt", Some(detail)),
    };
    let requested = crate::programs::query::snapshot::effective_python_declaration(observed)
        .unwrap_or(requested);
    Ok(Some(CurrentPython {
        requested,
        resolved_version,
        status: status.into(),
        instance,
        path,
        detail,
    }))
}

/// Removes only the explicitly selected exact Python instance. The report is
/// descriptive; internal removal authority never crosses this facade.
pub fn uninstall_python(version: &str, instance: Option<&str>, dry_run: bool) -> Result<String> {
    crate::programs::uninstall::reclaim::language(&context(!dry_run)?, version, instance, dry_run)
}

pub fn uninstall_package(
    request: &crate::packages::PackageInstallRequest,
    instance: Option<&str>,
    dry_run: bool,
) -> Result<String> {
    crate::programs::uninstall::reclaim::package(&context(!dry_run)?, request, instance, dry_run)
}

/// Pure CLI classification without exposing the registry or backend owners.
pub fn is_known_legacy_tool(tool: &crate::args::BackendArg) -> bool {
    !crate::programs::python::is_python_backend(tool)
        && (tool.has_explicit_backend()
            || crate::toolset::install_state::get_plugin_type(&tool.short).is_some()
            || crate::registry::REGISTRY.contains_key(tool.short.as_str()))
}
