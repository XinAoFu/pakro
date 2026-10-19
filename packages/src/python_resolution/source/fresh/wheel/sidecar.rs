//! Optional advertised metadata is checked against the actual wheel bytes.
//!
//! Offline evidence records local wheel-byte verification without claiming a
//! sidecar request. Every returned owner remains an unpublished candidate.

use super::*;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreMetadataEvidence {
    WholeWheel,
    WheelBytesMatchAdvertisedHash { digest: Digest },
    FetchedSidecarMatchesWheel { digest: Digest, size: u64 },
}

pub(crate) struct CheckedFreshWheel<'lock> {
    wheel: FreshWheelCandidate<'lock>,
    evidence: CoreMetadataEvidence,
}
impl CheckedFreshWheel<'_> {
    pub(crate) fn wheel(&self) -> &FreshWheelCandidate<'_> {
        &self.wheel
    }
    pub(crate) fn evidence(&self) -> CoreMetadataEvidence {
        self.evidence
    }
    pub(crate) fn recheck(&self) -> FreshSidecarResult<()> {
        self.wheel.recheck().map_err(|_| FreshSidecarError::Revoked)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> FreshSidecarResult<()> {
        self.wheel
            .recheck_for(owner)
            .map_err(|_| FreshSidecarError::Revoked)
    }
}
impl fmt::Debug for CheckedFreshWheel<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("CheckedFreshWheel")
            .field("wheel", &self.wheel)
            .field("evidence", &self.evidence)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SidecarLimits {
    pub(crate) transfer: SourceFileLimits,
    pub(crate) max_spool_bytes: u64,
    pub(crate) max_identity_bytes: usize,
    /// One caller-owned absolute budget, shared by hops, body and local checks.
    pub(crate) deadline: Instant,
}

pub(crate) type FreshSidecarResult<T> = Result<T, FreshSidecarError>;
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum FreshSidecarError {
    #[error("metadata sidecar limits must be positive and sufficient")]
    InvalidLimits,
    #[error("metadata sidecar deadline expired")]
    Deadline,
    #[error("metadata sidecar limit exceeded: {0:?}")]
    LimitExceeded(SimpleClientLimitKind),
    #[error("metadata sidecar identity exceeds its byte budget")]
    IdentityBudget,
    #[error("metadata sidecar slot is no longer current")]
    Revoked,
    #[error("metadata sidecar request identity is inconsistent")]
    IdentityMismatch,
    #[error("metadata sidecar acquisition failed: {0}")]
    Transport(SimpleClientError),
    #[error("metadata sidecar could not be verified against its advertised digest")]
    DigestMismatch,
    #[error("metadata sidecar differs from the complete actual wheel metadata")]
    MetadataMismatch,
    #[error("metadata sidecar I/O failed")]
    Io,
}

fn deadline(limit: Instant) -> FreshSidecarResult<()> {
    if Instant::now() >= limit {
        return Err(FreshSidecarError::Deadline);
    }
    Ok(())
}
fn active(wheel: &FreshWheelCandidate<'_>, limit: Instant) -> FreshSidecarResult<()> {
    deadline(limit)?;
    wheel.recheck().map_err(|_| FreshSidecarError::Revoked)?;
    deadline(limit)
}
fn validate_limits(
    wheel: &FreshWheelCandidate<'_>,
    limits: SidecarLimits,
) -> FreshSidecarResult<()> {
    if limits.transfer.max_wire_bytes == 0
        || limits.transfer.max_header_bytes == 0
        || limits.transfer.max_headers == 0
        || limits.max_spool_bytes == 0
        || limits.max_identity_bytes == 0
        || limits
            .transfer
            .max_wire_bytes
            .checked_mul(2)
            .is_none_or(|bytes| bytes > limits.max_spool_bytes)
    {
        return Err(FreshSidecarError::InvalidLimits);
    }
    if u64::try_from(wheel.manifest().core_metadata().raw().len())
        .map_or(true, |bytes| bytes > limits.transfer.max_wire_bytes)
    {
        return Err(FreshSidecarError::LimitExceeded(
            SimpleClientLimitKind::WireBytes,
        ));
    }
    Ok(())
}
fn charge(used: &mut usize, text: &str, maximum: usize) -> FreshSidecarResult<()> {
    *used = used
        .checked_add(text.len())
        .filter(|size| *size <= maximum)
        .ok_or(FreshSidecarError::IdentityBudget)?;
    Ok(())
}

