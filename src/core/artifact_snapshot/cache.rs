//! Expected-digest cache lookup only: no locator, network or publication authority.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, ensure};
use pakro_store::Digest;

use super::{VerifiedArtifact, open_regular_input, spool_verified_reader, validate_request};

#[path = "cache/publication.rs"]
mod publication;
pub(crate) use publication::publish_verified_artifact;

pub(super) fn cache_path_fields(filename: &str, digest: Digest, cache_root: &Path) -> PathBuf {
    cache_root
        .join("artifacts")
        .join(digest.to_string())
        .join(filename)
}

pub(super) fn verified_snapshot_fields(
    input: File,
    filename: &str,
    digest: Digest,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<VerifiedArtifact> {
    let metadata = input.metadata()?;
    ensure!(metadata.is_file(), "artifact input is not a regular file");
    ensure!(
        metadata.len() <= max_archive_bytes,
        "artifact exceeds byte limit {max_archive_bytes}"
    );
    spool_verified_reader(input, filename, digest, max_archive_bytes, cache_root)
}

/// Revalidate the exact expected-digest cache tuple without consulting a source.
pub(crate) fn cached_verified_artifact(
    filename: &str,
    digest: Digest,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<Option<VerifiedArtifact>> {
    validate_request(filename, max_archive_bytes)?;
    let path = cache_path_fields(filename, digest, cache_root);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading artifact cache"),
    };
    ensure!(
        metadata.is_file(),
        "artifact cache entry is not a regular file"
    );
    let file = match open_regular_input(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening artifact cache"),
    };
    verified_snapshot_fields(file, filename, digest, cache_root, max_archive_bytes)
        .map(Some)
        .context("artifact cache digest verification failed")
}

#[cfg(test)]
#[path = "cache/tests.rs"]
mod tests;
