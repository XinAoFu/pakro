//! Readonly catalog observations carried from project admission to commit.

use std::collections::BTreeMap;

use eyre::{Result, ensure};

use super::{identity::ProjectIdentity, repository::ProjectRepository};
use crate::packages::python_environment::model::{
    CatalogModel, ProjectRegistration, SelectionKey, SelectionStamp,
};
use crate::programs::OperationContext;

/// Includes cleared slots: forgetting a project must invalidate work prepared
/// against both an old target and an originally absent target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedProjectState {
    identity: ProjectIdentity,
    registration: Option<ProjectRegistration>,
    slots: BTreeMap<SelectionKey, SelectionStamp>,
}

#[cfg(test)]
mod tests;

impl ExpectedProjectState {
    pub(crate) fn capture(context: &OperationContext, identity: &ProjectIdentity) -> Result<Self> {
        let repository = ProjectRepository::open_readonly(context)?;
        Ok(Self::from_model(repository.model(), *identity))
    }

    pub(crate) fn from_model(model: &CatalogModel, identity: ProjectIdentity) -> Self {
        Self {
            identity,
            registration: model.project_registration(identity.as_uuid()).cloned(),
            slots: model
                .selection_slots()
                .filter(|(key, _)| key.project() == identity.as_uuid())
                .map(|(key, stamp)| (key.clone(), stamp.clone()))
                .collect(),
        }
    }

    pub(crate) fn ensure_current(&self, model: &CatalogModel) -> Result<()> {
        ensure!(
            *self == Self::from_model(model, self.identity),
            "project registration or selections changed while the operation was being prepared"
        );
        Ok(())
    }

    pub(crate) fn ensure_identity(&self, identity: &ProjectIdentity) -> Result<()> {
        ensure!(
            self.identity == *identity,
            "project state belongs to another project"
        );
        Ok(())
    }

    pub(crate) fn was_registered(&self) -> bool {
        self.registration.is_some() || !self.slots.is_empty()
    }
}
