//! Public reports for local library queries and explicitly targeted removal.
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
    crate::programs::query::inventory::inventory(&context(false)?)
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
    crate::programs::query::projects::projects(&context(false)?)
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentPython {
    pub requested: String,
    pub status: String,
    pub instance: Option<String>,
    pub path: Option<PathBuf>,
    pub detail: Option<String>,
}

fn observe_current_project(
    context: &crate::programs::OperationContext,
) -> Result<crate::libraries::project::snapshot::ObservedProjectInputs> {
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    let mut discovery = crate::config::FrozenConfigDiscovery::new(vec![
        "pakro.toml".into(),
        "pakro.local.toml".into(),
    ]);
    discovery.ceiling_paths = vec![PathBuf::from("/")];
    Ok(observe_project(
        &std::env::current_dir()?,
        ReadPolicy::new(discovery, context.policy().deadline),
    )?)
}

pub fn current_python() -> Result<Option<CurrentPython>> {
    let context = context(false)?;
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
    let context = context(false)?;
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
        || observed
            .declarations()
            .any(|(_, declarations)| declarations.tools.iter().any(|tool| tool.name != "python"));
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
    use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
    let query = open_query_context(context, observed)?;
    let (requested, status, instance, path, detail) = match select_existing(&query)? {
        SelectionReport::NoLanguageRequest => return Ok(None),
        SelectionReport::Selected(candidate) => {
            use eyre::ContextCompat;
            let lease =
                crate::libraries::language::repository::LanguageRepository::open_readonly(context)?
                    .acquire_by_key(candidate.key())?
                    .context("current Python publication disappeared during query")?;
            lease.check_binding()?;
            (
                candidate.version().to_owned(),
                "ready",
                Some(candidate.key().filename()),
                Some(candidate.root().to_owned()),
                None,
            )
        }
        SelectionReport::Missing { requested } => {
            (requested.as_str().to_owned(), "missing", None, None, None)
        }
        SelectionReport::InvalidDeclaration { requested } => (
            requested,
            "invalid",
            None,
            None,
            Some("write a complete exact Python version".to_owned()),
        ),
        SelectionReport::Ambiguous {
            requested,
            candidates,
        } => (
            requested.as_str().to_owned(),
            "ambiguous",
            None,
            None,
            Some(
                candidates
                    .iter()
                    .map(|candidate| candidate.key().filename())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        SelectionReport::Corrupt { detail } => (String::new(), "corrupt", None, None, Some(detail)),
    };
    Ok(Some(CurrentPython {
        requested,
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
    tool.short != "python"
        && (tool.has_explicit_backend()
            || crate::registry::REGISTRY.contains_key(tool.short.as_str()))
}
