//! Digest-bound acquisition for shared runtime and package artifacts.
//!
//! The verified capability owns its bytes independently of mutable cache paths.
//! Source selection, transport policy and extraction belong to the caller.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use eyre::{Context, Result, bail, ensure};
use pakro_index::artifact::{
    ArtifactTransport, NetworkPolicy, SourceArtifact, download_verified_bounded,
};
use pakro_store::Digest;
use sha2::{Digest as Sha2Digest, Sha256, Sha512};

#[path = "artifact_snapshot/observed.rs"]
mod observed;
pub(crate) use observed::{ObservedArtifact, spool_observed_reader};

#[path = "artifact_snapshot/cache.rs"]
mod cache;
pub(crate) use cache::{cached_verified_artifact, publish_verified_artifact};

/// An owned, verified snapshot. Its private file cannot be mutated by consumers.
pub(crate) struct VerifiedArtifact {
    file: File,
    filename: String,
    digest: Digest,
    size: u64,
}

impl VerifiedArtifact {
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }

    pub(crate) fn digest(&self) -> Digest {
        self.digest
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

impl fmt::Debug for VerifiedArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedArtifact")
            .field("filename", &self.filename)
            .field("digest", &self.digest)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Read for VerifiedArtifact {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.file.read(output)
    }
}

impl Seek for VerifiedArtifact {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

enum Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    fn new(digest: Digest) -> Self {
        match digest {
            Digest::Sha256(_) => Self::Sha256(Sha256::new()),
            Digest::Sha512(_) => Self::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Sha512(hasher) => hasher.update(bytes),
        }
    }

    fn finish(self) -> Digest {
        match self {
            Self::Sha256(hasher) => Digest::Sha256(hasher.finalize().into()),
            Self::Sha512(hasher) => Digest::Sha512(hasher.finalize().into()),
        }
    }
}

fn validate_request(filename: &str, max_archive_bytes: u64) -> Result<()> {
    ensure!(
        max_archive_bytes > 0,
        "artifact byte limit must be positive"
    );
    ensure!(
        !filename.is_empty()
            && !matches!(filename, "." | "..")
            && !filename
                .chars()
                .any(|character| matches!(character, '/' | '\\') || character.is_control()),
        "artifact filename must be a single nonempty component"
    );
    Ok(())
}

fn open_regular_input(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a swapped final symlink or block while opening a FIFO.
        // fstat below checks the opened descriptor, not the earlier path entry.
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact input is not a regular file",
        ));
    }
    Ok(file)
}

fn verified_snapshot(
    input: File,
    source: &SourceArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<VerifiedArtifact> {
    cache::verified_snapshot_fields(
        input,
        source.filename(),
        source.digest(),
        cache_root,
        max_archive_bytes,
    )
}

fn cached_snapshot(
    source: &SourceArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<Option<VerifiedArtifact>> {
    cached_verified_artifact(
        source.filename(),
        source.digest(),
        cache_root,
        max_archive_bytes,
    )
}

fn publish_snapshot(
    artifact: &mut VerifiedArtifact,
    source: &SourceArtifact,
    cache_root: &Path,
    max_archive_bytes: u64,
) -> Result<()> {
    ensure!(
        artifact.filename() == source.filename() && artifact.digest() == source.digest(),
        "verified artifact does not match the source publication fields"
    );
    publish_verified_artifact(artifact, cache_root, max_archive_bytes)
}

/// Acquire exact bytes, revalidating cached content before returning a capability.
pub(crate) async fn acquire_verified_artifact(
    source: &SourceArtifact,
    cache_root: &Path,
    transport: &ArtifactTransport,
    policy: NetworkPolicy,
    max_archive_bytes: u64,
) -> Result<VerifiedArtifact> {
    validate_request(source.filename(), max_archive_bytes)?;
    if let Some(artifact) = cached_snapshot(source, cache_root, max_archive_bytes)? {
        return Ok(artifact);
    }
    if policy == NetworkPolicy::Offline {
        bail!(
            "offline Python runtime artifact cache is missing for {} ({})",
            source.filename(),
            source.digest()
        );
    }
    std::fs::create_dir_all(cache_root).context("creating Python runtime artifact cache")?;
    let staging = tempfile::tempdir_in(cache_root)
        .context("creating private Python runtime download staging")?;
    let downloaded =
        download_verified_bounded(transport, source, policy, staging.path(), max_archive_bytes)
            .await
            .context("downloading exact artifact")?;
    ensure!(
        downloaded.digest() == source.digest(),
        "runtime download digest identity changed"
    );
    let input =
        open_regular_input(downloaded.path()).context("opening verified artifact download")?;
    let mut artifact = verified_snapshot(input, source, cache_root, max_archive_bytes)
        .context("rechecking artifact download snapshot")?;
    publish_snapshot(&mut artifact, source, cache_root, max_archive_bytes)?;
    Ok(artifact)
}

/// Verify an owned local/built artifact stream without inventing an HTTP source.
pub(crate) fn spool_verified_reader<R: Read>(
    reader: R,
    expected_filename: &str,
    expected_digest: Digest,
    max_archive_bytes: u64,
    scratch_root: &Path,
) -> Result<VerifiedArtifact> {
    let captured = spool_snapshot(
        reader,
        expected_filename,
        max_archive_bytes,
        scratch_root,
        Hasher::new(expected_digest),
    )?;
    ensure!(
        captured.actual_digest == expected_digest,
        "artifact digest mismatch (expected {}, actual {})",
        expected_digest,
        captured.actual_digest
    );
    Ok(VerifiedArtifact {
        file: captured.file,
        filename: captured.filename,
        digest: expected_digest,
        size: captured.size,
    })
}

/// Private bytes shared by the distinct observed and expected-digest wrappers.
/// Returning this payload internally grants no caller installation authority.
struct CapturedArtifact {
    file: File,
    filename: String,
    actual_digest: Digest,
    size: u64,
}

fn spool_snapshot<R: Read>(
    mut reader: R,
    expected_filename: &str,
    max_archive_bytes: u64,
    scratch_root: &Path,
    mut hasher: Hasher,
) -> Result<CapturedArtifact> {
    validate_request(expected_filename, max_archive_bytes)?;
    std::fs::create_dir_all(scratch_root)
        .context("creating artifact snapshot scratch directory")?;
    // The anonymous file is never exposed by path or as a writable descriptor.
    let mut file =
        tempfile::tempfile_in(scratch_root).context("creating private artifact snapshot")?;
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        // Even at the exact limit, observe EOF or one overflow byte. A reader
        // error at that point is an error, never successful truncated input.
        let remaining = max_archive_bytes - size;
        let request = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let count = reader
            .read(&mut buffer[..request])
            .map_err(|error| eyre::eyre!("artifact reader I/O failed ({:?})", error.kind()))?;
        ensure!(
            count <= request,
            "artifact reader returned an invalid byte count"
        );
        if count == 0 {
            break;
        }
        let next_size = size
            .checked_add(count as u64)
            .filter(|value| *value <= max_archive_bytes)
            .ok_or_else(|| eyre::eyre!("artifact exceeds byte limit {max_archive_bytes}"))?;
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
        size = next_size;
    }
    let actual_digest = hasher.finish();
    file.flush()?;
    file.rewind()?;
    Ok(CapturedArtifact {
        file,
        filename: expected_filename.to_owned(),
        actual_digest,
        size,
    })
}

#[cfg(test)]
#[path = "artifact_snapshot/tests.rs"]
mod tests;
