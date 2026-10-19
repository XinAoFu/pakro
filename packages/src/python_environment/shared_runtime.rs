//! The single shared runtime bridge for package-carrying environments.
//!
//! New production chains hold the real LanguageLease across preparation and
//! verification instead of copying a full runtime per environment. Only the
//! necessary lightweight adapters (package paths, startup configuration) are
//! generated; the interpreter and standard library stay in the shared
//! language payload.

use eyre::Result;

use crate::libraries::language::lease::LanguageLease;
use crate::programs::OperationContext;

/// Binds one real language lease for a package-carrying preparation. The
/// lease is held (not copied) for the whole preparation and verification.
pub(crate) struct SharedRuntime<'a> {
    context: &'a OperationContext,
    lease: LanguageLease,
}

impl<'a> SharedRuntime<'a> {
    pub(crate) fn bind(context: &'a OperationContext, lease: LanguageLease) -> Result<Self> {
        lease.check_binding()?;
        Ok(Self { context, lease })
    }
    pub(crate) fn lease(&self) -> &LanguageLease {
        &self.lease
    }
    /// The interpreter path stays inside the shared payload; adapters only
    /// point package paths at it, they never duplicate the interpreter.
    pub(crate) fn interpreter(&self) -> &std::path::Path {
        self.lease.entrypoint()
    }
    pub(crate) fn context(&self) -> &OperationContext {
        self.context
    }
}

#[cfg(test)]
mod tests;
