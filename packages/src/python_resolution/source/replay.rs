//! Exact expected-pin replay remains owned with its actual project-lock slot.
//! A cache hit needs no Simple observation or fabricated request locator.

use super::*;

pub(crate) mod metadata;
pub(crate) mod selected;
use crate::artifact_snapshot::{cached_verified_artifact, publish_verified_artifact};
use crate::packages::python_resolution::lock_model::{
    ContentPin, LockModelLimits, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    canonical_registry_base_fingerprint,
};
use crate::packages::python_resolution::lock_session::{BoundPythonLock, ExistingFileSlot};
use pakro_index::simple::SimpleFile;
use pakro_resolver::solve::{Pep440, VersionAdapter};
pub(crate) use selected::acquire_selected_existing_registry_file;
use std::time::Instant;

pub(crate) type LockedReplayResult<T> = Result<T, LockedReplayError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum LockedReplayError {
    #[error("locked replay limits must be positive and sufficient")]
    InvalidLimits,
    #[error("locked replay input exceeds its current byte or identity limits")]
    InputLimits,
    #[error("locked file slot is no longer current")]
    Revoked,
    #[error("locked replay is missing its exact artifact input while offline")]
    MissingInputs,
    #[error("locked artifact cache is invalid")]
    Cache,
    #[error("configured registry differs from the actual locked file")]
    RegistryMismatch,
    #[error("the exact locked file is absent from its registry project")]
    FileMissing,
    #[error("mixed lock is missing its portable registry artifact URL")]
    MissingLocator,
    #[error("registry file identity differs from the actual locked file")]
    FileIdentity,
    #[error("registry page digest conflicts with the locked expected digest")]
    HashConflict,
    #[error("locked replay bytes failed expected digest verification")]
    ByteIdentity,
    #[error("locked replay actual size differs from the existing pin")]
    SizeMismatch,
    #[error("locked replay deadline expired")]
    Deadline,
    #[error("locked registry acquisition failed: {0}")]
    Source(SourceResolutionError),
    #[error("verified locked artifact cache publication failed")]
    CachePublication,
    #[error("selected project does not belong to this complete source context")]
    ProjectContext,
}

pub(crate) struct LockedReplayContext<'ctx> {
    pub(crate) client: &'ctx SimpleIndexClient,
    pub(crate) project_session: PythonSourceSession<'ctx>,
    pub(crate) file_session: SourceFileSession<'ctx>,
    pub(crate) policy: SimpleFetchPolicy,
    pub(crate) project_limits: SimpleClientLimits,
    pub(crate) source_limits: SourceObservationLimits,
    pub(crate) lock_limits: LockModelLimits,
    pub(crate) cache_root: &'ctx Path,
    pub(crate) scratch_root: &'ctx Path,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockedReplayOrigin {
    Cache,
    Registry,
}
enum ReplayProvenance {
    Cache,
    SelectedCache {
        project: Arc<BoundProjectData>,
        file_index: usize,
    },
    Registry(RegistryFileIdentity),
    LockedUrl,
}

/// Immutable expected bytes and their still-live lock proof. No parts,
/// observed promotion, fresh-pin callback or graph/publication conversion.
pub(crate) struct LockedRegistryArtifact<'lock> {
    slot: ExistingFileSlot<'lock>,
    artifact: VerifiedArtifact,
    provenance: ReplayProvenance,
}
impl LockedRegistryArtifact<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        self.slot.pin()
    }
    pub(crate) fn filename(&self) -> &str {
        self.artifact.filename()
    }
    pub(crate) fn digest(&self) -> Digest {
        self.artifact.digest()
    }
    pub(crate) fn size(&self) -> u64 {
        self.artifact.size()
    }
    pub(crate) fn origin(&self) -> LockedReplayOrigin {
        match self.provenance {
            ReplayProvenance::Cache | ReplayProvenance::SelectedCache { .. } => {
                LockedReplayOrigin::Cache
            }
            ReplayProvenance::Registry(_) | ReplayProvenance::LockedUrl => {
                LockedReplayOrigin::Registry
            }
        }
    }
    pub(crate) fn source_key(&self) -> Option<SourceObservationKey> {
        match &self.provenance {
            ReplayProvenance::Cache
            | ReplayProvenance::SelectedCache { .. }
            | ReplayProvenance::LockedUrl => None,
            ReplayProvenance::Registry(identity) => Some(identity.source_key),
        }
    }
    pub(crate) fn recheck(&self) -> LockedReplayResult<()> {
        self.slot.recheck().map_err(|_| LockedReplayError::Revoked)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> LockedReplayResult<()> {
        self.slot
            .recheck_for(owner)
            .map_err(|_| LockedReplayError::Revoked)
    }
}
impl fmt::Debug for LockedRegistryArtifact<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("LockedRegistryArtifact")
            .field("pin", &self.pin())
            .field("size", &self.size())
            .field("origin", &self.origin())
            .finish_non_exhaustive()
    }
}
impl Read for LockedRegistryArtifact<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.artifact.read(output)
    }
}
impl Seek for LockedRegistryArtifact<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.artifact.seek(position)
    }
}

