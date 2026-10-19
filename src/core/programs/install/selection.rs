//! Controlled application of one directory selection.
//!
//! Only existing, exact, verified objects are registered here; no download,
//! install or declaration-edit path is reachable from this entry. Protections
//! are re-taken in project → object SH → catalog order and the declaration and
//! Ready state are rechecked before the single-catalog commit.

use eyre::Result;

use crate::libraries::language::identity::LanguageKey;
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::snapshot::ObservedProjectInputs;
use crate::programs::query::snapshot::LanguageCandidate;
use crate::programs::shared::commit::{commit_selection, prepare_selection_commit};
use crate::programs::shared::context::OperationContext;
use crate::programs::shared::report::{OperationEffects, OperationReport, OperationState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionSubject {
    Project(ProjectIdentity),
    Language(LanguageKey),
}

pub(crate) fn apply_directory_selection(
    context: &OperationContext,
    observed: &ObservedProjectInputs,
    candidate: &LanguageCandidate,
) -> Result<OperationReport<SelectionSubject>> {
    let prepared = prepare_selection_commit(context, observed, candidate)?;
    let _receipt = commit_selection(prepared)?;
    let effects = OperationEffects {
        added: Vec::new(),
        reused: vec![SelectionSubject::Language(*candidate.key())],
        associations_changed: vec![SelectionSubject::Project(
            ProjectIdentity::from_observation(observed)?,
        )],
        retained: Vec::new(),
        removed: Vec::new(),
    };
    Ok(OperationReport::new(
        OperationState::Committed,
        effects,
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests;
