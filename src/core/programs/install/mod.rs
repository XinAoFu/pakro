//! Install request normalization; actual preparation and commit come later.

#[cfg(test)]
pub(crate) mod apply;
#[cfg(unix)]
pub(crate) mod build;
#[cfg(unix)]
pub(crate) mod lock;
#[cfg(unix)]
pub(crate) mod lock_document;
#[cfg(unix)]
pub(crate) mod resolve;
#[cfg(unix)]
pub(crate) mod runtime;
#[cfg(unix)]
pub(crate) mod selection;
