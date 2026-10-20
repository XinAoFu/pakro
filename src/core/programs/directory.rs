//! Minimal public directory-selection entry for the CLI binary.
//!
//! Entering a directory selects the exact Python already published in the
//! shared language library (D07). Missing items are only reported (D08);
//! declarations must be complete exact versions (D10). The commit mode is
//! used by the directory hook and performs the controlled registration;
//! every other caller stays read-only and never registers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eyre::{ContextCompat, Result, ensure};
use tokio_util::sync::CancellationToken;

use crate::config::{FrozenConfigDiscovery, Settings};
use crate::dirs::{CACHE, DATA};
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::capture_operation_context;
use crate::programs::install::selection::apply_directory_selection;
use crate::programs::query::paths::selection_paths;
use crate::programs::query::snapshot::{
    QueryError, SelectionReport, open_query_context, select_existing,
};
use crate::programs::shared::context::{OperationContext, OperationContextInputs};
use crate::programs::shared::policy::{AllowedWrites, OperationPolicy, RequestBudget};

/// The selected shared interpreter of the current directory, described by its
/// real payload facts. Carrying this value grants no lease.
pub struct DirectoryPython {
    pub bin_dir: PathBuf,
    pub executable: PathBuf,
    pub version: String,
}

/// Whether a raw version string is a complete exact Python declaration
/// (D10). Exposed for the CLI's install fast path, which must reject
/// incomplete requests before any remote resolution or write.
pub fn is_exact_python_version(raw: &str) -> bool {
    crate::programs::shared::exact_version::exact_python_request(raw).is_ok()
}

/// Selects the current directory's declared Python from the shared library.
/// `commit` records the project registration and selection through the
/// controlled apply entry; without it this call never writes anything.
/// Failures of the selection itself degrade to `None` with a warning so the
/// shell hook stays fast and non-fatal.
pub fn select_directory_python(commit: bool) -> Result<Option<DirectoryPython>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let observed = match observe_current(deadline) {
        Ok(observed) => observed,
        Err(error) => {
            warn!("pakro: could not observe the current directory's configuration: {error:#}");
            return Ok(None);
        }
    };
    let context = capture_context(deadline, commit)?;
    match select_observed(&context, &observed, commit) {
        Ok(selection) => Ok(selection),
        Err(error) => {
            warn!("pakro: could not select the shared Python environment: {error:#}");
            Ok(None)
        }
    }
}

fn observe_current(
    deadline: Instant,
) -> Result<crate::libraries::project::snapshot::ObservedProjectInputs> {
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![PathBuf::from("/")];
    Ok(observe_project(
        &std::env::current_dir()?,
        ReadPolicy::new(discovery, deadline),
    )?)
}

/// A missing-command event may use an existing guarded selection, but it is
/// never authorization to obtain Python or its declared packages.
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryCommandSelection {
    NotPython,
    Available(PathBuf),
    Missing,
}