pub(crate) async fn acquire_locked_registry_file<'lock>(
    slot: ExistingFileSlot<'lock>,
    context: LockedReplayContext<'_>,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    acquire_with_finish(slot, context, || {}).await
}

/// Fetch only the URL recorded in the actual mixed lock; never query Simple.
pub(crate) async fn acquire_locked_url<'lock>(
    root: &'lock BoundPythonLock,
    slot: ExistingFileSlot<'lock>,
    context: LockedReplayContext<'_>,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    within_deadline(context.deadline, async {
        slot.recheck_for(root)
            .map_err(|_| LockedReplayError::Revoked)?;
        let id = hex::encode(slot.pin().id().as_bytes());
        let model = root.model().ok_or(LockedReplayError::MissingLocator)?;
        let url = if model.has_locators() {
            model
                .locator(slot.pin().id())
                .ok_or(LockedReplayError::MissingLocator)?
        } else {
            let mut locations = model
                .mixed()
                .iter()
                .flat_map(|graph| &graph.nodes)
                .filter_map(|node| match &node.source {
                    crate::packages::python_resolution::lock_model::MixedLockSource::Registry {
                        pin,
                        url,
                    } if pin == &id => Some(url.as_deref()),
                    _ => None,
                });
            let url = locations
                .next()
                .flatten()
                .ok_or(LockedReplayError::MissingLocator)?;
            if locations.any(|other| other != Some(url)) {
                return Err(LockedReplayError::FileIdentity);
            }
            url
        };
        let limits = checked_limits(slot.pin(), &context)?;
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        let pin = slot.pin();
        if !registry_matches(pin.file(), context.project_session.registry) {
            return Err(LockedReplayError::RegistryMismatch);
        }
        if !crate::packages::python_resolution::lock_model::valid_registry_url(
            url,
            pin.file().filename(),
        ) {
            return Err(LockedReplayError::FileIdentity);
        }
        if let Some(artifact) = cached_replay_bytes(&slot, &context, limits)? {
            return Ok(LockedRegistryArtifact {
                slot,
                artifact,
                provenance: ReplayProvenance::Cache,
            });
        }
        if context.policy.network == NetworkPolicy::Offline {
            return Err(LockedReplayError::MissingInputs);
        }
        let canonical = Url::parse(url).map_err(|_| LockedReplayError::FileIdentity)?;
        let physical = context
            .file_session
            .transport
            .request_url(&canonical)
            .map_err(|error| LockedReplayError::Source(SourceResolutionError::Transport(error)))?;
        let fingerprint = context.file_session.transport.fingerprint();
        std::fs::create_dir_all(context.scratch_root).map_err(|_| LockedReplayError::Cache)?;
        let mut staging =
            tempfile::tempfile_in(context.scratch_root).map_err(|_| LockedReplayError::Cache)?;
        let transfer = context
            .client
            .copy_source_file_at_deadline(
                SourceFileSession {
                    credential_ref: context.file_session.credential_ref,
                    auth_scope: context.file_session.auth_scope,
                    credentials: context.file_session.credentials,
                    auth_origins: context.file_session.auth_origins,
                    transport: context.file_session.transport,
                },
                &canonical,
                context.policy.network,
                limits.transfer,
                &mut staging,
                context.deadline,
            )
            .await
            .map_err(|error| LockedReplayError::Source(SourceResolutionError::Transport(error)))?;
        if transfer.initial_canonical_url() != &canonical
            || transfer.initial_request_url() != &physical
            || transfer.auth_scope() != context.file_session.auth_scope
            || transfer.transport_fingerprint() != fingerprint
        {
            return Err(LockedReplayError::FileIdentity);
        }
        if transfer.size() != pin.size() {
            return Err(LockedReplayError::SizeMismatch);
        }
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        staging
            .rewind()
            .map_err(|_| LockedReplayError::ByteIdentity)?;
        let mut artifact = spool_verified_reader(
            caller_deadline::Reader::new(&mut staging, Some(context.deadline)),
            pin.file().filename(),
            pin.digest(),
            limits.transfer.max_wire_bytes,
            context.scratch_root,
        )
        .map_err(|_| LockedReplayError::ByteIdentity)?;
        if artifact.size() != pin.size() {
            return Err(LockedReplayError::SizeMismatch);
        }
        deadline(context.deadline)?;
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        publish_verified_artifact(
            &mut artifact,
            context.cache_root,
            limits.transfer.max_wire_bytes,
        )
        .map_err(|_| LockedReplayError::CachePublication)?;
        deadline(context.deadline)?;
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        artifact
            .rewind()
            .map_err(|_| LockedReplayError::ByteIdentity)?;
        Ok(LockedRegistryArtifact {
            slot,
            artifact,
            provenance: ReplayProvenance::LockedUrl,
        })
    })
    .await
}

