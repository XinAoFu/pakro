//! Raw catalog metadata edits inside an already locked store control session.
//!
//! This backend supplies no validated-environment or publication authority.

use eyre::{Result, ensure, eyre};

use super::model::{CatalogCodecLimits, CatalogModel};
use super::prefix::control::{ControlNamespace, ControlSnapshot, StoreControl};

const STATE: &str = "state.json";

fn load(
    control: &StoreControl<'_>,
    limits: CatalogCodecLimits,
) -> Result<(CatalogModel, Option<ControlSnapshot>)> {
    ensure!(
        limits.max_document_bytes != 0,
        "catalog document byte limit must be nonzero"
    );
    let empty = CatalogModel::new(limits.model)?;
    let max_bytes = u64::try_from(limits.max_document_bytes)
        .map_err(|_| eyre!("catalog document byte limit overflow"))?;
    let snapshot = control.read_bounded(ControlNamespace::Catalog, STATE, max_bytes)?;
    let model = match &snapshot {
        Some(snapshot) => CatalogModel::decode_catalog(snapshot.bytes(), limits)?,
        None => empty,
    };
    Ok((model, snapshot))
}

pub(crate) fn read_catalog_metadata(
    control: &StoreControl<'_>,
    limits: CatalogCodecLimits,
) -> Result<CatalogModel> {
    load(control, limits).map(|(model, _snapshot)| model)
}

pub(crate) fn edit_catalog_metadata<T>(
    control: &mut StoreControl<'_>,
    limits: CatalogCodecLimits,
    edit: impl FnOnce(&mut CatalogModel) -> Result<T>,
) -> Result<T> {
    let (mut model, snapshot) = load(control, limits)?;
    let result = edit(&mut model)?;
    let bytes = model.encode_catalog(limits)?;
    control.replace(ControlNamespace::Catalog, STATE, snapshot.as_ref(), &bytes)?;
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests;
