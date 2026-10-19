//! Native Python environment allocation, validation and publication.

pub(crate) mod build;
pub(crate) mod catalog;
pub(crate) mod journal;

pub(crate) mod model;
pub(crate) mod prefix;
pub(crate) mod resolved_build;
pub(crate) mod runtime_bootstrap;
pub(crate) mod runtime_layout;
pub(crate) mod runtime_snapshot;
pub(crate) mod runtime_source;
pub(crate) mod runtime_startup;
pub(crate) mod runtime_target;
pub(crate) mod seal;
