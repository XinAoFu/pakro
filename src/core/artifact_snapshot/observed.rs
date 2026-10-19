//! Local byte observations, never an expected-digest or installation authority.
//!
//! The shared bounded spool owns the returned anonymous bytes. Observation
//! records an actual digest and never claims an independent expected match.

use super::*;

pub(crate) struct ObservedArtifact {
    file: File,
    filename: String,
    observed_sha256: [u8; 32],
    size: u64,
}
impl ObservedArtifact {
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }
    pub(crate) fn observed_sha256(&self) -> [u8; 32] {
        self.observed_sha256
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}
impl fmt::Debug for ObservedArtifact {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ObservedArtifact")
            .field("filename", &self.filename)
            .field("observed_sha256", &self.observed_sha256)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}
impl Read for ObservedArtifact {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.file.read(output)
    }
}
impl Seek for ObservedArtifact {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

pub(crate) fn spool_observed_reader<R: Read>(
    reader: R,
    filename: &str,
    max_archive_bytes: u64,
    scratch_root: &Path,
) -> Result<ObservedArtifact> {
    let captured = super::spool_snapshot(
        reader,
        filename,
        max_archive_bytes,
        scratch_root,
        Hasher::Sha256(Sha256::new()),
    )?;
    let Digest::Sha256(observed_sha256) = captured.actual_digest else {
        bail!("observed artifact used an unexpected hash algorithm")
    };
    Ok(ObservedArtifact {
        file: captured.file,
        filename: captured.filename,
        observed_sha256,
        size: captured.size,
    })
}

#[cfg(test)]
#[path = "observed/tests.rs"]
mod tests;
