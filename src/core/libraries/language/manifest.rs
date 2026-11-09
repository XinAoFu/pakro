//! Persisted observations are descriptions, never build or removal authority.

use crate::packages::python_environment::prefix::verify::TreeInventory;
use crate::python_runtime::PythonRuntimeDescriptor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::libraries) struct DiskIdentity {
    pub device: u64,
    pub inode: u64,
}
impl DiskIdentity {
    pub(super) fn validate(&self) -> eyre::Result<()> {
        let identity = self.file_identity();
        eyre::ensure!(
            identity.device as u64 == self.device && identity.inode as u64 == self.inode,
            "language manifest physical identity exceeds this platform's representation"
        );
        Ok(())
    }
    pub(super) fn from_file_identity(identity: pakro_store::control::FileIdentity) -> Self {
        Self {
            device: identity.device as u64,
            inode: identity.inode as u64,
        }
    }
    pub(super) fn file_identity(&self) -> pakro_store::control::FileIdentity {
        pakro_store::control::FileIdentity {
            device: self.device as _,
            inode: self.inode as _,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::libraries) struct LanguageManifest {
    pub schema: u32,
    pub key: [u8; 32],
    pub target: [u8; 32],
    pub artifact: String,
    pub filename: String,
    pub source: String,
    pub patch_sysconfig: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_build: Option<super::source::SourcePythonBuildRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_layout: Option<super::source::SourcePythonLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conda: Option<rattler_conda_types::RepoDataRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_dependencies: Vec<[u8; 32]>,
    pub lock: DiskIdentity,
    pub allocation: DiskIdentity,
    pub payload: DiskIdentity,
    pub entrypoint: std::path::PathBuf,
    pub inventory: TreeInventory,
    pub descriptor: PythonRuntimeDescriptor,
}

/// Validate complete published facts without acquiring a lease or walking the
/// payload. A protected language still needs this schema/source/location gate.
/// The returned identity is descriptive and never grants filesystem rights.
pub(crate) fn validate_published_metadata(
    bytes: &[u8],
    catalog_name: &str,
    library_root: &std::path::Path,
) -> eyre::Result<([u8; 32], Vec<[u8; 32]>)> {
    let manifest: LanguageManifest = serde_json::from_slice(bytes)?;
    let spec = super::repository::LanguageRepository::spec_from_manifest(&manifest)?;
    super::publication::validate_manifest(&spec, &manifest)?;
    eyre::ensure!(
        catalog_name == super::repository::manifest_name(&spec),
        "language manifest filename differs from its complete identity"
    );
    super::publication::validate_manifest_location(
        &manifest,
        &library_root.join(spec.key().filename()).join("payload"),
    )?;
    Ok((manifest.key, manifest.runtime_dependencies))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::libraries) struct LockRegistration {
    pub schema: u32,
    pub key: [u8; 32],
    pub lock: DiskIdentity,
}
