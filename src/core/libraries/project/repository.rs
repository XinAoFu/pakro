//! Persistent IO for the single authoritative project catalog (R05 store).
//!
//! Registrations and language-only selections live in one catalog file under
//! the managed data root; path lists or reference counts elsewhere are
//! rebuildable views, never a second authority. Readonly opens never
//! initialize or repair a library, and a damaged catalog is an error, never
//! an empty registry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{ContextCompat, Result, ensure};
use pakro_store::Digest;
use pakro_store::control::{
    ControlLimits, ControlNamespace, ControlSnapshot, ReadStore, RootLimits, StoreWriter,
};

use crate::packages::python_environment::model::{CatalogCodecLimits, CatalogModel};
use crate::programs::OperationContext;

use super::read_views::ProjectReadViews;

const PROJECTS_CATALOG: &str = "projects";

fn initialize_store_writer(context: &OperationContext, path: &Path) -> Result<Arc<StoreWriter>> {
    context.checkpoint()?;
    let writer = Arc::new(StoreWriter::open_writer(
        path,
        repository_uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?);
    context.checkpoint()?;
    // Only creation of this actual store may initialize its empty authority.
    // Opening an existing store never repairs a missing catalog.
    if writer.initialized_here() {
        let mut control = writer.control(ControlLimits::default())?;
        if control
            .read(ControlNamespace::Catalog, PROJECTS_CATALOG)?
            .is_none()
        {
            let limits = CatalogCodecLimits::default();
            let empty = CatalogModel::new(limits.model)?.encode_catalog(limits)?;
            control.replace(ControlNamespace::Catalog, PROJECTS_CATALOG, None, &empty)?;
        }
    }
    Ok(writer)
}

pub(crate) struct ProjectRepository {
    context: OperationContext,
    path: PathBuf,
    writer: Option<Arc<StoreWriter>>,
    model: CatalogModel,
    catalog: Option<ControlSnapshot>,
    codec: CatalogCodecLimits,
}

impl ProjectRepository {
    /// Readonly open of the projects catalog. A missing store is an empty
    /// registration view; a damaged catalog is an error, never an empty list.
    pub(crate) fn open_readonly(context: &OperationContext) -> Result<Self> {
        context.checkpoint()?;
        let path = context.requested_data_root().join("projects");
        let (model, catalog) = load_catalog(&path, context)?;
        Ok(Self {
            context: context.clone(),
            path,
            writer: None,
            model,
            catalog,
            codec: CatalogCodecLimits::default(),
        })
    }

    /// Writer open, reserved for the controlled registration and selection
    /// commit. Queries never take this path.
    pub(crate) fn open_writer(context: &OperationContext) -> Result<Self> {
        context.checkpoint()?;
        ensure!(
            context.policy().writes.project_selection,
            "operation policy forbids project selection writes"
        );
        let path = context.requested_data_root().join("projects");
        let writer = initialize_store_writer(context, &path)?;
        let (model, catalog) = load_catalog(&path, context)?;
        Ok(Self {
            context: context.clone(),
            path,
            writer: Some(writer),
            model,
            catalog,
            codec: CatalogCodecLimits::default(),
        })
    }

    /// Library deletion may own an operation journal without obtaining a
    /// mutable registration/selection model. Existing catalog damage still
    /// rejects this path; only the actual initializer creates an empty one.
    pub(crate) fn open_reclaim_journal_writer(
        context: &OperationContext,
    ) -> Result<Arc<StoreWriter>> {
        context.checkpoint()?;
        ensure!(
            context.policy().writes.libraries,
            "operation policy forbids reclamation journal writes"
        );
        let path = context.requested_data_root().join("projects");
        let writer = initialize_store_writer(context, &path)?;
        let _ = load_catalog(&path, context)?;
        context.checkpoint()?;
        Ok(writer)
    }

    /// Reclamation can retire only these derived generation/index records.
    /// The model's active-slot guard and the ordinary catalog CAS remain in
    /// force; this does not expose a mutable selection model to the caller.
    pub(crate) fn forget_reclaimed_retired(
        context: &OperationContext,
        allocations: &[String],
    ) -> Result<()> {
        context.checkpoint()?;
        ensure!(
            context.policy().writes.libraries,
            "operation policy forbids reclaimed index writes"
        );
        ensure!(
            allocations.len() <= 65536,
            "retired index capacity exceeded"
        );
        if allocations.is_empty() {
            return Ok(());
        }
        let writer = Self::open_reclaim_journal_writer(context)?;
        let path = context.requested_data_root().join("projects");
        let (model, catalog) = load_catalog(&path, context)?;
        let mut repository = Self {
            context: context.clone(),
            path,
            writer: Some(writer),
            model,
            catalog,
            codec: CatalogCodecLimits::default(),
        };
        let mut changed = false;
        for allocation in allocations {
            if repository.model.generation(allocation).is_some() {
                repository.model.forget_retired(allocation)?;
                changed = true;
            }
        }
        if changed {
            repository.commit_model()?;
        }
        Ok(())
    }

    pub(crate) fn model(&self) -> &CatalogModel {
        &self.model
    }
    pub(crate) fn model_mut(&mut self) -> &mut CatalogModel {
        &mut self.model
    }
    /// The writer this repository owns; readonly repositories have none.
    pub(crate) fn writer(&self) -> Result<Arc<StoreWriter>> {
        self.writer
            .clone()
            .context("readonly project repository has no writer")
    }
    /// Re-reads the committed catalog from the store, keeping the in-memory
    /// model honest after waiting on protection.
    pub(crate) fn reload(&mut self) -> Result<()> {
        self.context.checkpoint()?;
        let (model, catalog) = load_catalog(&self.path, &self.context)?;
        self.model = model;
        self.catalog = catalog;
        Ok(())
    }
    /// Encodes the current model and replaces the catalog file in one CAS
    /// step against the snapshot this repository last observed. On failure
    /// the disk state is unchanged; the in-memory model may be ahead and must
    /// be dropped by the caller.
    pub(crate) fn commit_model(&mut self) -> Result<()> {
        self.context.checkpoint()?;
        validate_default_selections(&self.model)?;
        let bytes = self.model.encode_catalog(self.codec)?;
        let writer = self.writer()?;
        let mut control = writer.control(ControlLimits::default())?;
        let snapshot = control.replace(
            ControlNamespace::Catalog,
            PROJECTS_CATALOG,
            self.catalog.as_ref(),
            &bytes,
        )?;
        self.context.checkpoint()?;
        self.catalog = Some(snapshot);
        Ok(())
    }
    pub(crate) fn read_views(&self) -> ProjectReadViews<'_> {
        ProjectReadViews::new(self)
    }
}