async fn acquire_with_finish<'lock>(
    slot: ExistingFileSlot<'lock>,
    context: LockedReplayContext<'_>,
    finish: impl FnOnce(),
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    within_deadline(context.deadline, async {
        let artifact = replay(slot, context).await?;
        finish();
        Ok(artifact)
    })
    .await
}

async fn within_deadline<T>(
    original_deadline: Instant,
    work: impl std::future::Future<Output = LockedReplayResult<T>>,
) -> LockedReplayResult<T> {
    deadline(original_deadline)?;
    let result = tokio::time::timeout_at(tokio::time::Instant::from_std(original_deadline), work)
        .await
        .map_err(|_| LockedReplayError::Deadline)?;
    // The inner future can finish synchronous work before yielding again.
    deadline(original_deadline)?;
    result
}

fn deadline(limit: Instant) -> LockedReplayResult<()> {
    if Instant::now() >= limit {
        Err(LockedReplayError::Deadline)
    } else {
        Ok(())
    }
}

fn checked_limits(
    pin: &ContentPin,
    context: &LockedReplayContext<'_>,
) -> LockedReplayResult<SourceObservationLimits> {
    context
        .lock_limits
        .validate()
        .map_err(|_| LockedReplayError::InvalidLimits)?;
    let mut limits = context.source_limits;
    if limits.transfer.max_wire_bytes == 0
        || limits.transfer.max_header_bytes == 0
        || limits.transfer.max_headers == 0
        || limits.max_spool_bytes == 0
        || limits.max_identity_bytes == 0
        || limits.transfer.max_wire_bytes.checked_mul(2).is_none()
    {
        return Err(LockedReplayError::InvalidLimits);
    }
    limits.transfer.max_wire_bytes = limits
        .transfer
        .max_wire_bytes
        .min(context.lock_limits.max_artifact_bytes);
    if limits
        .transfer
        .max_wire_bytes
        .checked_mul(2)
        .is_none_or(|required| required > limits.max_spool_bytes)
    {
        return Err(LockedReplayError::InvalidLimits);
    }
    let file = pin.file();
    RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: file.registry_id(),
            canonical_base_fingerprint: *file.canonical_base_fingerprint(),
            project: file.project(),
            version: file.version(),
            filename: file.filename(),
            kind: file.kind(),
        },
        context.lock_limits,
    )
    .map_err(|_| LockedReplayError::InputLimits)?;
    if pin.size() > limits.transfer.max_wire_bytes {
        return Err(LockedReplayError::InputLimits);
    }
    let mut identity_bytes = 0;
    for value in [
        file.registry_id(),
        file.project(),
        file.version(),
        file.filename(),
        context.project_session.registry.id().as_str(),
        context
            .project_session
            .registry
            .canonical_base()
            .as_url()
            .as_str(),
        context
            .project_session
            .registry
            .credential_ref()
            .map_or("", |reference| reference.as_str()),
        context
            .file_session
            .credential_ref
            .map_or("", |reference| reference.as_str()),
    ] {
        charge_identity(&mut identity_bytes, value, limits.max_identity_bytes)
            .map_err(|_| LockedReplayError::InputLimits)?;
    }
    Ok(limits)
}

