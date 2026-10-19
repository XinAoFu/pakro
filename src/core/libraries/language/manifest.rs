//! Persisted observations are descriptions, never build or removal authority.

use crate::packages::python_environment::prefix::verify::TreeInventory;
use crate::python_runtime::PythonRuntimeDescriptor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DiskIdentity {
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
pub(super) struct LanguageManifest {
    pub schema: u32,
    pub key: [u8; 32],
    pub target: [u8; 32],
    pub artifact: String,
    pub filename: String,
    pub source: String,
    pub patch_sysconfig: bool,
    pub lock: DiskIdentity,
    pub allocation: DiskIdentity,
    pub payload: DiskIdentity,
    pub entrypoint: std::path::PathBuf,
    pub inventory: TreeInventory,
    pub descriptor: PythonRuntimeDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LockRegistration {
    pub schema: u32,
    pub key: [u8; 32],
    pub lock: DiskIdentity,
}
