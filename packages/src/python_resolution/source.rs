//! Private source owners created by actual Simple and file acquisitions.

use std::fmt;

pub(crate) mod caller_deadline;
pub(crate) mod locked_graph;
pub(crate) mod registry_depot;
pub(crate) mod replay;
mod wheel_metadata;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use pakro_index::artifact::NetworkPolicy;
use pakro_index::python_source::{
    AuthScopeKey, NormalizedPythonName, PythonRegistry, PythonRegistryId, SimpleRepresentation,
    TransportRulesFingerprint,
};
use pakro_index::simple::{
    CompletedSourceTransfer, ProjectLookup, PythonDistributionKind, PythonSourceSession,
    SimpleClientError, SimpleClientLimitKind, SimpleClientLimits, SimpleFetchPolicy,
    SimpleIndexClient, SimpleProject, SourceFileLimits, SourceFileSession,
};
use pakro_store::Digest;
use reqwest::Url;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::artifact_snapshot::{
    ObservedArtifact, VerifiedArtifact, spool_observed_reader, spool_verified_reader,
};

pub(crate) type SourceResolutionResult<T> = Result<T, SourceResolutionError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SourceResolutionError {
    #[error("source acquisition failed: {0}")]
    Transport(SimpleClientError),
    #[error("source observation limits must be positive")]
    InvalidLimits,
    #[error("source observation spool budget is insufficient")]
    SpoolBudget,
    #[error("source identity exceeds its byte budget")]
    IdentityBudget,
    #[error("selected file is missing from its exact registry project")]
    FileMissing,
    #[error("selected source or file identity is inconsistent")]
    IdentityMismatch,
    #[error("source artifact bytes could not be verified against their declared identity")]
    ByteIdentity,
    #[error("source observation I/O failed")]
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SourceObservationKey([u8; 32]);
impl SourceObservationKey {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub(crate) struct BoundRegistryProject {
    data: Arc<BoundProjectData>,
}
struct BoundProjectData {
    registry: PythonRegistry,
    project: SimpleProject,
    key: SourceObservationKey,
}
impl BoundRegistryProject {
    pub(crate) fn registry_id(&self) -> &PythonRegistryId {
        self.data.registry.id()
    }
    pub(crate) fn name(&self) -> &NormalizedPythonName {
        self.data.project.name()
    }
    pub(crate) fn source_key(&self) -> SourceObservationKey {
        self.data.key
    }
    pub(crate) fn select_exact_file(
        &self,
        filename: &str,
    ) -> SourceResolutionResult<SelectedRegistryFile<'_>> {
        let file_index = self
            .data
            .project
            .files()
            .iter()
            .position(|file| file.filename() == filename)
            .ok_or(SourceResolutionError::FileMissing)?;
        Ok(SelectedRegistryFile {
            project: self,
            file_index,
        })
    }
}
impl fmt::Debug for BoundRegistryProject {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("BoundRegistryProject")
            .field("source", self.registry_id())
            .field("project", self.name())
            .field("files", &self.data.project.files().len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) enum RegistryProjectLookup {
    Present(BoundRegistryProject),
    Missing404,
}

pub(crate) struct SelectedRegistryFile<'a> {
    project: &'a BoundRegistryProject,
    file_index: usize,
}
impl SelectedRegistryFile<'_> {
    pub(crate) fn filename(&self) -> &str {
        self.project.data.project.files()[self.file_index].filename()
    }
    pub(crate) fn version(&self) -> Option<&str> {
        self.project.data.project.files()[self.file_index].version()
    }
}
impl fmt::Debug for SelectedRegistryFile<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SelectedRegistryFile")
            .field("filename", &self.filename())
            .finish_non_exhaustive()
    }
}

struct RegistryFileIdentity {
    project: Arc<BoundProjectData>,
    file_index: usize,
    source_key: SourceObservationKey,
}

pub(crate) struct BoundSourceObservation {
    identity: RegistryFileIdentity,
    artifact: ObservedArtifact,
}
impl BoundSourceObservation {
    pub(crate) fn registry_id(&self) -> &PythonRegistryId {
        self.identity.project.registry.id()
    }
    pub(crate) fn project(&self) -> &NormalizedPythonName {
        self.identity.project.project.name()
    }
    pub(crate) fn filename(&self) -> &str {
        self.identity.project.project.files()[self.identity.file_index].filename()
    }
    pub(crate) fn version(&self) -> Option<&str> {
        self.identity.project.project.files()[self.identity.file_index].version()
    }
    pub(crate) fn kind(&self) -> &PythonDistributionKind {
        self.identity.project.project.files()[self.identity.file_index].kind()
    }
    pub(crate) fn source_key(&self) -> SourceObservationKey {
        self.identity.source_key
    }
    pub(crate) fn size(&self) -> u64 {
        self.artifact.size()
    }
    pub(crate) fn observed_sha256(&self) -> [u8; 32] {
        self.artifact.observed_sha256()
    }
}
impl fmt::Debug for BoundSourceObservation {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("BoundSourceObservation")
            .field("source", self.registry_id())
            .field("project", self.project())
            .field("filename", &self.filename())
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}
impl Read for BoundSourceObservation {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.artifact.read(output)
    }
}
impl Seek for BoundSourceObservation {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.artifact.seek(position)
    }
}

