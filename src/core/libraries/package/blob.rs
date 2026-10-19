//! Verified wheel blob storage under the managed data root.
//!
//! A blob is the exact verified archive bytes keyed by digest. Publishing is
//! idempotent on content: the same digest reuses the stored blob, and a
//! conflicting blob for the same digest is refused, never overwritten.

use eyre::Result;

use crate::artifact_snapshot::VerifiedArtifact;
use crate::programs::OperationContext;

/// Publishes one verified artifact into the shared blob store, returning the
/// canonical digest identity. Reuse keeps the existing bytes untouched.
pub(crate) fn publish_blob(
    context: &OperationContext,
    artifact: &VerifiedArtifact,
) -> Result<pakro_store::Digest> {
    let _ = (context, artifact);
    eyre::bail!("f3 shared blob publication is not implemented")
}

#[cfg(test)]
mod tests;
