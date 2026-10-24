//! Pure admission for decoded targets. No library reads, configuration parsing,
//! provider execution or acquisition belongs in this module.

use crate::args::BackendArg;
use eyre::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PythonWriteOperation {
    Use,
    Unuse,
    Upgrade,
    Add,
    Remove,
    Prune,
    LegacyInstall,
    Force,
    PostInstall,
}

/// Consume the resolved backend identity, including loaded aliases. A tool
/// merely containing `python` in its name is not a Python runtime.
pub fn is_python_backend(backend: &BackendArg) -> bool {
    matches!(
        backend.full_without_opts().as_str(),
        "core:python" | "asdf:python"
    )
}

/// A local query must never silently discard runtime build/install options.
/// P1 selects existing Python instances by exact version/build or local prefix.
pub fn validate_python_query_options(backend: &BackendArg) -> Result<()> {
    eyre::ensure!(
        !is_python_backend(backend)
            || (backend.opts.is_none() && backend.full() == backend.full_without_opts()),
        "shared Python local queries do not support runtime options; select an installed version or build explicitly"
    );
    Ok(())
}

/// These are provider identities, not shell fragments. Qualified monorepo
/// provider ids retain the same built-in provider semantics.
pub fn is_python_provider(provider: &str) -> bool {
    matches!(
        provider.rsplit(':').next(),
        Some("python" | "pip" | "uv" | "poetry")
    )
}

pub fn reject_python_write(operation: PythonWriteOperation, backend: &BackendArg) -> Result<()> {
    if is_python_backend(backend) {
        reject(operation)?;
    }
    Ok(())
}

pub fn reject_python_provider_write(operation: PythonWriteOperation, provider: &str) -> Result<()> {
    if is_python_provider(provider) {
        reject(operation)?;
    }
    Ok(())
}

fn reject(operation: PythonWriteOperation) -> Result<()> {
    eyre::bail!(
        "Python {operation:?} is not supported by the shared Python transaction path yet; \
         refusing the complete request before any effect. Use explicit `pakro install` \
         for supported Python installation; declaration changes and bulk prune are unavailable"
    )
}

#[cfg(test)]
mod tests;
