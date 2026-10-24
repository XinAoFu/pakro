//! Descriptive request contracts; these values do not grant filesystem authority.

#[cfg(unix)]
pub(crate) mod commit;
pub(crate) mod context;
pub(crate) mod errors;
pub(crate) mod exact_version;
pub(crate) mod policy;
pub(crate) mod python_entry;
pub(crate) mod report;
pub(crate) mod types;
