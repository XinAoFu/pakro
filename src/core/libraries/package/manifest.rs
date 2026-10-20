//! Persistent facts. These records describe content and never grant IO rights.
use crate::packages::python_environment::prefix::verify::TreeInventory;
use crate::programs::OperationContext;
use eyre::{Result, ensure};
use pakro_store::control::{ControlLimits, ControlNamespace, ReadStore, RootLimits};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StorageManifest {
    pub schema: u32,
    pub key: [u8; 32],
    pub allocation: String,
    pub lock: [u64; 2],
    pub root_identity: [u64; 2],
    pub inventory: TreeInventory,
}
impl StorageManifest {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unsupported package storage manifest");
        ensure!(
            self.allocation.len() == 34
                && self.allocation.starts_with("g-")
                && self.allocation.as_bytes()[2..]
                    .iter()
                    .all(u8::is_ascii_hexdigit),
            "invalid package allocation"
        );
        for pair in [self.lock, self.root_identity] {
            let actual = super::lease::identity(pair);
            ensure!(
                [actual.device as u64, actual.inode as u64] == pair,
                "package physical identity exceeds this platform's representation"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageFacts {
    pub key: [u8; 32],
    pub name: String,
    pub version: String,
    pub artifact: String,
    pub source: String,
    pub dependencies: Vec<[u8; 32]>,
}
pub(crate) const PACKAGE_FACTS_FILE: &str = ".pakro-package-facts.json";

pub(crate) fn package_facts_bytes(
    facts: &PackageFacts,
    language: &[u8; 32],
    filename: &str,
) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&(1u32, facts, language, filename))?;
    ensure!(
        bytes.len() as u64 <= ControlLimits::default().max_file_bytes,
        "package facts exceed their bounded metadata budget"
    );
    Ok(bytes)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageManifest {
    pub facts: PackageFacts,
    pub language: [u8; 32],
    pub filename: String,
    pub storage: StorageManifest,
}
impl PackageManifest {
    pub(crate) fn name(&self) -> &str {
        &self.facts.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.facts.version
    }
    pub(crate) fn artifact(&self) -> Result<pakro_store::Digest> {
        Ok(self.facts.artifact.parse()?)
    }
    pub(crate) fn key(&self) -> [u8; 32] {
        self.facts.key
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlobManifest {
    pub artifact: String,
    pub filename: String,
    pub storage: StorageManifest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutManifest {
    pub language: [u8; 32],
    pub graph: [u8; 32],
    pub packages: Vec<[u8; 32]>,
    pub storage: StorageManifest,
}

pub(crate) fn store_path(context: &OperationContext) -> std::path::PathBuf {
    context.requested_data_root().join("packages")
}
pub(crate) fn package_name(key: &[u8; 32]) -> String {
    format!("package-{}.json", hex::encode(key))
}
pub(crate) fn layout_name(key: &[u8; 32]) -> String {
    format!("layout-{}.json", hex::encode(key))
}
pub(crate) fn blob_key(digest: &pakro_store::Digest) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(digest.to_string().as_bytes()).into()
}
pub(crate) fn blob_name(digest: &pakro_store::Digest) -> String {
    format!("blob-{}.json", hex::encode(blob_key(digest)))
}
pub(crate) fn read_json<T: DeserializeOwned>(
    context: &OperationContext,
    name: &str,
) -> Result<Option<T>> {
    context.checkpoint()?;
    let Some(read) = ReadStore::open_readonly(
        &store_path(context),
        super::lease::uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(None);
    };
    let control = read.control(ControlLimits::default())?;
    control
        .read(ControlNamespace::Catalog, name)?
        .map(|s| serde_json::from_slice(s.bytes()).map_err(Into::into))
        .transpose()
}
pub(crate) fn read_manifest(
    context: &OperationContext,
    digest: &pakro_store::Digest,
) -> Result<Option<PackageManifest>> {
    let mut matching = enumerate_published(context)?
        .into_iter()
        .filter(|manifest| manifest.facts.artifact == digest.to_string());
    let first = matching.next();
    ensure!(
        matching.next().is_none(),
        "artifact has several package instances; select an exact package key"
    );
    Ok(first)
}
pub(crate) fn read_package(
    context: &OperationContext,
    key: &[u8; 32],
) -> Result<Option<PackageManifest>> {
    read_json(context, &package_name(key))
}
pub(crate) fn read_blob_manifest(
    context: &OperationContext,
    digest: &pakro_store::Digest,
) -> Result<Option<BlobManifest>> {
    read_json(context, &blob_name(digest))
}
pub(crate) fn enumerate_published(context: &OperationContext) -> Result<Vec<PackageManifest>> {
    context.checkpoint()?;
    let Some(read) = ReadStore::open_readonly(
        &store_path(context),
        super::lease::uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(Vec::new());
    };
    let control = read.control(ControlLimits::default())?;
    let mut result = Vec::new();
    for name in control.list_names(ControlNamespace::Catalog, 65536, 8 * 1024 * 1024)? {
        if !name.starts_with("package-") {
            continue;
        }
        let snapshot = control
            .read(ControlNamespace::Catalog, &name)?
            .ok_or_else(|| eyre::eyre!("package manifest disappeared"))?;
        let manifest: PackageManifest = serde_json::from_slice(snapshot.bytes())?;
        manifest.storage.validate()?;
        ensure!(
            name == package_name(&manifest.facts.key) && manifest.storage.key == manifest.facts.key,
            "package manifest identity differs"
        );
        result.push(manifest);
    }
    // Never retain catalog while waiting for actual object protection.
    drop(control);
    for manifest in &result {
        let lease = super::lease::acquire_package(context, manifest.key())?
            .ok_or_else(|| eyre::eyre!("package disappeared while reading its bound facts"))?;
        ensure!(
            serde_json::to_vec(lease.manifest())? == serde_json::to_vec(manifest)?,
            "package facts changed during enumeration"
        );
    }
    Ok(result)
}
