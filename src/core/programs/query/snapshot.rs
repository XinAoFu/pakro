//! One same-observation read context for local selection queries.
//!
//! The context consumes a frozen operation context and one real project
//! observation. Selection results are descriptions only: they grant no write,
//! registration or lease authority.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::libraries::language::identity::LanguageKey;
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::programs::shared::context::OperationContext;

#[cfg(test)]
#[path = "snapshot/lock_tests.rs"]
mod lock_tests;

#[derive(Debug, Error)]
pub(crate) enum QueryError {
    #[error(transparent)]
    Operation(#[from] crate::programs::shared::errors::OperationError),
    #[error(transparent)]
    Observation(#[from] crate::libraries::project::snapshot::ProjectObservationError),
    #[error("language library state is unreadable: {0}")]
    Unreadable(String),
    #[error(
        "shared Python local queries do not support runtime options for {0}; select an installed version or build explicitly"
    )]
    UnsupportedBackendOptions(String),
}

/// One published local instance, described from its real manifest and payload
/// facts. Carrying this value is not a lease and grants no protection.
#[derive(Debug, Clone)]
pub(crate) struct LanguageCandidate {
    key: LanguageKey,
    root: PathBuf,
    entrypoint: PathBuf,
    version: String,
    exact_version: String,
}

impl LanguageCandidate {
    pub(crate) fn new(
        key: LanguageKey,
        root: PathBuf,
        entrypoint: PathBuf,
        version: String,
    ) -> Self {
        Self {
            key,
            root,
            entrypoint,
            exact_version: version.clone(),
            version,
        }
    }
    pub(crate) fn key(&self) -> &LanguageKey {
        &self.key
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }
    pub(crate) fn exact_version(&self) -> &str {
        &self.exact_version
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// The complete factual outcome of a local selection. Non-selected variants
/// keep missing, ambiguous, invalid and damaged states distinct; a damaged
/// stored fact is never reported as an uninstalled version.
#[derive(Debug, Clone)]
pub(crate) enum SelectionReport {
    Selected(LanguageCandidate),
    NoLanguageRequest,
    InvalidDeclaration {
        requested: String,
    },
    Missing {
        requested: String,
    },
    Ambiguous {
        requested: String,
        candidates: Vec<LanguageCandidate>,
    },
    Corrupt {
        detail: String,
    },
}

pub(crate) struct QueryContext<'a> {
    context: OperationContext,
    observed: &'a ObservedProjectInputs,
}

/// Holds one real observation alongside the frozen context. The same
/// observation must still describe the project when the selection is applied.
pub(crate) fn open_query_context<'a>(
    context: &OperationContext,
    observed: &'a ObservedProjectInputs,
) -> Result<QueryContext<'a>, QueryError> {
    context.checkpoint()?;
    observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(context, observed)
        .map_err(|error| QueryError::Unreadable(format!("{error:#}")))?;
    Ok(QueryContext {
        context: context.clone(),
        observed,
    })
}

/// Matches the complete exact declaration against published local instances.
/// Only real manifests of the shared language library are considered; nothing
/// is acquired and no remote source is contacted for a missing item.
pub(crate) fn select_existing(query: &QueryContext<'_>) -> Result<SelectionReport, QueryError> {
    select_target(query, None)
}

/// Decode only bytes held by the readonly project observation. Explicit
/// queries for a different runtime retain their independent inventory scope.
pub(crate) fn project_lock_for_selected_query(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    target: Option<&str>,
    language: &crate::libraries::language::lease::LanguageLease,
) -> eyre::Result<Option<crate::packages::python_resolution::lock_model::PythonLockModel>> {
    let mut native_binding = None;
    if target.is_some() {
        let Some(declared) = effective_python_declaration(observed) else {
            return Ok(None);
        };
        let declared = crate::programs::shared::exact_version::exact_python_request(&declared)?;
        let selected = language.lock_version()?;
        let selected = crate::programs::shared::exact_version::exact_python_request(&selected)?;
        if declared.version_only() != selected.version_only()
            || declared
                .build_pin()
                .is_some_and(|build| Some(build) != selected.build_pin())
        {
            return Ok(None);
        }
        // Equivalent upstream/build spellings must retain the same lock.
        // A genuinely different selected source remains an independent query.
        if let Some(pin) =
            crate::libraries::project::runtime_pin::observed_runtime_pin(observed, &declared)?
        {
            if !language.matches_source_pin(declared.as_str(), &pin, context.settings())? {
                return Ok(None);
            }
            native_binding = Some((declared, pin));
        }
    }
    let model = read_project_lock_model(observed)?;
    if let Some(model) = &model {
        if target.is_some() {
            let selected = language.key().fingerprint();
            let same_runtime = model.graphs().iter().any(|graph| {
                graph.key().config_scope().is_empty()
                    && graph.key().group() == "shared-runtime"
                    && *graph.runtime().content_tree() == selected
            });
            // Native source pins do not identify the extraction recipe. A
            // different, provably legal recipe is an independent instance;
            // unknown keys and missing targets stay on the strict path.
            if !same_runtime
                && let Some(graph) =
                    model.graph(&crate::programs::install::lock::group_key(language)?)
                && let Some((declared, pin)) = &native_binding
            {
                let legal = [false, true]
                    .into_iter()
                    .map(|patch| {
                        crate::plugins::core::python::pinned_shared_language_spec(
                            declared.as_str(),
                            pin,
                            patch,
                            context.settings(),
                        )
                        .map(|spec| spec.key().fingerprint())
                    })
                    .collect::<eyre::Result<std::collections::BTreeSet<_>>>();
                if legal.is_ok_and(|keys| keys.contains(graph.runtime().content_tree())) {
                    return Ok(None);
                }
            }
        }
        crate::programs::install::lock_document::validate_declarations(observed, model)?;
    }
    Ok(model)
}

