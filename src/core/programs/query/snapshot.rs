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
pub(crate) fn project_lock_for_query(
    observed: &ObservedProjectInputs,
    target: Option<&str>,
) -> eyre::Result<Option<crate::packages::python_resolution::lock_model::PythonLockModel>> {
    if target.is_some() && effective_python_declaration(observed).as_deref() != target {
        return Ok(None);
    }
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
        crate::programs::install::lock_document::validate_declarations(observed, &candidate)?;
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
            let expected = crate::plugins::core::python::pinned_shared_language_spec(
                &raw,
                &pin,
                language.spec().patch_sysconfig(),
                query.context.settings(),
            )
            .map_err(|error| {
                QueryError::Unreadable(format!("project runtime source pin is unusable: {error:#}"))
            })?;
            let actual_source = language.spec().plan().source();
            let expected_source = expected.plan().source();
            if expected.key() == language.key()
                && expected_source.filename() == actual_source.filename()
                && expected_source.digest() == actual_source.digest()
            {
                expected.plan().validate_source_locator(actual_source.url().as_str())
                    .map_err(|error|QueryError::Unreadable(format!("published runtime differs from the project's canonical source: {error:#}")))?;
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

/// Resolve only inert backend identity from the same frozen declarations.
/// Never ask a plugin or a second Config observation to classify ownership.
fn frozen_backend_name<'a>(observed: &'a ObservedProjectInputs, requested: &'a str) -> &'a str {
    let name = crate::args::split_bracketed_opts(requested).map_or(requested, |(name, _)| name);
    // Match Config::load_aliases and PakroToml::aliases without consulting a
    // second Config: later backends win; current entries replace legacy ones.
    observed
        .declarations()
        .filter_map(|(_, declarations)| {
            let fields = &declarations.unprojected_fields;
            fields
                .get("tool_alias")
                .and_then(|section| section.get(name))
                .or_else(|| fields.get("alias").and_then(|section| section.get(name)))?
                .get("backend")?
                .as_str()
        })
        .last()
        .unwrap_or(requested)
}

pub(crate) fn python_tool_name(observed: &ObservedProjectInputs, name: &str) -> bool {
    let backend = frozen_backend_name(observed, name);
    let base = crate::args::split_bracketed_opts(backend).map_or(backend, |(base, _)| base);
    matches!(base, "python" | "core:python" | "asdf:python")
}

fn python_backend_options(observed: &ObservedProjectInputs, name: &str) -> Option<String> {
    let backend = frozen_backend_name(observed, name);
    (crate::args::split_bracketed_opts(name).is_some()
        || crate::args::split_bracketed_opts(backend).is_some())
    .then(|| format!("{name} ({backend})"))
}

fn effective_python_backend_options(observed: &ObservedProjectInputs) -> Option<String> {
    if observed.scope() != crate::libraries::project::snapshot::ReadScope::LocalOnly {
        let names: Vec<_> = observed
            .environment()
            .keys()
            .filter_map(|key| crate::toolset::tool_from_env_var_name(key))
            .filter(|name| python_tool_name(observed, name))
            .collect();
        if !names.is_empty() {
            return names
                .iter()
                .find_map(|name| python_backend_options(observed, name));
        }
    }
    for (_, declarations) in observed.declarations() {
        let mut tools = declarations
            .tools
            .iter()
            .filter(|tool| python_tool_name(observed, &tool.name))
            .peekable();
        if tools.peek().is_some() {
            return tools.find_map(|tool| python_backend_options(observed, &tool.name));
        }
    }
    None
}

/// Config discovery is ordered from nearest/highest-priority document to
/// ancestors and defaults. The first document declaring Python wins, using
/// the same order as the snapshot's effective-package merge kernel.
pub(crate) fn effective_python_declaration(observed: &ObservedProjectInputs) -> Option<String> {
    if observed.scope() != crate::libraries::project::snapshot::ReadScope::LocalOnly {
        let mut versions: Vec<_> = observed
            .environment()
            .iter()
            .filter_map(|(key, value)| {
                let name = crate::toolset::tool_from_env_var_name(key)?;
                python_tool_name(observed, &name).then_some(value.as_str())
            })
            .collect();
        versions.dedup();
        if !versions.is_empty() {
            if let Some(backend) = effective_python_backend_options(observed) {
                return Some(format!("unsupported Python backend options for {backend}"));
            }
            return Some(versions.join(" "));
        }
    }
    observed.declarations().find_map(|(_, declarations)| {
        let tools: Vec<_> = declarations
            .tools
            .iter()
            .filter(|tool| python_tool_name(observed, &tool.name))
            .collect();
        if tools.is_empty() {
            return None;
        }
        if let Some(tool) = tools
            .iter()
            .find(|tool| python_backend_options(observed, &tool.name).is_some())
        {
            return Some(format!(
                "unsupported Python backend options for {}",
                tool.name
            ));
        }
        if let Some(tool) = tools.iter().find(|tool| tool.selector != "version") {
            return Some(format!(
                "unsupported Python selector {} for {}",
                tool.selector, tool.version
            ));
        }
        if let Some(tool) = tools.iter().find(|tool| !tool.options.is_empty()) {
            return Some(format!(
                "unsupported Python options [{}] for {}",
                tool.options.keys().cloned().collect::<Vec<_>>().join(", "),
                tool.version
            ));
        }
        Some(
            tools
                .iter()
                .map(|tool| tool.version.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    })
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
