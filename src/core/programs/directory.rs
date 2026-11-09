//! Typed local Python queries and the separate controlled directory application.
//! Queries grant no lease, fetch nothing, and never register a project.
use crate::config::Settings;
use crate::dirs::{CACHE, DATA};
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::libraries::project::{identity::ProjectIdentity, state::ExpectedProjectState};
use crate::programs::capture_operation_context;
use crate::programs::shared::context::{OperationContext, OperationContextInputs};
use crate::programs::shared::policy::{AllowedWrites, OperationPolicy, RequestBudget};
use eyre::{Result, ensure};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const DIRECTORY_SELECTION_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct DirectoryPython {
    pub root: PathBuf,
    pub instance: String,
    pub bin_dir: PathBuf,
    pub bin_dirs: Vec<PathBuf>,
    pub executable: PathBuf,
    pub version: String,
    pub(crate) activation: Option<(crate::packages::conda_scripts::activation::ActivationPlan, PathBuf)>,
}
impl DirectoryPython {
    pub fn activation_changes(&self, original: &std::collections::BTreeMap<String, String>) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        self.activation.as_ref().map(|(plan, prefix)| plan.bind(prefix, original)).transpose().map(Option::unwrap_or_default)
    }
    /// Entry points come only from the verified language or package layout.
    /// This is a path description, not a future process lease.
    pub fn command(&self, command: &str) -> Result<Option<PathBuf>> {
        ensure!(
            !command.is_empty()
                && Path::new(command).file_name() == Some(std::ffi::OsStr::new(command))
                && !command.contains(['/', '\\']),
            "directory command selection requires a command basename"
        );
        for bin in &self.bin_dirs {
            let executable = bin.join(command);
            match std::fs::metadata(&executable) {
                Ok(metadata) => {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                        return Ok(Some(executable));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonQueryScope {
    Language,
    Environment,
}
#[derive(Debug, Clone)]
pub struct PythonQueryRequest {
    /// A complete exact version or numeric local prefix. None uses the declaration.
    pub target: Option<String>,
    pub scope: PythonQueryScope,
}
#[derive(Debug, Clone)]
pub enum PythonQueryReport {
    NoDeclaration,
    Ready(DirectoryPython),
    MissingLanguage {
        requested: String,
    },
    MissingPackages {
        requested: String,
        packages: Vec<String>,
    },
    IncompatibleTarget {
        requested: String,
        detail: String,
    },
    InvalidDeclaration {
        requested: String,
    },
    Ambiguous {
        requested: String,
        candidates: Vec<String>,
    },
    Corrupt {
        detail: String,
    },
}
impl PythonQueryReport {
    pub fn into_ready(self) -> Result<Option<DirectoryPython>> {
        match self {
            Self::NoDeclaration => Ok(None),
            Self::Ready(selected) => Ok(Some(selected)),
            other => eyre::bail!("{other}"),
        }
    }
}
impl std::fmt::Display for PythonQueryReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDeclaration => write!(f, "no Python declaration in the current directory"),
            Self::Ready(selected) => write!(f, "Python {} is ready", selected.version),
            Self::MissingLanguage { requested } => write!(
                f,
                "Python {requested} is not installed in the shared library; install it explicitly"
            ),
            Self::MissingPackages {
                requested,
                packages,
            } => write!(
                f,
                "Python {requested} exists, but shared packages are missing: {}; install them explicitly",
                packages.join(", ")
            ),
            Self::IncompatibleTarget { requested, detail } => write!(
                f,
                "Python {requested} is incompatible with the current package binding: {detail}"
            ),
            Self::InvalidDeclaration { requested } => write!(
                f,
                "invalid Python request {requested:?}; project declarations need a complete exact version; explicit local queries also accept numeric prefixes"
            ),
            Self::Ambiguous {
                requested,
                candidates,
            } => write!(
                f,
                "multiple shared Python instances or layouts match {requested}: {}; refusing to choose",
                candidates.join(", ")
            ),
            Self::Corrupt { detail } => write!(f, "shared Python library is damaged: {detail}"),
        }
    }
}

pub fn is_exact_python_version(raw: &str) -> bool {
    crate::programs::shared::exact_version::exact_python_request(raw).is_ok()
}
pub fn query_python(request: PythonQueryRequest) -> Result<PythonQueryReport> {
    let context = query_context()?;
    let observed = crate::programs::library::observe_current_project(&context)?;
    Ok(crate::programs::query::environment::query_observed(&context, &observed, &request)?.report)
}
/// Apply only the declared current environment. The read and controlled commit
/// share their original observation and deadline; query guards are released
/// before project → objects → catalog protection is re-acquired.
pub fn apply_directory_python() -> Result<PythonQueryReport> {
    let context = capture_context(
        query_deadline()?,
        AllowedWrites {
            libraries: false,
            project_selection: true,
            lockfile: false,
            declarations: false,
        },
    )?;
    let observed = crate::programs::library::observe_current_project(&context)?;
    apply_observed(&context, &observed)
}
pub(crate) fn apply_observed(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
) -> Result<PythonQueryReport> {
    let identity = ProjectIdentity::from_observation(observed)?;
    let state = ExpectedProjectState::capture(context, &identity)?;
    let selection = crate::programs::query::environment::query_observed(
        context,
        observed,
        &PythonQueryRequest {
            target: None,
            scope: PythonQueryScope::Environment,
        },
    )?;
    apply_selection(context, observed, selection, &state)
}
fn apply_selection(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    selection: crate::programs::query::environment::ObservedSelection,
    state: &ExpectedProjectState,
) -> Result<PythonQueryReport> {
    // Even an unchanged association must still describe the original live
    // observation; the query result is not authority to skip currentness.
    context.checkpoint()?;
    observed.recheck()?;
    if matches!(&selection.report, PythonQueryReport::Ready(_)) && !selection.already_applied {
        let candidate = selection
            .candidate
            .as_ref()
            .expect("ready query has a language");
        if let Some(graph) = selection.graph {
            let record = crate::packages::python_environment::model::PackageGraphRecord {
                roots: graph.roots().to_vec(),
                nodes: graph.nodes().to_vec(),
                edges: graph.edges().to_vec(),
            };
            crate::programs::shared::commit::commit_package_selection(
                crate::programs::shared::commit::prepare_package_selection_commit_with_state(
                    context,
                    observed,
                    *candidate.key(),
                    *graph.layout(),
                    record,
                    state,
                )?,
            )?;
        } else {
            crate::programs::install::selection::apply_directory_selection_with_state(
                context, observed, candidate, state,
            )?;
        }
    }
    Ok(selection.report)
}
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryCommandSelection {
    NotPython,
    Available(PathBuf),
    Missing,
}
pub fn select_directory_command(command: &str) -> Result<DirectoryCommandSelection> {
    let context = query_context()?;
    let observed = crate::programs::library::observe_current_project(&context)?;
    select_observed_command(&context, &observed, command)
}
fn python_command(command: &str) -> bool {
    ["python", "pip"].iter().any(|prefix| {
        command.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.is_empty()
                || (!suffix.starts_with('.')
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.'))
        })
    })
}
fn select_observed_command(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    command: &str,
) -> Result<DirectoryCommandSelection> {
    ensure!(
        !command.is_empty()
            && Path::new(command).file_name() == Some(std::ffi::OsStr::new(command))
            && !command.contains(['/', '\\']),
        "directory command selection requires a command basename"
    );
    let family = python_command(command);
    let report = crate::programs::query::environment::query_observed(
        context,
        observed,
        &PythonQueryRequest {
            target: None,
            scope: PythonQueryScope::Environment,
        },
    )?
    .report;
    let Some(selected) = report.into_ready()? else {
        return Ok(if family {
            DirectoryCommandSelection::Missing
        } else {
            DirectoryCommandSelection::NotPython
        });
    };
    if let Some(executable) = selected.command(command)? {
        return Ok(DirectoryCommandSelection::Available(executable));
    }
    Ok(if family {
        DirectoryCommandSelection::Missing
    } else {
        DirectoryCommandSelection::NotPython
    })
}
fn query_deadline() -> Result<Instant> {
    let deadline = Instant::now() + DIRECTORY_SELECTION_TIMEOUT;
    let loaded = crate::config::Config::maybe_get();
    let deadline = loaded
        .as_ref()
        .and_then(|config| config.readonly_query_deadline())
        .map_or(deadline, |original| original.min(deadline));
    if let Some(config) = loaded {
        config.recheck_readonly_inputs(deadline)?;
    }
    Ok(deadline)
}
pub(crate) fn query_context() -> Result<OperationContext> {
    capture_context(
        query_deadline()?,
        AllowedWrites {
            libraries: false,
            project_selection: false,
            lockfile: false,
            declarations: false,
        },
    )
}
fn capture_context(deadline: Instant, writes: AllowedWrites) -> Result<OperationContext> {
    Ok(capture_operation_context(OperationContextInputs {
        settings: &Settings::get(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline,
            budget: RequestBudget {
                max_arguments: 32,
                max_total_argument_bytes: 16384,
                max_declarations: 4096,
                max_runtime_candidates: 4096,
            },
            writes,
        },
        cancellation: CancellationToken::new(),
        data_root: DATA.to_path_buf(),
        cache_root: CACHE.to_path_buf(),
        python_sources: Some(crate::programs::python::public_sources()?),
        python_target_description: None,
    })?)
}
#[cfg(test)]
#[path = "directory/tests.rs"]
mod tests;