fn registry_matches(file: &RegistryFileRef, registry: &PythonRegistry) -> bool {
    file.registry_id() == registry.id().as_str()
        && file.canonical_base_fingerprint()
            == &canonical_registry_base_fingerprint(registry.canonical_base())
}

fn selected_matches(file: &RegistryFileRef, selected: &SelectedRegistryFile<'_>) -> bool {
    let project = &selected.project.data;
    let selected = &project.project.files()[selected.file_index];
    registry_matches(file, &project.registry)
        && file.project() == project.project.name().as_str()
        && file.filename() == selected.filename()
        && matches!(
            (file.kind(), selected.kind()),
            (
                RegistryArtifactKind::Wheel,
                PythonDistributionKind::Wheel(_)
            ) | (RegistryArtifactKind::Sdist, PythonDistributionKind::Sdist)
        )
        && match (
            Pep440::parse_version(file.version()),
            selected.version().and_then(Pep440::parse_version),
        ) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        }
}

fn page_digests(file: &SimpleFile) -> LockedReplayResult<[Option<Digest>; 2]> {
    let sha256 = file
        .hashes()
        .get("sha256")
        .map(|raw| {
            let mut bytes = [0; 32];
            hex::decode_to_slice(raw, &mut bytes).map_err(|_| LockedReplayError::FileIdentity)?;
            Ok(Digest::Sha256(bytes))
        })
        .transpose()?;
    let sha512 = file
        .hashes()
        .get("sha512")
        .map(|raw| {
            let mut bytes = [0; 64];
            hex::decode_to_slice(raw, &mut bytes).map_err(|_| LockedReplayError::FileIdentity)?;
            Ok(Digest::Sha512(bytes))
        })
        .transpose()?;
    Ok([sha256, sha512])
}

fn conflicting_digest(expected: Digest, advertised: Digest) -> bool {
    match (expected, advertised) {
        (Digest::Sha256(expected), Digest::Sha256(actual)) => expected != actual,
        (Digest::Sha512(expected), Digest::Sha512(actual)) => expected != actual,
        _ => false,
    }
}

fn verify_claims<R: Read + Seek>(
    mut reader: R,
    filename: &str,
    expected: Digest,
    advertised: [Option<Digest>; 2],
    context: &LockedReplayContext<'_>,
    limits: SourceObservationLimits,
) -> LockedReplayResult<VerifiedArtifact> {
    // At most the original held snapshot and one new anonymous snapshot live
    // together. Each temporary page verification is dropped before the next
    // claim; the final capability is always bound to the OLD lock digest.
    for claim in advertised.into_iter().flatten() {
        deadline(context.deadline)?;
        reader
            .rewind()
            .map_err(|_| LockedReplayError::ByteIdentity)?;
        let verified = spool_verified_reader(
            &mut reader,
            filename,
            claim,
            limits.transfer.max_wire_bytes,
            context.scratch_root,
        )
        .map_err(|_| LockedReplayError::ByteIdentity)?;
        drop(verified);
        deadline(context.deadline)?;
    }
    reader
        .rewind()
        .map_err(|_| LockedReplayError::ByteIdentity)?;
    deadline(context.deadline)?;
    let artifact = spool_verified_reader(
        reader,
        filename,
        expected,
        limits.transfer.max_wire_bytes,
        context.scratch_root,
    )
    .map_err(|_| LockedReplayError::ByteIdentity)?;
    deadline(context.deadline)?;
    Ok(artifact)
}

fn cached_replay_bytes(
    slot: &ExistingFileSlot<'_>,
    context: &LockedReplayContext<'_>,
    limits: SourceObservationLimits,
) -> LockedReplayResult<Option<VerifiedArtifact>> {
    let pin = slot.pin();
    let artifact = cached_verified_artifact(
        pin.file().filename(),
        pin.digest(),
        context.cache_root,
        limits.transfer.max_wire_bytes,
    )
    .map_err(|_| LockedReplayError::Cache)?;
    deadline(context.deadline)?;
    if let Some(artifact) = &artifact {
        if artifact.size() != pin.size() {
            return Err(LockedReplayError::SizeMismatch);
        }
        slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
        deadline(context.deadline)?;
    }
    Ok(artifact)
}