fn read_project_lock_model(
    observed: &ObservedProjectInputs,
) -> eyre::Result<Option<crate::packages::python_resolution::lock_model::PythonLockModel>> {
    observed.recheck()?;
    let mut model = None;
    for path in observed.paths().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "lock")
    }) {
        let Some(candidate) = crate::packages::python_resolution::lock_session::decode_root(
            observed
                .document(path)
                .ok_or_else(|| eyre::eyre!("observed lock bytes are missing"))?,
            Default::default(),
        )?
        else {
            continue;
        };
        eyre::ensure!(
            model.is_none(),
            "strict Python queries require one applicable exact lock document"
        );
        model = Some(candidate);
    }
    observed.recheck()?;
    Ok(model)
}

/// An explicit prefix is a local inventory filter only. Project declarations
/// still require a complete exact version and never acquire a latest release.
pub(crate) fn select_target(
    query: &QueryContext<'_>,
    target: Option<&str>,
) -> Result<SelectionReport, QueryError> {
    let report = select_target_inner(query, target)?;
    query.observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(
        &query.context,
        query.observed,
    )
    .map_err(|error| QueryError::Unreadable(format!("{error:#}")))?;
    Ok(report)
}

fn select_target_inner(
    query: &QueryContext<'_>,
    target: Option<&str>,
) -> Result<SelectionReport, QueryError> {
    query.context.checkpoint()?;
    query.observed.recheck()?;
    crate::programs::shared::lock_transaction::ensure_project_consistent(
        &query.context,
        query.observed,
    )
    .map_err(|error| QueryError::Unreadable(format!("{error:#}")))?;
    if target.is_none()
        && let Some(backend) = effective_python_backend_options(query.observed)
    {
        return Err(QueryError::UnsupportedBackendOptions(backend));
    }
    let declared = effective_python_declaration(query.observed);
    let Some(raw) = target.map(str::to_owned).or_else(|| declared.clone()) else {
        return Ok(SelectionReport::NoLanguageRequest);
    };
    let exact = crate::programs::shared::exact_version::exact_python_request(&raw).ok();
    let source_kind = if target.is_none() || declared.as_deref() == Some(raw.as_str()) {
        query.observed.python_request().ok().flatten().map_or(crate::libraries::language::source::PythonRuntimeSource::Prebuilt, |request| request.source)
    } else { crate::libraries::language::source::PythonRuntimeSource::Prebuilt };
    let prefix = target.is_some() && local_prefix(&raw);
    if exact.is_none() && !prefix {
        return Ok(SelectionReport::InvalidDeclaration { requested: raw });
    }
    // The current project's runtime pin applies only when querying its own
    // exact declaration. An override never inherits a different runtime pin.
    let runtime_pin = if target.is_none() || declared.as_deref() == Some(raw.as_str()) {
        match &exact {
            Some(requested) => crate::libraries::project::runtime_pin::observed_runtime_pin(
                query.observed,
                requested,
            )
            .map_err(|error| {
                QueryError::Unreadable(format!("project runtime source pin is unusable: {error:#}"))
            })?,
            None => None,
        }
    } else {
        None
    };
    if let Some(pin) = &runtime_pin {
        // Validate the fixed source even when the local library is empty. A
        // malformed commitment must not be disguised as an uninstalled item.
        crate::plugins::core::python::pinned_shared_language_spec(
            &raw,
            pin,
            false,
            query.context.settings(),
        )
        .map_err(|error| {
            QueryError::Unreadable(format!("project runtime source pin is unusable: {error:#}"))
        })?;
    }
    use crate::libraries::language::repository::LanguageEnumerateError;
    use crate::libraries::language::repository::LanguageRepository;
    let languages = LanguageRepository::open_readonly(&query.context)
        .map_err(|error| QueryError::Unreadable(format!("{error:#}")))?;
    let published = match languages.enumerate_published() {
        Ok(published) => published,
        Err(LanguageEnumerateError::Unreadable(error)) => {
            return Err(QueryError::Unreadable(format!("{error:#}")));
        }
        // Damaged stored facts are never reported as an uninstalled version.
        Err(LanguageEnumerateError::Corrupt(detail)) => {
            return Ok(SelectionReport::Corrupt { detail });
        }
    };
    let mut matching: Vec<_> = published
        .into_iter()
        .filter(|language| {
            language.matches_host_platform()
                && language.source_kind() == source_kind
                && match &exact {
                    Some(requested) => {
                        language.version() == requested.version_only()
                            && requested
                                .build_pin()
                                .is_none_or(|pin| language.build() == Some(pin))
                    }
                    None => language.version().starts_with(&format!("{raw}.")),
                }
        })
        .collect();
    if let Some(pin) = runtime_pin {
        let mut pinned = Vec::new();
        for language in matching {
            if language.matches_source_pin(&raw, &pin, query.context.settings())
                .map_err(|error| QueryError::Unreadable(format!("project runtime source pin is unusable: {error:#}")))?
            {
                pinned.push(language);
            }
        }
        matching = pinned;
    }
    matching.sort_by_key(|language| language.key().fingerprint());
    let into_candidate = |language: crate::libraries::language::repository::PublishedLanguage| {
        let mut candidate = LanguageCandidate::new(
            language.key(),
            language.payload_root().to_path_buf(),
            language.entrypoint().to_path_buf(),
            language.version(),
        );
        candidate.exact_version = language.build().map_or_else(
            || language.version(),
            |build| format!("{}+{build}", language.version()),
        );
        candidate
    };
    if matching.is_empty() {
        return Ok(SelectionReport::Missing { requested: raw });
    }
    if matching.len() == 1 {
        return Ok(SelectionReport::Selected(into_candidate(
            matching.into_iter().next().expect("one candidate"),
        )));
    }
    Ok(SelectionReport::Ambiguous {
        requested: raw,
        candidates: matching.into_iter().map(into_candidate).collect(),
    })
}

