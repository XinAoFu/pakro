//! Exact verified archives are published once under controlled shared storage.
use super::manifest::{self, BlobManifest};
use crate::artifact_snapshot::VerifiedArtifact;
use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
use crate::programs::OperationContext;
use eyre::{Result, ensure};
use std::io::{Read, Seek, Write};

pub(crate) fn publish_blob(
    context: &OperationContext,
    artifact: &mut VerifiedArtifact,
) -> Result<pakro_store::Digest> {
    context.checkpoint()?;
    let digest = artifact.digest();
    if let Some(existing) = manifest::read_blob_manifest(context, &digest)? {
        ensure!(
            existing.artifact == digest.to_string(),
            "blob identity differs"
        );
        super::lease::acquire_object(
            context,
            &manifest::blob_name(&digest),
            &existing.storage,
            &serde_json::to_vec(&existing)?,
        )?;
        return Ok(digest);
    }
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package library writes"
    );
    let store = EnvironmentStore::open_shared_writer(
        &manifest::store_path(context),
        super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )?;
    let mut prefix = store.allocate()?;
    artifact.rewind()?;
    let mut written = 0u64;
    {
        let mut file = prefix.create_file_exclusive(std::path::Path::new("archive"), 0o600)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            context.checkpoint()?;
            let count = artifact.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written = written
                .checked_add(count as u64)
                .ok_or_else(|| eyre::eyre!("blob size overflow"))?;
            ensure!(written <= artifact.size(), "blob exceeds verified size");
            file.write_all(&buffer[..count])?;
        }
        file.finish()?;
    }
    ensure!(
        written == artifact.size(),
        "blob differs from verified size"
    );
    let storage = super::tree::storage_for(&prefix, manifest::blob_key(&digest))?;
    let value = BlobManifest {
        artifact: digest.to_string(),
        filename: artifact.filename().into(),
        storage,
    };
    let publication = prefix.publish_shared(
        &manifest::blob_name(&digest),
        &serde_json::to_vec(&value)?,
        &value.storage.inventory,
    );
    if let Err(error) = publication {
        if !error.is::<crate::packages::python_environment::prefix::SharedPublicationConflict>() {
            return Err(error);
        }
        let Some(winner) = manifest::read_blob_manifest(context, &digest)? else {
            return Err(error);
        };
        ensure!(
            winner.artifact == value.artifact,
            "conflicting blob publication"
        );
        super::lease::acquire_object(
            context,
            &manifest::blob_name(&digest),
            &winner.storage,
            &serde_json::to_vec(&winner)?,
        )?;
    }
    context.checkpoint()?;
    Ok(digest)
}
#[cfg(test)]
mod tests;
