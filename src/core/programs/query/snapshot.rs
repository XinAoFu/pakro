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

/// The effective Python tool declaration of one observation. Later documents
/// in the same observation win, matching the snapshot's document order; F1
/// projects only this one tool from the declaration set.
fn effective_python_declaration(observed: &ObservedProjectInputs) -> Option<String> {
    let mut declared = None;
    for (_, declarations) in observed.declarations() {
        for tool in &declarations.tools {
            if tool.name == "python" {
                declared = Some(tool.version.clone());
            }
        }
    }
    declared
}