pub fn select_directory_command(command: &str) -> Result<DirectoryCommandSelection> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let observed = observe_current(deadline)?;
    select_observed_command(&capture_context(deadline, false)?, &observed, command)
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
    observed: &crate::libraries::project::snapshot::ObservedProjectInputs,
    command: &str,
) -> Result<DirectoryCommandSelection> {
    ensure!(
        !command.is_empty()
            && std::path::Path::new(command).file_name() == Some(std::ffi::OsStr::new(command))
            && !command.contains(['/', '\\']),
        "directory command selection requires a command basename"
    );
    let family = python_command(command);
    let declared =
        crate::programs::query::snapshot::effective_python_declaration(observed).is_some();
    if !declared {
        return Ok(if family {
            DirectoryCommandSelection::Missing
        } else {
            DirectoryCommandSelection::NotPython
        });
    }
    let Some(selected) = select_observed(context, observed, false)? else {
        return Ok(DirectoryCommandSelection::Missing);
    };
    let executable = selected.bin_dir.join(command);
    match std::fs::metadata(&executable) {
        Ok(metadata) => {
            use std::os::unix::fs::PermissionsExt;
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return Ok(DirectoryCommandSelection::Available(executable));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(if family {
        DirectoryCommandSelection::Missing
    } else {
        DirectoryCommandSelection::NotPython
    })
}

pub(crate) fn select_observed(
    context: &OperationContext,
    observed: &crate::libraries::project::snapshot::ObservedProjectInputs,
    commit: bool,
) -> Result<Option<DirectoryPython>> {
    let query = match open_query_context(&context, &observed) {
        Ok(query) => query,
        // A stale observation between the two calls converges on the next
        // prompt; cancellation is silent by design.
        Err(_) => return Ok(None),
    };
    let candidate = match select_existing(&query) {
        Ok(SelectionReport::Selected(candidate)) => candidate,
        Ok(SelectionReport::NoLanguageRequest) => return Ok(None),
        Ok(SelectionReport::InvalidDeclaration { requested }) => {
            warn!(
                "pakro: python declaration {requested:?} is not a complete exact version; \
                 write the full version, e.g. python = \"3.11.14\""
            );
            return Ok(None);
        }
        Ok(SelectionReport::Missing { requested }) => {
            warn!(
                "pakro: python {} is not installed in the shared library; \
                 install it explicitly if you want it",
                requested.as_str()
            );
            return Ok(None);
        }
        Ok(SelectionReport::Ambiguous { candidates, .. }) => {
            warn!(
                "pakro: {} shared python instances match this declaration; \
                 refusing to pick one",
                candidates.len()
            );
            return Ok(None);
        }
        Ok(SelectionReport::Corrupt { detail }) => {
            warn!("pakro: the shared language library is damaged: {detail}");
            return Ok(None);
        }
        Err(QueryError::Unreadable(error)) => {
            warn!("pakro: the shared language library is unreadable: {error}");
            return Ok(None);
        }
        Err(_) => return Ok(None),
    };
    let languages =
        crate::libraries::language::repository::LanguageRepository::open_readonly(context)?;
    let language = languages
        .acquire_by_key(candidate.key())?
        .context("selected Python is no longer Ready")?;
    language.check_binding()?;
    crate::programs::python::check_requires_python(observed, &language)?;
    let declared = crate::programs::query::snapshot::effective_python_declaration(observed)
        .context("selected Python has no observed declaration")?;
    let roots = crate::programs::python::project_roots(observed, &declared, &[])?;
    let mut repository = ProjectRepository::open_readonly(context)?;
    let identity = ProjectIdentity::from_observation(observed)?;
    let declaration = crate::programs::shared::commit::declaration_digest(observed);
    let paths = selection_paths(&candidate);
    let mut selected = DirectoryPython {
        bin_dir: paths.bin_dir().to_owned(),
        executable: paths.executable().to_owned(),
        version: candidate.version().to_owned(),
    };
    let mut chosen_graph = None;
    if !roots.is_empty() {
        let sources = context.python_sources()?;
        ensure!(
            sources.registries.len() == 1,
            "shared package selection requires one fixed registry source"
        );
        let allowed: Vec<_> = sources.registries.iter().map(|registry| (
            registry.id().as_str(), crate::packages::python_resolution::lock_model::canonical_registry_base_fingerprint(registry.canonical_base())
        )).collect();
        let requested: Vec<_> = roots
            .iter()
            .map(|(name, version)| (name.as_str(), version.as_str()))
            .collect();
        let graphs = repository
            .read_views()
            .matching_package_graphs(candidate.key().fingerprint(), &requested)?;
        let mut ready = Vec::new();
        for graph in graphs {
            if let Err(error) = graph.validate_observed_pins(observed, &allowed) {
                if error
                    .downcast_ref::<crate::libraries::project::package_graph::GraphPinMismatch>()
                    .is_some()
                {
                    continue;
                }
                return Err(error);
            }
            // A deliberately deleted adapter has no Ready manifest. Its old
            // catalog description is not a usable candidate; damage is Err.
            let Some(layout) =
                crate::libraries::package::lease::acquire_layout(context, *graph.layout())?
            else {
                continue;
            };
            crate::programs::query::snapshot::validate_layout_graph(&graph, &layout)?;
            crate::programs::python::check_requires_python(observed, layout.language_lease())?;
            ready.push((graph, layout.bin(), layout.bin().join("python")));
            // Query guards must end before a commit takes the project lock.
            drop(layout);
        }
        if ready.is_empty() {
            warn!(
                "pakro: missing shared Python packages: {}; install them explicitly to select this project",
                roots
                    .iter()
                    .map(|(name, version)| format!("{name}=={version}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(None);
        }
        if ready.len() > 1 {
            warn!(
                "pakro: multiple complete shared package layouts match: {}; refusing to choose an instance",
                ready
                    .iter()
                    .map(|(graph, _, _)| format!("layout-{}", hex::encode(graph.layout())))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(None);
        }
        let (graph, bin, executable) = ready.pop().expect("one Ready graph");
        selected.bin_dir = bin;
        selected.executable = executable;
        chosen_graph = Some(graph);
    }
    observed.recheck()?;
    repository.reload()?;
    let same_observation = repository
        .model()
        .project_registration(identity.as_uuid())
        .is_some_and(|registered| registered.declaration() == declaration);
    let current = repository.read_views().language_selection(&identity);
    let current_graph = repository.read_views().package_graph(&identity)?;
    let already = same_observation
        && current.as_ref().is_some_and(|selection| {
            selection.language() == &candidate.key().fingerprint()
                && match &chosen_graph {
                    Some(graph) => current_graph
                        .as_ref()
                        .is_some_and(|current| current.layout() == graph.layout()),
                    None => {
                        selection.generation_allocation().is_none()
                            && selection.package_references().is_empty()
                    }
                }
        });
    // Never invert project -> actual objects -> catalog: all query SH guards
    // are released before the controlled commit reacquires those protections.
    drop(language);
    drop(repository);
    if commit && !already {
        if let Some(graph) = chosen_graph {
            let record = crate::packages::python_environment::model::PackageGraphRecord {
                roots: graph.roots().to_vec(),
                nodes: graph.nodes().to_vec(),
                edges: graph.edges().to_vec(),
            };
            crate::programs::shared::commit::commit_package_selection(
                crate::programs::shared::commit::prepare_package_selection_commit(
                    context,
                    observed,
                    *candidate.key(),
                    *graph.layout(),
                    record,
                )?,
            )?;
        } else {
            apply_directory_selection(context, observed, &candidate)?;
        }
    }
    Ok(Some(selected))
}

fn capture_context(deadline: Instant, commit: bool) -> Result<OperationContext> {
    Ok(capture_operation_context(OperationContextInputs {
        settings: &Settings::get(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline,
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: false,
                project_selection: commit,
                lockfile: false,
                declarations: false,
            },
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