/// The generic verifier keeps its one copy/hash loop. This bounded reader adds
/// the caller's absolute budget without changing expected-digest semantics.
struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}
impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.inner.read(output)
    }
}
fn verify<R: Read>(
    reader: R,
    filename: &str,
    digest: Digest,
    limits: SidecarLimits,
    scratch_root: &Path,
) -> FreshSidecarResult<VerifiedArtifact> {
    deadline(limits.deadline)?;
    let artifact = spool_verified_reader(
        DeadlineReader {
            inner: reader,
            deadline: limits.deadline,
        },
        filename,
        digest,
        limits.transfer.max_wire_bytes,
        scratch_root,
    )
    .map_err(|_| {
        if Instant::now() >= limits.deadline {
            FreshSidecarError::Deadline
        } else {
            FreshSidecarError::DigestMismatch
        }
    })?;
    deadline(limits.deadline)?;
    Ok(artifact)
}
fn compare_raw(
    artifact: &mut VerifiedArtifact,
    raw: &[u8],
    limit: Instant,
) -> FreshSidecarResult<()> {
    deadline(limit)?;
    if artifact.size() != raw.len() as u64 {
        return Err(FreshSidecarError::MetadataMismatch);
    }
    artifact.rewind().map_err(|_| FreshSidecarError::Io)?;
    let mut offset = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        deadline(limit)?;
        let count = match artifact.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            value => value.map_err(|_| FreshSidecarError::Io)?,
        };
        if count == 0 {
            break;
        }
        let end = offset
            .checked_add(count)
            .filter(|end| *end <= raw.len())
            .ok_or(FreshSidecarError::MetadataMismatch)?;
        if buffer[..count] != raw[offset..end] {
            return Err(FreshSidecarError::MetadataMismatch);
        }
        offset = end;
    }
    if offset != raw.len() {
        return Err(FreshSidecarError::MetadataMismatch);
    }
    deadline(limit)
}

