//! Minimal public directory-selection entry for the CLI binary.
//!
//! Entering a directory selects the exact Python already published in the
//! shared language library (D07). Missing items are only reported (D08);
//! declarations must be complete exact versions (D10). The commit mode is
//! used by the directory hook and performs the controlled registration;
//! every other caller stays read-only and never registers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eyre::Result;
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

/// Selects the current directory's declared Python from the shared library.
/// `commit` records the project registration and selection through the
/// controlled apply entry; without it this call never writes anything.
/// Failures of the selection itself degrade to `None` with a warning so the
/// shell hook stays fast and non-fatal.
pub fn select_directory_python(commit: bool) -> Result<Option<DirectoryPython>> {
    let cwd = std::env::current_dir()?;
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".to_owned(), "pakro.local.toml".to_owned()]);
    discovery.ceiling_paths = vec![PathBuf::from("/")];
    let deadline = Instant::now() + Duration::from_secs(5);
    let observed = match observe_project(&cwd, ReadPolicy::new(discovery, deadline)) {
        Ok(observed) => observed,
        Err(error) => {
            warn!("pakro: could not observe the current directory's configuration: {error}");
            return Ok(None);
        }
    };
    let context = capture_context(deadline, commit)?;
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
    let paths = selection_paths(&candidate);
    if commit {
        // Idempotent entry: an already-committed identical reference must not
        // spawn a new transaction on every prompt.
        let already = (|| -> Result<bool> {
            let identity = ProjectIdentity::from_observation(&observed)?;
            Ok(ProjectRepository::open_readonly(&context)
                .ok()
                .and_then(|repository| repository.read_views().language_selection(&identity))
                .is_some_and(|selection| selection.language() == &candidate.key().fingerprint()))
        })();
        match already {
            Ok(false) => {
                if let Err(error) = apply_directory_selection(&context, &observed, &candidate) {
                    warn!("pakro: failed to record the python selection: {error:#}");
                    return Ok(None);
                }
            }
            Ok(true) => {}
            Err(error) => {
                warn!("pakro: failed to verify the recorded python selection: {error:#}");
                return Ok(None);
            }
        }
    }
    Ok(Some(DirectoryPython {
        bin_dir: paths.bin_dir().to_path_buf(),
        executable: paths.executable().to_path_buf(),
        version: candidate.version().to_string(),
    }))
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
        python_sources: None,
        python_target_description: None,
    })?)
}
