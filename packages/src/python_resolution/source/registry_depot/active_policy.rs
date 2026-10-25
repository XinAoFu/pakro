//! Actual project/file-index adapter for the pure active-policy query.

use super::*;
use crate::packages::python_resolution::candidate_policy::{
    ActiveCandidateAssessment, CandidateKnowledge, CandidatePolicyError, CandidatePolicyUsage,
    PolicyNotice, assess_cached_candidate,
};
use pakro_resolver::python_native::policy::{
    ActiveCandidatePolicy, ActivePolicyDecision, ActivePolicyError, ActivePolicyQuery,
};
use std::cell::RefCell;

pub(super) struct RegistryActivePolicy<'a, 'root, 'config> {
    pub depot: &'a ResolveRegistryDepot<'root, 'config>,
    pub usage: &'a RefCell<CandidatePolicyUsage>,
    pub notices: &'a RefCell<BTreeMap<CandidateKey, Vec<PolicyNotice>>>,
}
fn invalid(_: RegistryDepotError) -> ActivePolicyError {
    ActivePolicyError::InvalidInput
}
impl ActiveCandidatePolicy for RegistryActivePolicy<'_, '_, '_> {
    fn evaluate(
        &self,
        query: ActivePolicyQuery<'_>,
    ) -> Result<ActivePolicyDecision, ActivePolicyError> {
        self.depot.check_live().map_err(invalid)?;
        let entry = self
            .depot
            .projects
            .get(query.name)
            .ok_or(ActivePolicyError::InvalidInput)?;
        let project = entry
            .owner
            .as_ref()
            .ok_or(ActivePolicyError::InvalidInput)?;
        if query.candidates.len() != entry.candidates.len() {
            return Err(ActivePolicyError::InvalidInput);
        }
        let universe = entry
            .universe
            .as_ref()
            .ok_or(ActivePolicyError::InvalidInput)?;
        if universe.len() != entry.candidates.len() {
            return Err(ActivePolicyError::InvalidInput);
        }
        if project.data.project.files().len() > self.depot.context.limits.candidates.max_files {
            return Err(ActivePolicyError::Limit);
        }
        let mut knowledge = Vec::new();
        knowledge
            .try_reserve_exact(entry.candidates.len())
            .map_err(|_| ActivePolicyError::Limit)?;
        let mut selected = None;
        for (position, (view, actual)) in query.candidates.iter().zip(&entry.candidates).enumerate()
        {
            if !std::ptr::eq(view.candidate, actual) {
                return Err(ActivePolicyError::InvalidInput);
            }
            let file = self
                .depot
                .files
                .get(&actual.key)
                .filter(|file| file.project == query.name)
                .ok_or(ActivePolicyError::InvalidInput)?;
            if universe.file_index(position) != Some(file.file_index) {
                return Err(ActivePolicyError::InvalidInput);
            }
            let expected = self
                .depot
                .metadata
                .get(&actual.key)
                .map(|entry| &entry.input);
            let value = match (view.metadata, expected) {
                (None, None) => CandidateKnowledge::Unknown,
                (Some(observed), Some(actual)) if std::ptr::eq(observed, actual) => {
                    if actual.rejection.is_some() {
                        CandidateKnowledge::Incompatible
                    } else {
                        CandidateKnowledge::Compatible
                    }
                }
                _ => return Err(ActivePolicyError::InvalidInput),
            };
            knowledge.push(value);
            if actual.key == query.selected {
                selected = Some(position);
            }
        }
        let selected = selected.ok_or(ActivePolicyError::InvalidInput)?;
        let mut constraints = Vec::new();
        if query.constraints.len() > self.depot.context.limits.candidates.max_constraints {
            return Err(ActivePolicyError::Limit);
        }
        constraints
            .try_reserve_exact(query.constraints.len())
            .map_err(|_| ActivePolicyError::Limit)?;
        constraints.extend(query.constraints.iter().map(|constraint| constraint.raw));
        let assessment = assess_cached_candidate(
            universe,
            selected,
            &constraints,
            &knowledge,
            self.depot.context.limits.candidates,
            &mut self.usage.borrow_mut(),
        )
        .map_err(|error| match error {
            CandidatePolicyError::Limit => ActivePolicyError::Limit,
            _ => ActivePolicyError::InvalidInput,
        })?;
        self.depot.check_live().map_err(invalid)?;
        match assessment {
            ActiveCandidateAssessment::Eligible(notices) => {
                self.notices.borrow_mut().insert(query.selected, notices);
                Ok(ActivePolicyDecision::Allow)
            }
            ActiveCandidateAssessment::Reject => Ok(ActivePolicyDecision::Reject),
            ActiveCandidateAssessment::RejectWithAlternative(index) => {
                let alternative =
                    query
                        .candidates
                        .iter()
                        .enumerate()
                        .find(|(position, candidate)| {
                            universe.file_index(*position) == Some(index)
                                && knowledge[*position] == CandidateKnowledge::Compatible
                                && candidate.candidate.key != query.selected
                                && self.depot.files.get(&candidate.candidate.key).is_some_and(
                                    |file| file.project == query.name && file.file_index == index,
                                )
                        })
                        .map(|(_, candidate)| candidate)
                        .ok_or(ActivePolicyError::InvalidInput)?;
                // The complete loop above already bound the candidate and
                // metadata pointers to this project's actual retained owner.
                Ok(ActivePolicyDecision::RejectWithAlternative(
                    alternative.candidate.key,
                ))
            }
            ActiveCandidateAssessment::NeedMetadata(index) => {
                let unknown =
                    query
                        .candidates
                        .iter()
                        .find(|candidate| {
                            candidate.metadata.is_none()
                                && self.depot.files.get(&candidate.candidate.key).is_some_and(
                                    |file| file.project == query.name && file.file_index == index,
                                )
                        })
                        .ok_or(ActivePolicyError::InvalidInput)?;
                Ok(ActivePolicyDecision::NeedMetadata(unknown.candidate.key))
            }
        }
    }
}