#[cfg(test)]
mod reclaim_writer_tests {
    use super::*;
    use crate::libraries::language::repository::tests::operation_context;
    use std::time::Duration;

    #[test]
    fn p4_reclaim_journal_writer_does_not_grant_selection_model_access() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, true, Duration::from_secs(30));
        assert!(context.policy().writes.libraries);
        assert!(!context.policy().writes.project_selection);
        let writer = ProjectRepository::open_reclaim_journal_writer(&context).unwrap();
        let mut control = writer.control(ControlLimits::default()).unwrap();
        let before = control
            .read(ControlNamespace::Catalog, PROJECTS_CATALOG)
            .unwrap()
            .unwrap();
        let bytes = before.bytes().to_vec();
        control
            .replace(ControlNamespace::Journals, "p4-journal-probe", None, b"{}")
            .unwrap();
        assert_eq!(
            control
                .read(ControlNamespace::Catalog, PROJECTS_CATALOG)
                .unwrap()
                .unwrap()
                .bytes(),
            bytes.as_slice()
        );
        drop(control);
        assert!(ProjectRepository::open_writer(&context).is_err());
    }

    #[test]
    fn p4_reclaim_journal_writer_rejects_readonly_and_missing_existing_catalog() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let readonly = operation_context(&root, false, Duration::from_secs(30));
        assert!(ProjectRepository::open_reclaim_journal_writer(&readonly).is_err());
        assert!(!readonly.requested_data_root().join("projects").exists());
        let context = operation_context(&root, true, Duration::from_secs(30));
        let writer = ProjectRepository::open_reclaim_journal_writer(&context).unwrap();
        let mut control = writer.control(ControlLimits::default()).unwrap();
        let existing = control
            .read(ControlNamespace::Catalog, PROJECTS_CATALOG)
            .unwrap()
            .unwrap();
        control.remove(&existing).unwrap();
        drop(control);
        assert!(ProjectRepository::open_reclaim_journal_writer(&context).is_err());
        assert!(
            writer
                .control(ControlLimits::default())
                .unwrap()
                .read(ControlNamespace::Catalog, PROJECTS_CATALOG)
                .unwrap()
                .is_none()
        );
    }
}

/// An absent store has no registrations. Once initialized, its authoritative
/// catalog must exist; loss is unknown state, never an empty reference set.
fn load_catalog(
    path: &Path,
    context: &OperationContext,
) -> Result<(CatalogModel, Option<ControlSnapshot>)> {
    context.checkpoint()?;
    let limits = CatalogCodecLimits::default();
    let Some(read) = ReadStore::open_readonly(
        path,
        repository_uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok((CatalogModel::new(limits.model)?, None));
    };
    let control = read.control(ControlLimits::default())?;
    match control.read(ControlNamespace::Catalog, PROJECTS_CATALOG)? {
        Some(snapshot) => {
            let model = CatalogModel::decode_catalog(snapshot.bytes(), limits)
                .map_err(|error| eyre::eyre!("project catalog is damaged: {error:#}"))?;
            validate_default_selections(&model)?;
            Ok((model, Some(snapshot)))
        }
        None => eyre::bail!(
            "initialized project store is missing its authoritative catalog; references are unknown"
        ),
    }
}

/// The effective uid for store binding in this process.
pub(crate) fn repository_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// The canonical language identity string stored in selections.
pub(crate) fn language_digest(fingerprint: [u8; 32]) -> String {
    Digest::Sha256(fingerprint).to_string()
}

/// A directory default group has one active target. Multiple default targets
/// are damaged project facts; readonly views must never choose by revisions
/// that belong to different independent target slots.
fn validate_default_selections(model: &CatalogModel) -> Result<()> {
    let mut projects = std::collections::BTreeSet::new();
    for (key, stamp) in model.selection_slots() {
        if key.group() == crate::programs::shared::commit::DEFAULT_GROUP
            && (stamp.language().is_some() || stamp.generation().is_some())
        {
            ensure!(
                projects.insert(key.project()),
                "project catalog has multiple active default target selections"
            );
        }
    }
    Ok(())
}
