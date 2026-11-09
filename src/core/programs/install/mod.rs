//! Install request normalization; actual preparation and commit come later.

#[cfg(test)]
pub(crate) mod apply;
#[cfg(unix)]
pub(crate) mod build;
#[cfg(unix)]
pub(crate) mod conda;
#[cfg(unix)]
pub(crate) mod conda_project;
#[cfg(unix)]
pub(crate) mod conda_update;
#[cfg(unix)]
pub(crate) mod lock;
#[cfg(unix)]
pub(crate) mod lock_document;
#[cfg(unix)]
pub(crate) mod mixed;
#[cfg(unix)]
pub(crate) mod resolve;
#[cfg(unix)]
pub(crate) mod reuse;
#[cfg(unix)]
pub(crate) mod runtime;
#[cfg(unix)]
pub(crate) mod selection;
#[cfg(unix)]
pub(crate) mod update;
