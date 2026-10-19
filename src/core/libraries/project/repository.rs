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

use crate::packages::python_environment::model::{CatalogCodecLimits, CatalogModel, ModelLimits};
use crate::programs::OperationContext;

use super::read_views::ProjectReadViews;

const PROJECTS_CATALOG: &str = "projects";

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
        let writer = StoreWriter::open_writer(
            &path,
            repository_uid(),
            RootLimits::default(),
            context.policy().deadline,
        )?;
        context.checkpoint()?;
        let (model, catalog) = load_catalog(&path, context)?;
        Ok(Self {
            context: context.clone(),
            path,
            writer: Some(Arc::new(writer)),
            model,
            catalog,
            codec: CatalogCodecLimits::default(),
        })
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

/// Loads the catalog bytes from one store. A missing store or catalog file is
/// an empty model; damaged bytes are an explicit error.
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
            Ok((model, Some(snapshot)))
        }
        None => Ok((CatalogModel::new(ModelLimits::default())?, None)),
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