pub(crate) struct BoundVerifiedSource {
    identity: RegistryFileIdentity,
    artifact: VerifiedArtifact,
}
impl BoundVerifiedSource {
    pub(crate) fn registry_id(&self) -> &PythonRegistryId {
        self.identity.project.registry.id()
    }
    pub(crate) fn filename(&self) -> &str {
        self.identity.project.project.files()[self.identity.file_index].filename()
    }
    pub(crate) fn source_key(&self) -> SourceObservationKey {
        self.identity.source_key
    }
    pub(crate) fn size(&self) -> u64 {
        self.artifact.size()
    }
    pub(crate) fn digest(&self) -> Digest {
        self.artifact.digest()
    }
}
impl fmt::Debug for BoundVerifiedSource {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("BoundVerifiedSource")
            .field("source", self.registry_id())
            .field("filename", &self.filename())
            .field("digest", &self.digest())
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}
impl Read for BoundVerifiedSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.artifact.read(output)
    }
}
impl Seek for BoundVerifiedSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.artifact.seek(position)
    }
}

#[derive(Debug)]
pub(crate) enum AcquiredRegistryFile {
    Observed(BoundSourceObservation),
    Expected(BoundVerifiedSource),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceObservationLimits {
    pub(crate) transfer: SourceFileLimits,
    pub(crate) max_spool_bytes: u64,
    pub(crate) max_identity_bytes: usize,
}
impl Default for SourceObservationLimits {
    fn default() -> Self {
        Self {
            transfer: SourceFileLimits::default(),
            max_spool_bytes: 1024 * 1024 * 1024,
            max_identity_bytes: 2 * 1024 * 1024,
        }
    }
}
impl SourceObservationLimits {
    pub(crate) fn validate(self) -> SourceResolutionResult<()> {
        if self.transfer.max_wire_bytes == 0
            || self.transfer.max_header_bytes == 0
            || self.transfer.max_headers == 0
            || self.max_spool_bytes == 0
            || self.max_identity_bytes == 0
        {
            return Err(SourceResolutionError::InvalidLimits);
        }
        if self
            .transfer
            .max_wire_bytes
            .checked_mul(2)
            .is_none_or(|required| required > self.max_spool_bytes)
        {
            return Err(SourceResolutionError::SpoolBudget);
        }
        Ok(())
    }
}

/// Fetch and bind the complete input context in one owner. No arbitrary
/// SimpleProject-to-owner conversion is exposed.
pub(crate) async fn fetch_registry_project(
    client: &SimpleIndexClient,
    session: PythonSourceSession<'_>,
    project: &NormalizedPythonName,
    policy: SimpleFetchPolicy,
    limits: SimpleClientLimits,
) -> SourceResolutionResult<RegistryProjectLookup> {
    fetch_registry_project_with_budget(client, session, project, policy, limits, None).await
}

async fn fetch_registry_project_with_budget(
    client: &SimpleIndexClient,
    session: PythonSourceSession<'_>,
    project: &NormalizedPythonName,
    policy: SimpleFetchPolicy,
    limits: SimpleClientLimits,
    deadline: Option<std::time::Instant>,
) -> SourceResolutionResult<RegistryProjectLookup> {
    caller_deadline::check(deadline)?;
    let registry = session.registry;
    let scope = *session.auth_scope;
    let fingerprint = session.transport.fingerprint();
    let canonical = registry
        .project_url(project)
        .map_err(|_| SourceResolutionError::IdentityMismatch)?;
    // The router is an explicit, pure configuration lookup, as required by
    // the shared transport. Its exact initial route is part of this owner.
    let physical = session
        .transport
        .request_url(canonical.as_url())
        .map_err(SourceResolutionError::Transport)?;
    caller_deadline::check(deadline)?;
    let fetched = match deadline {
        Some(deadline) => {
            client
                .project_at_deadline(session, project, policy, limits, deadline)
                .await
        }
        None => client.project(session, project, policy, limits).await,
    }
    .map_err(SourceResolutionError::Transport)?;
    caller_deadline::check(deadline)?;
    match fetched {
        ProjectLookup::Missing404 => Ok(RegistryProjectLookup::Missing404),
        ProjectLookup::Present(facts) => {
            if facts.source() != registry.id() || facts.name() != project {
                return Err(SourceResolutionError::IdentityMismatch);
            }
            let key = project_context_key(
                registry,
                &scope,
                &fingerprint,
                canonical.as_url(),
                &physical,
                project,
            );
            caller_deadline::check(deadline)?;
            Ok(RegistryProjectLookup::Present(BoundRegistryProject {
                data: Arc::new(BoundProjectData {
                    registry: registry.clone(),
                    project: facts,
                    key,
                }),
            }))
        }
    }
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

fn project_context_key(
    registry: &PythonRegistry,
    scope: &AuthScopeKey,
    mapping: &TransportRulesFingerprint,
    canonical: &Url,
    physical: &Url,
    name: &NormalizedPythonName,
) -> SourceObservationKey {
    let mut hash = Sha256::new();
    hash_field(&mut hash, b"pakro.python-bound-project/v1");
    // This is the fixed Accept-negotiation identity, never a claim that the
    // actual response had one specific representation.
    for representation in [
        SimpleRepresentation::JsonV1,
        SimpleRepresentation::HtmlV1,
        SimpleRepresentation::LegacyHtml,
    ] {
        hash_field(
            &mut hash,
            registry
                .cache_key(scope, mapping, representation)
                .as_bytes(),
        );
    }
    hash_field(&mut hash, canonical.as_str().as_bytes());
    hash_field(&mut hash, physical.as_str().as_bytes());
    hash_field(&mut hash, name.as_str().as_bytes());
    SourceObservationKey(hash.finalize().into())
}

pub(crate) async fn acquire_registry_file(
    selected: &SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    scratch_root: &Path,
) -> SourceResolutionResult<AcquiredRegistryFile> {
    acquire_registry_file_with_budget(
        selected,
        client,
        session,
        network,
        limits,
        scratch_root,
        None,
    )
    .await
}

async fn acquire_registry_file_with_budget(
    selected: &SelectedRegistryFile<'_>,
    client: &SimpleIndexClient,
    session: SourceFileSession<'_>,
    network: NetworkPolicy,
    limits: SourceObservationLimits,
    scratch_root: &Path,
    deadline: Option<std::time::Instant>,
) -> SourceResolutionResult<AcquiredRegistryFile> {
    limits.validate()?;
    caller_deadline::check(deadline)?;
    if network == NetworkPolicy::Offline {
        return Err(SourceResolutionError::Transport(
            SimpleClientError::OfflineMissing,
        ));
    }
    let project = &selected.project.data;
    let file = &project.project.files()[selected.file_index];
    if file
        .size()
        .is_some_and(|size| size > limits.transfer.max_wire_bytes)
    {
        return Err(SourceResolutionError::Transport(
            SimpleClientError::LimitExceeded(SimpleClientLimitKind::WireBytes),
        ));
    }
    let canonical = file.locator().url();
    let physical = session
        .transport
        .request_url(canonical)
        .map_err(SourceResolutionError::Transport)?;
    caller_deadline::check(deadline)?;
    let fingerprint = session.transport.fingerprint();
    let mut identity_bytes = 0usize;
    for value in [
        project.registry.id().as_str(),
        project.registry.canonical_base().as_url().as_str(),
        project
            .registry
            .credential_ref()
            .map_or("", |value| value.as_str()),
        project.project.name().as_str(),
        file.filename(),
        file.version().unwrap_or_default(),
        canonical.as_str(),
        physical.as_str(),
        session.credential_ref.map_or("", |value| value.as_str()),
    ] {
        charge_identity(&mut identity_bytes, value, limits.max_identity_bytes)?;
    }
    std::fs::create_dir_all(scratch_root).map_err(|_| SourceResolutionError::Io)?;
    // No pathname or writable descriptor crosses the source-owner boundary.
    // Network copy and snapshot acquisition both use this exact held file.
    let mut staging = tempfile::tempfile_in(scratch_root).map_err(|_| SourceResolutionError::Io)?;
    caller_deadline::check(deadline)?;
    let copied_session = SourceFileSession {
        credential_ref: session.credential_ref,
        auth_scope: session.auth_scope,
        credentials: session.credentials,
        auth_origins: session.auth_origins,
        transport: session.transport,
    };
    let transfer = match deadline {
        Some(deadline) => {
            client
                .copy_source_file_at_deadline(
                    copied_session,
                    canonical,
                    network,
                    limits.transfer,
                    &mut staging,
                    deadline,
                )
                .await
        }
        None => {
            client
                .copy_source_file(
                    copied_session,
                    canonical,
                    network,
                    limits.transfer,
                    &mut staging,
                )
                .await
        }
    }
    .map_err(SourceResolutionError::Transport)?;
    caller_deadline::check(deadline)?;
    if transfer.initial_canonical_url() != canonical
        || transfer.initial_request_url() != &physical
        || transfer.auth_scope() != session.auth_scope
        || transfer.transport_fingerprint() != fingerprint
    {
        return Err(SourceResolutionError::IdentityMismatch);
    }
    for url in [transfer.canonical_url(), transfer.request_url()] {
        charge_identity(&mut identity_bytes, url.as_str(), limits.max_identity_bytes)?;
    }
    if file.size().is_some_and(|size| size != transfer.size()) {
        return Err(SourceResolutionError::ByteIdentity);
    }
    staging.rewind().map_err(|_| SourceResolutionError::Io)?;
    if let Some(expected) = file.hashes().preferred_digest() {
        let artifact = spool_verified_reader(
            caller_deadline::Reader::new(&mut staging, deadline),
            file.filename(),
            expected,
            limits.transfer.max_wire_bytes,
            scratch_root,
        )
        .map_err(|_| caller_deadline::error(deadline, SourceResolutionError::ByteIdentity))?;
        caller_deadline::check(deadline)?;
        if artifact.size() != transfer.size() {
            return Err(SourceResolutionError::ByteIdentity);
        }
        let key = file_observation_key(selected, &session, &transfer, expected);
        caller_deadline::check(deadline)?;
        Ok(AcquiredRegistryFile::Expected(BoundVerifiedSource {
            identity: RegistryFileIdentity {
                project: Arc::clone(project),
                file_index: selected.file_index,
                source_key: key,
            },
            artifact,
        }))
    } else {
        let artifact = spool_observed_reader(
            caller_deadline::Reader::new(&mut staging, deadline),
            file.filename(),
            limits.transfer.max_wire_bytes,
            scratch_root,
        )
        .map_err(|_| caller_deadline::error(deadline, SourceResolutionError::Io))?;
        caller_deadline::check(deadline)?;
        if artifact.size() != transfer.size() {
            return Err(SourceResolutionError::ByteIdentity);
        }
        let digest = Digest::Sha256(artifact.observed_sha256());
        let key = file_observation_key(selected, &session, &transfer, digest);
        caller_deadline::check(deadline)?;
        Ok(AcquiredRegistryFile::Observed(BoundSourceObservation {
            identity: RegistryFileIdentity {
                project: Arc::clone(project),
                file_index: selected.file_index,
                source_key: key,
            },
            artifact,
        }))
    }
}

fn charge_identity(used: &mut usize, value: &str, max: usize) -> SourceResolutionResult<()> {
    *used = used
        .checked_add(value.len())
        .filter(|size| *size <= max)
        .ok_or(SourceResolutionError::IdentityBudget)?;
    Ok(())
}

fn file_observation_key(
    selected: &SelectedRegistryFile<'_>,
    session: &SourceFileSession<'_>,
    transfer: &CompletedSourceTransfer,
    digest: Digest,
) -> SourceObservationKey {
    let project = &selected.project.data;
    let mut hash = Sha256::new();
    hash_field(&mut hash, b"pakro.python-bound-file/v1");
    hash_field(&mut hash, project.key.as_bytes());
    for representation in [
        SimpleRepresentation::JsonV1,
        SimpleRepresentation::HtmlV1,
        SimpleRepresentation::LegacyHtml,
    ] {
        hash_field(
            &mut hash,
            project
                .registry
                .cache_key(
                    session.auth_scope,
                    &transfer.transport_fingerprint(),
                    representation,
                )
                .as_bytes(),
        );
    }
    hash_field(
        &mut hash,
        if session.credential_ref.is_some() {
            b"credential"
        } else {
            b"anonymous"
        },
    );
    if let Some(reference) = session.credential_ref {
        hash_field(&mut hash, reference.as_str().as_bytes());
    }
    let file = &project.project.files()[selected.file_index];
    hash_field(&mut hash, file.filename().as_bytes());
    hash_field(&mut hash, file.version().unwrap_or_default().as_bytes());
    hash_field(
        &mut hash,
        match file.kind() {
            PythonDistributionKind::Wheel(_) => b"wheel",
            PythonDistributionKind::Sdist => b"sdist",
            PythonDistributionKind::HistoricalFormat => b"historical",
            PythonDistributionKind::Other => b"other",
        },
    );
    for url in [
        transfer.initial_canonical_url(),
        transfer.initial_request_url(),
        transfer.canonical_url(),
        transfer.request_url(),
    ] {
        hash_field(&mut hash, url.as_str().as_bytes());
    }
    match digest {
        Digest::Sha256(value) => {
            hash_field(&mut hash, b"sha256");
            hash_field(&mut hash, &value);
        }
        Digest::Sha512(value) => {
            hash_field(&mut hash, b"sha512");
            hash_field(&mut hash, &value);
        }
    }
    hash_field(&mut hash, &transfer.size().to_be_bytes());
    SourceObservationKey(hash.finalize().into())
}

pub(crate) mod fresh;

#[cfg(test)]
#[path = "source/tests.rs"]
mod tests;