async fn replay<'lock>(
    slot: ExistingFileSlot<'lock>,
    context: LockedReplayContext<'_>,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    deadline(context.deadline)?;
    let limits = checked_limits(slot.pin(), &context)?;
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    let pin = slot.pin();
    let file = pin.file();
    if !registry_matches(file, context.project_session.registry) {
        return Err(LockedReplayError::RegistryMismatch);
    }
    // Standalone replay checks the old expected cache before any Simple,
    // router, credential provider or source-observation request.
    if let Some(artifact) = cached_replay_bytes(&slot, &context, limits)? {
        return Ok(LockedRegistryArtifact {
            slot,
            artifact,
            provenance: ReplayProvenance::Cache,
        });
    }
    if context.policy.network == NetworkPolicy::Offline {
        return Err(LockedReplayError::MissingInputs);
    }
    let name =
        NormalizedPythonName::new(file.project()).map_err(|_| LockedReplayError::FileIdentity)?;
    let project = caller_deadline::fetch_registry_project_at_deadline(
        context.client,
        PythonSourceSession {
            registry: context.project_session.registry,
            auth_scope: context.project_session.auth_scope,
            credentials: context.project_session.credentials,
            auth_origins: context.project_session.auth_origins,
            transport: context.project_session.transport,
        },
        &name,
        context.policy,
        context.project_limits,
        context.deadline,
    )
    .await
    .map_err(LockedReplayError::Source)?;
    deadline(context.deadline)?;
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    let RegistryProjectLookup::Present(project) = project else {
        return Err(LockedReplayError::FileMissing);
    };
    let selected = project
        .select_exact_file(file.filename())
        .map_err(|_| LockedReplayError::FileMissing)?;
    download_selected(slot, &selected, context, limits).await
}

async fn download_selected<'lock>(
    slot: ExistingFileSlot<'lock>,
    selected: &SelectedRegistryFile<'_>,
    context: LockedReplayContext<'_>,
    limits: SourceObservationLimits,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    deadline(context.deadline)?;
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    let pin = slot.pin();
    let file = pin.file();
    if !selected_matches(file, selected) {
        return Err(LockedReplayError::FileIdentity);
    }
    let advertised = page_digests(&selected.project.data.project.files()[selected.file_index])?;
    if advertised
        .into_iter()
        .flatten()
        .any(|digest| conflicting_digest(pin.digest(), digest))
    {
        return Err(LockedReplayError::HashConflict);
    }
    let acquired = caller_deadline::acquire_registry_file_at_deadline(
        selected,
        context.client,
        SourceFileSession {
            credential_ref: context.file_session.credential_ref,
            auth_scope: context.file_session.auth_scope,
            credentials: context.file_session.credentials,
            auth_origins: context.file_session.auth_origins,
            transport: context.file_session.transport,
        },
        context.policy.network,
        limits,
        context.scratch_root,
        context.deadline,
    )
    .await
    .map_err(LockedReplayError::Source)?;
    deadline(context.deadline)?;
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    let (identity, mut artifact) = match acquired {
        AcquiredRegistryFile::Expected(source) => (
            source.identity,
            verify_claims(
                source.artifact,
                file.filename(),
                pin.digest(),
                advertised,
                &context,
                limits,
            )?,
        ),
        AcquiredRegistryFile::Observed(source) => (
            source.identity,
            verify_claims(
                source.artifact,
                file.filename(),
                pin.digest(),
                advertised,
                &context,
                limits,
            )?,
        ),
    };
    if artifact.filename() != file.filename() || artifact.digest() != pin.digest() {
        return Err(LockedReplayError::ByteIdentity);
    }
    if artifact.size() != pin.size() {
        return Err(LockedReplayError::SizeMismatch);
    }
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    deadline(context.deadline)?;
    publish_verified_artifact(
        &mut artifact,
        context.cache_root,
        limits.transfer.max_wire_bytes,
    )
    .map_err(|_| LockedReplayError::CachePublication)?;
    deadline(context.deadline)?;
    slot.recheck().map_err(|_| LockedReplayError::Revoked)?;
    artifact
        .rewind()
        .map_err(|_| LockedReplayError::ByteIdentity)?;
    Ok(LockedRegistryArtifact {
        slot,
        artifact,
        provenance: ReplayProvenance::Registry(identity),
    })
}

#[cfg(test)]
#[path = "replay/tests.rs"]
mod tests;
