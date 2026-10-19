//! Immutable expanded package trees extracted from verified wheel blobs.
//!
//! A tree is the unpacked wheel content keyed by its expansion recipe; the
//! same content is extracted once and reused. Damage is reported, never
//! treated as missing.

use eyre::Result;

use crate::programs::OperationContext;

/// A verified, immutable expanded tree bound to its blob digest.
pub(crate) struct PreparedTree {
    digest: pakro_store::Digest,
}

impl PreparedTree {
    pub(crate) fn digest(&self) -> &pakro_store::Digest {
        &self.digest
    }
}

/// Prepares (or reuses) the expanded tree of one verified blob. Returns the
/// tree identity; extraction never writes into the shared language payload.
pub(crate) fn prepare_tree(
    context: &OperationContext,
    digest: &pakro_store::Digest,
) -> Result<PreparedTree> {
    let _ = (context, digest);
    eyre::bail!("f3 shared tree preparation is not implemented")
}
