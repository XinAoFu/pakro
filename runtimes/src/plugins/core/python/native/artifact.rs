//! Python runtime adapter for the shared digest-bound artifact snapshot.

use std::path::Path;

use eyre::Result;
use pakro_index::artifact::{ArtifactTransport, NetworkPolicy, SourceArtifact};

pub(in super::super) type VerifiedRuntimeArtifact = crate::artifact_snapshot::VerifiedArtifact;

/// Keep verification/cache logic shared and bound native runtime archive bytes.
pub(in super::super) async fn acquire_runtime_artifact(
    source: &SourceArtifact,
    cache_root: &Path,
    transport: &ArtifactTransport,
    policy: NetworkPolicy,
) -> Result<VerifiedRuntimeArtifact> {
    crate::artifact_snapshot::acquire_verified_artifact(
        source,
        cache_root,
        transport,
        policy,
        4 * 1024 * 1024 * 1024,
    )
    .await
}
