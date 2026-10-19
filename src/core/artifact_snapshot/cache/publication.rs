//! Publication of an already verified data owner, without source authority.

use std::io::{self, Read, Seek, Write};
use std::path::Path;

use eyre::{Context, ContextCompat, Result, ensure};

use super::super::{VerifiedArtifact, validate_request};
use super::{cache_path_fields, cached_verified_artifact};

pub(crate) fn publish_verified_artifact(
    artifact: &mut VerifiedArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<()> {
    publish_with_hook(artifact, cache_root, max_archive_bytes, |_| {})
}

fn publish_with_hook(
    artifact: &mut VerifiedArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
    before_persist: impl FnOnce(&Path),
) -> Result<()> {
    publish_with_validation_hooks(
        artifact,
        cache_root,
        max_archive_bytes,
        before_persist,
        |_| {},
        |_, _| {},
    )
}

fn publish_with_validation_hooks(
    artifact: &mut VerifiedArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
    before_persist: impl FnOnce(&Path),
    before_existing_validation: impl FnOnce(&Path),
    after_existing_validation: impl FnOnce(&Path, &VerifiedArtifact),
) -> Result<()> {
    let result = publish(
        artifact,
        cache_root,
        max_archive_bytes,
        before_persist,
        before_existing_validation,
        after_existing_validation,
    );
    let rewind = artifact
        .rewind()
        .context("rewinding verified artifact after cache publication");
    result.and(rewind)
}

fn publish(
    artifact: &mut VerifiedArtifact,
    cache_root: &Path,
    maximum: u64,
    before_persist: impl FnOnce(&Path),
    before_existing_validation: impl FnOnce(&Path),
    after_existing_validation: impl FnOnce(&Path, &VerifiedArtifact),
) -> Result<()> {
    validate_request(artifact.filename(), maximum)?;
    let size = artifact.size();
    ensure!(
        size <= maximum,
        "verified artifact exceeds cache publication byte limit"
    );
    let copy_limit = size
        .checked_add(1)
        .context("verified artifact size exceeds cache publication limits")?;
    let filename = artifact.filename().to_owned();
    let digest = artifact.digest();
    // Reuse the one strict cache validator. Cache bytes are never repaired or
    // trusted merely because their content-addressed pathname exists.
    if let Some(existing) = cached_verified_artifact(&filename, digest, cache_root, maximum)? {
        ensure!(
            existing.size() == size,
            "verified artifact cache size disagrees"
        );
        return Ok(());
    }
    let path = cache_path_fields(&filename, digest, cache_root);
    let parent = path.parent().context("artifact cache parent is missing")?;
    artifact.rewind()?;
    std::fs::create_dir_all(parent).context("creating artifact cache key")?;
    let mut publication = tempfile::NamedTempFile::new_in(parent)
        .context("creating artifact cache publication file")?;
    let copied = io::copy(
        &mut (&mut *artifact).take(copy_limit),
        publication.as_file_mut(),
    )?;
    ensure!(
        copied == size && copied <= maximum,
        "verified artifact snapshot size changed"
    );
    publication.flush()?;
    publication.as_file().sync_all()?;
    before_persist(publication.path());
    match publication.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let loser_path = error.file.path().to_owned();
            drop(error);
            before_existing_validation(&loser_path);
            let existing = cached_verified_artifact(&filename, digest, cache_root, maximum)?
                .context("concurrent artifact cache publication disappeared")?;
            after_existing_validation(&loser_path, &existing);
            ensure!(
                existing.size() == size,
                "concurrent artifact cache size disagrees"
            );
        }
        Err(error) => return Err(error.error).context("publishing verified artifact cache"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "publication/tests.rs"]
mod tests;
