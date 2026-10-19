//! Persistent manifest facts of published package content.
//!
//! The manifest records the exact name/version/source identity, the digest of
//! the underlying artifact, and the tree identity. It is descriptive: reading
//! it grants no write or removal authority.

use eyre::Result;

use crate::programs::OperationContext;

/// Reads the manifest facts of one published package instance, or reports
/// its explicit absence. A damaged manifest is an error, never "missing".
pub(crate) fn read_manifest(
    context: &OperationContext,
    digest: &pakro_store::Digest,
) -> Result<Option<PackageManifest>> {
    let _ = (context, digest);
    eyre::bail!("f3 package manifest reading is not implemented")
}

/// The persisted facts of one package instance.
pub(crate) struct PackageManifest {
    name: String,
    version: String,
    artifact: pakro_store::Digest,
}

impl PackageManifest {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn artifact(&self) -> &pakro_store::Digest {
        &self.artifact
    }
}
