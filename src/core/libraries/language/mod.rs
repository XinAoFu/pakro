//! Shared language descriptions and capabilities for one actual installation.

pub(crate) mod identity;
#[cfg(unix)]
pub(crate) mod lease;
#[cfg(unix)]
pub(crate) mod manifest;
#[cfg(unix)]
pub(crate) mod publication;
#[cfg(unix)]
pub(crate) mod repository;
