//! Read-only projections of committed project registrations and selections.

use std::path::PathBuf;

use pakro_store::Digest;
use uuid::Uuid;

use super::identity::ProjectIdentity;
use super::repository::ProjectRepository;

pub(crate) struct RegisteredProject {
    identity: ProjectIdentity,
    root: PathBuf,
    revision: u64,
}

impl RegisteredProject {
    pub(crate) fn identity(&self) -> &ProjectIdentity {
        &self.identity
    }
    pub(crate) fn root(&self) -> &PathBuf {
        &self.root
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// The committed language-only selection of one project. `generation_allocation`
/// stays `None` in F1: a selection without a real package layout must never
/// carry a fabricated `g-*` allocation or receipt.
pub(crate) struct LanguageSelectionView {
    group: String,
    language: [u8; 32],
    packages: Vec<[u8; 32]>,
    generation_allocation: Option<String>,
    revision: u64,
}

impl LanguageSelectionView {
    pub(crate) fn group(&self) -> &str {
        &self.group
    }
    pub(crate) fn language(&self) -> &[u8; 32] {
        &self.language
    }
    pub(crate) fn package_references(&self) -> &[[u8; 32]] {
        &self.packages
    }
    pub(crate) fn generation_allocation(&self) -> Option<&str> {
        self.generation_allocation.as_deref()
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

pub(crate) struct ProjectReadViews<'a> {
    repository: &'a ProjectRepository,
}

impl<'a> ProjectReadViews<'a> {
    pub(super) fn new(repository: &'a ProjectRepository) -> Self {
        Self { repository }
    }

    /// Committed registrations only. Damage in the catalog surfaces at the
    /// repository level; these views never reconstruct or repair anything.
    pub(crate) fn registered_projects(&self) -> Vec<RegisteredProject> {
        self.repository
            .model()
            .registered_projects()
            .map(|(project, fact)| RegisteredProject {
                identity: ProjectIdentity::from_uuid(project),
                root: PathBuf::from(fact.root()),
                revision: self.latest_revision(project),
            })
            .collect()
    }

    /// The committed language selection of one project, if any. The highest
    /// revision wins when several groups carried language selections.
    pub(crate) fn language_selection(
        &self,
        identity: &ProjectIdentity,
    ) -> Option<LanguageSelectionView> {
        let (key, stamp) = self
            .repository
            .model()
            .selection_slots()
            .filter(|(key, stamp)| {
                key.project() == identity.as_uuid() && stamp.language().is_some()
            })
            .max_by_key(|(_, stamp)| stamp.revision())?;
        let language = stamp.language()?;
        let Digest::Sha256(fingerprint) = language.parse::<Digest>().ok()? else {
            return None;
        };
        Some(LanguageSelectionView {
            group: key.group().to_string(),
            language: fingerprint,
            packages: Vec::new(),
            generation_allocation: stamp.generation().map(str::to_string),
            revision: stamp.revision(),
        })
    }

    fn latest_revision(&self, project: Uuid) -> u64 {
        self.repository
            .model()
            .selection_slots()
            .filter(|(key, _)| key.project() == project)
            .map(|(_, stamp)| stamp.revision())
            .max()
            .unwrap_or(0)
    }
}
