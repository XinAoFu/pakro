//! Stable project identity, separate from any path or location binding.

use eyre::Result;
use uuid::Uuid;

use super::location::ProjectLocation;
use super::snapshot::ObservedProjectInputs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProjectIdentity(Uuid);

impl ProjectIdentity {
    /// A stable identity derived from the location binding of this one
    /// observation: the same directory observed again yields the same
    /// identity, without registering anything. The identity is not the path;
    /// moving the directory changes the binding that must be re-observed.
    pub(crate) fn from_observation(observed: &ObservedProjectInputs) -> Result<Self> {
        let location = ProjectLocation::bind(observed)?;
        Ok(Self(stable_root_identity(location.root())))
    }

    pub(crate) fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Rebuilds the identity handle from a committed catalog record.
    pub(crate) fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// UUIDv5-shaped digest of the canonical root, so identities sort and print
/// as UUIDs while depending only on the bound location.
fn stable_root_identity(root: &std::path::Path) -> Uuid {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new_with_prefix(b"pakro.project-root.v1")
        .chain_update(root.as_os_str().as_encoded_bytes())
        .finalize();
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("sha256 yields 16 bytes");
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Builder::from_bytes(bytes).into_uuid()
}
