//! Shared language descriptions and capabilities for one actual installation.

pub(crate) mod identity;
#[cfg(unix)]
pub(crate) mod conda;
pub(crate) mod source;
#[cfg(unix)]
pub(crate) mod lease;
#[cfg(unix)]
pub(crate) mod manifest;
pub(crate) mod mutation;
#[cfg(unix)]
pub(crate) mod publication;
#[cfg(unix)]
pub(crate) mod recovery;
#[cfg(unix)]
pub(crate) mod removal;
#[cfg(unix)]
pub(crate) mod repository;