pub(crate) async fn verify_advertised_core_metadata<'lock>(
    wheel: FreshWheelCandidate<'lock>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SidecarLimits,
    scratch_root: &Path,
) -> FreshSidecarResult<CheckedFreshWheel<'lock>> {
    validate_limits(&wheel, limits)?;
    active(&wheel, limits.deadline)?;
    // Inspect only already-bounded facts; no raw locator is logged or written
    // to the portable pin. Charge original URI before cloning a metadata URL.
    let file = wheel.file_facts();
    let mut identity_bytes = 0;
    for value in [
        file.filename(),
        file.locator().url().as_str(),
        session
            .credential_ref
            .map_or("", |reference| reference.as_str()),
    ] {
        charge(&mut identity_bytes, value, limits.max_identity_bytes)?;
    }
    let expected = match file.core_metadata() {
        pakro_index::simple::CoreMetadataAvailability::Available { hashes } => {
            hashes.preferred_digest()
        }
        _ => None,
    };
    let Some(expected) = expected else {
        active(&wheel, limits.deadline)?;
        return Ok(CheckedFreshWheel {
            wheel,
            evidence: CoreMetadataEvidence::WholeWheel,
        });
    };
    let filename_bytes = file
        .filename()
        .len()
        .checked_add(".metadata".len())
        .filter(|size| *size <= limits.max_identity_bytes)
        .ok_or(FreshSidecarError::IdentityBudget)?;
    let mut filename = String::new();
    filename
        .try_reserve_exact(filename_bytes)
        .map_err(|_| FreshSidecarError::IdentityBudget)?;
    filename.push_str(file.filename());
    filename.push_str(".metadata");
    charge(&mut identity_bytes, &filename, limits.max_identity_bytes)?;
    let raw = wheel.manifest().core_metadata().raw();
    let evidence = if network == NetworkPolicy::Offline {
        // Verify the actual immutable wheel observation locally. Do not claim
        // this branch contacted the separately hosted metadata sidecar.
        let mut verified = verify(
            io::Cursor::new(raw),
            &filename,
            expected,
            limits,
            scratch_root,
        )?;
        compare_raw(&mut verified, raw, limits.deadline)?;
        CoreMetadataEvidence::WheelBytesMatchAdvertisedHash { digest: expected }
    } else {
        let metadata = file
            .locator()
            .metadata_locator()
            .map_err(|_| FreshSidecarError::IdentityMismatch)?;
        charge(
            &mut identity_bytes,
            metadata.url().as_str(),
            limits.max_identity_bytes,
        )?;
        let physical = session
            .transport
            .request_url(metadata.url())
            .map_err(FreshSidecarError::Transport)?;
        charge(
            &mut identity_bytes,
            physical.as_str(),
            limits.max_identity_bytes,
        )?;
        let fingerprint = session.transport.fingerprint();
        active(&wheel, limits.deadline)?;
        std::fs::create_dir_all(scratch_root).map_err(|_| FreshSidecarError::Io)?;
        let mut staging = tempfile::tempfile_in(scratch_root).map_err(|_| FreshSidecarError::Io)?;
        let transfer = tokio::time::timeout_at(
            tokio::time::Instant::from_std(limits.deadline),
            client.copy_source_file_at_deadline(
                SourceFileSession {
                    credential_ref: session.credential_ref,
                    auth_scope: session.auth_scope,
                    credentials: session.credentials,
                    auth_origins: session.auth_origins,
                    transport: session.transport,
                },
                metadata.url(),
                network,
                limits.transfer,
                &mut staging,
                limits.deadline,
            ),
        )
        .await
        .map_err(|_| FreshSidecarError::Deadline)?
        .map_err(|error| {
            if error == SimpleClientError::Timeout {
                FreshSidecarError::Deadline
            } else {
                FreshSidecarError::Transport(error)
            }
        })?;
        if transfer.initial_canonical_url() != metadata.url()
            || transfer.initial_request_url() != &physical
            || transfer.auth_scope() != session.auth_scope
            || transfer.transport_fingerprint() != fingerprint
        {
            return Err(FreshSidecarError::IdentityMismatch);
        }
        for url in [transfer.canonical_url(), transfer.request_url()] {
            charge(&mut identity_bytes, url.as_str(), limits.max_identity_bytes)?;
        }
        active(&wheel, limits.deadline)?;
        staging.rewind().map_err(|_| FreshSidecarError::Io)?;
        let mut verified = verify(&mut staging, &filename, expected, limits, scratch_root)?;
        if verified.size() != transfer.size() {
            return Err(FreshSidecarError::MetadataMismatch);
        }
        compare_raw(&mut verified, raw, limits.deadline)?;
        CoreMetadataEvidence::FetchedSidecarMatchesWheel {
            digest: expected,
            size: verified.size(),
        }
    };
    active(&wheel, limits.deadline)?;
    Ok(CheckedFreshWheel { wheel, evidence })
}

#[cfg(test)]
#[path = "sidecar/tests.rs"]
mod tests;

// To be appended in CheckedFreshWheel's own child module after Root release.
pub(in crate::packages::python_resolution::source) struct RetainedFreshWheel<'lock> {
    slot: FreshFileSlot<'lock>,
    pin: ContentPin,
    identity: RegistryFileIdentity,
    facts: PythonDependencyMetadataFacts,
    evidence: CoreMetadataEvidence,
}
impl<'lock> CheckedFreshWheel<'lock> {
    pub(in crate::packages::python_resolution::source) fn into_build_evidence(
        self,
    ) -> (InspectedWheelArchive, RetainedFreshWheel<'lock>) {
        let CheckedFreshWheel { wheel, evidence } = self;
        let FreshWheelCandidate {
            slot,
            pin,
            identity,
            archive,
            facts,
        } = wheel;
        (
            archive,
            RetainedFreshWheel {
                slot,
                pin,
                identity,
                facts,
                evidence,
            },
        )
    }
}
impl RetainedFreshWheel<'_> {
    pub(in crate::packages::python_resolution::source) fn pin(&self) -> &ContentPin {
        &self.pin
    }
    pub(in crate::packages::python_resolution::source) fn facts(
        &self,
    ) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(in crate::packages::python_resolution::source) fn recheck_for(
        &self,
        root: &BoundPythonLock,
    ) -> FreshSidecarResult<()> {
        self.slot
            .recheck_for(root)
            .map_err(|_| FreshSidecarError::Revoked)
    }
}