fn local_prefix(raw: &str) -> bool {
    let parts: Vec<_> = raw.split('.').collect();
    (1..=2).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

pub(crate) fn python_tool_name(observed: &ObservedProjectInputs, name: &str) -> bool {
    observed.python_tool_name(name)
}

fn effective_python_backend_options(observed: &ObservedProjectInputs) -> Option<String> {
    observed.python_request().err().filter(|error| error.starts_with("unsupported Python backend options")).map(str::to_owned)
}

/// Config discovery is ordered from nearest/highest-priority document to
/// ancestors and defaults. The first document declaring Python wins, using
/// the same order as the snapshot's effective-package merge kernel.
pub(crate) fn effective_python_declaration(observed: &ObservedProjectInputs) -> Option<String> {
    match observed.python_request() { Ok(request) => request.map(|request| request.version.clone()), Err(error) => Some(error.to_owned()) }
}

/// Cross-checks the same committed graph against the actual guarded layout.
/// Both values are needed: a catalog node alone is never Ready evidence.
pub(crate) fn validate_layout_graph(
    graph: &crate::libraries::project::package_graph::CommittedGraph,
    layout: &crate::libraries::package::lease::LayoutLease,
) -> eyre::Result<()> {
    use eyre::{ContextCompat, ensure};
    ensure!(
        layout.key() == *graph.layout() && layout.language() == *graph.language(),
        "committed graph differs from its actual layout or language"
    );
    ensure!(
        layout.graph() == graph.fingerprint(),
        "committed graph facts differ from the actual layout graph identity"
    );
    let packages: std::collections::BTreeMap<_, _> = layout
        .packages()
        .iter()
        .map(|package| (package.manifest().key(), &package.manifest().facts))
        .collect();
    ensure!(
        packages.len() == graph.nodes().len(),
        "committed graph package count differs from its actual layout"
    );
    for (index, node) in graph.nodes().iter().enumerate() {
        let facts = packages
            .get(node.instance())
            .context("committed graph package is absent from its actual layout")?;
        ensure!(
            facts.name == node.name()
                && facts.version == node.version()
                && facts.artifact == node.artifact()
                && facts.source == node.source_pin(),
            "committed graph node differs from its verified package manifest"
        );
        let mut dependencies: Vec<_> = graph
            .edges()
            .iter()
            .filter(|edge| edge.from() == index)
            .map(|edge| {
                graph
                    .nodes()
                    .get(edge.to())
                    .map(|node| *node.instance())
                    .context("committed graph dependency target is missing")
            })
            .collect::<eyre::Result<_>>()?;
        dependencies.sort_unstable();
        dependencies.dedup();
        ensure!(
            dependencies == facts.dependencies,
            "committed graph dependency edges differ from verified package facts"
        );
    }
    layout.check_binding()
}
