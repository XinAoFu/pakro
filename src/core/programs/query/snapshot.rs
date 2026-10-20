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
use crate::programs::shared::exact_version::ExactPythonRequest;

#[derive(Debug, Error)]
pub(crate) enum QueryError {
    #[error("operation deadline expired or was cancelled")]
    Cancelled,
    #[error("observed project inputs changed; observe the project again")]
    StaleInput,
    #[error("language library state is unreadable: {0}")]
    Unreadable(String),
}

/// One published local instance, described from its real manifest and payload
/// facts. Carrying this value is not a lease and grants no protection.
#[derive(Debug, Clone)]
pub(crate) struct LanguageCandidate {
    key: LanguageKey,
    root: PathBuf,
    entrypoint: PathBuf,
    version: String,
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
        requested: ExactPythonRequest,
    },
    Ambiguous {
        requested: ExactPythonRequest,
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
    context.checkpoint().map_err(|_| QueryError::Cancelled)?;
    observed.recheck().map_err(|_| QueryError::StaleInput)?;
    Ok(QueryContext {
        context: context.clone(),
        observed,
    })
}

/// Matches the complete exact declaration against published local instances.
/// Only real manifests of the shared language library are considered; nothing
/// is acquired and no remote source is contacted for a missing item.
pub(crate) fn select_existing(query: &QueryContext<'_>) -> Result<SelectionReport, QueryError> {
    query
        .context
        .checkpoint()
        .map_err(|_| QueryError::Cancelled)?;
    query
        .observed
        .recheck()
        .map_err(|_| QueryError::StaleInput)?;
    let Some(raw) = effective_python_declaration(query.observed) else {
        return Ok(SelectionReport::NoLanguageRequest);
    };
    let requested = match crate::programs::shared::exact_version::exact_python_request(&raw) {
        Ok(request) => request,
        Err(_) => {
            return Ok(SelectionReport::InvalidDeclaration { requested: raw });
        }
    };
    let runtime_pin =
        crate::libraries::project::runtime_pin::observed_runtime_pin(query.observed, &requested)
            .map_err(|error| {
                QueryError::Unreadable(format!("project runtime source pin is unusable: {error:#}"))
            })?;
    if let Some(pin) = &runtime_pin {
        // Validate the fixed source even when the local library is empty. A
        // malformed commitment must not be disguised as an uninstalled item.
        crate::plugins::core::python::pinned_shared_language_spec(
            requested.as_str(),
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
            language.version() == requested.version_only()
                && language.matches_host_platform()
                && requested
                    .build_pin()
                    .is_none_or(|pin| language.build() == Some(pin))
        })
        .collect();
    if let Some(pin) = runtime_pin {
        let mut pinned = Vec::new();
        for language in matching {
            let expected = crate::plugins::core::python::pinned_shared_language_spec(
                requested.as_str(),
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
        LanguageCandidate::new(
            language.key(),
            language.payload_root().to_path_buf(),
            language.entrypoint().to_path_buf(),
            language.version(),
        )
    };
    if matching.is_empty() {
        return Ok(SelectionReport::Missing { requested });
    }
    if matching.len() == 1 {
        return Ok(SelectionReport::Selected(into_candidate(
            matching.into_iter().next().expect("one candidate"),
        )));
    }
    Ok(SelectionReport::Ambiguous {
        requested,
        candidates: matching.into_iter().map(into_candidate).collect(),
    })
}

/// Config discovery is ordered from nearest/highest-priority document to
/// ancestors and defaults. The first document declaring Python wins, using
/// the same order as the snapshot's effective-package merge kernel.
pub(crate) fn effective_python_declaration(observed: &ObservedProjectInputs) -> Option<String> {
    observed.declarations().find_map(|(_, declarations)| {
        let tools: Vec<_> = declarations
            .tools
            .iter()
            .filter(|tool| tool.name == "python")
            .collect();
        if tools.is_empty() {
            return None;
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
