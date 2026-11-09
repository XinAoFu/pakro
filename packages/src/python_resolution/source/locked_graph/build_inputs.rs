//! Build inputs are selected only from the live lock's original source receipt.
use super::*;
use crate::packages::python_environment::model::{SourceBuildInput, SourceBuildRecord};
use crate::packages::python_resolution::lock_model::{
    RegistryArtifactKind, canonical_registry_base_fingerprint,
};
use crate::packages::python_resolution::source::registry_depot::RegistryDepotContext;
use crate::packages::python_resolution::source::replay::{
    LockedReplayContext, acquire_locked_registry_file, acquire_locked_url,
};
use crate::packages::python_source_build::CompletedSourceBuild;
use crate::packages::python_wheel::WheelExpectation;
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};

pub(crate) struct LockedBuildInputEvidence<'lock> {
    source: LockedRegistryArtifact<'lock>,
    deadline: Instant,
}
impl LockedBuildInputEvidence<'_> {
    pub(crate) fn recheck(&self, root: &BoundPythonLock) -> Result<(), LockedGraphError> {
        clock(self.deadline)?;
        self.source
            .recheck_for(root)
            .map_err(|_| LockedGraphError::Revoked)?;
        clock(self.deadline)
    }
}
pub(crate) struct LockedBuildInput<'lock> {
    input: SourceBuildInput,
    archive: InspectedWheelArchive,
    evidence: LockedBuildInputEvidence<'lock>,
}
impl<'lock> LockedBuildInput<'lock> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        SourceBuildInput,
        InspectedWheelArchive,
        LockedBuildInputEvidence<'lock>,
    ) {
        (self.input, self.archive, self.evidence)
    }
}
pub(crate) enum LockedBuildInputPreparation<'lock, 'config> {
    Ready(LockedBuildInput<'lock>),
    NeedsBuild(LockedBuildInputContinuation<'lock, 'config>),
}
pub(crate) struct LockedBuildInputContinuation<'lock, 'config> {
    root: &'lock BoundPythonLock,
    input: SourceBuildInput,
    source: LockedRegistryArtifact<'lock>,
    context: RegistryDepotContext<'config>,
    target: FrozenRegistryTarget,
}
impl<'lock, 'config> LockedBuildInputContinuation<'lock, 'config> {
    pub(crate) fn pin(&self) -> &ContentPin {
        self.source.pin()
    }
    pub(crate) fn expected_record(&self) -> Result<&SourceBuildRecord, LockedGraphError> {
        build::expected(self.root, self.source.pin())
    }
    pub(crate) fn source_verified_copy(
        &mut self,
        scratch: &Path,
    ) -> Result<VerifiedArtifact, LockedGraphError> {
        self.expected_record()?;
        build::copy_source(
            &mut self.source,
            scratch,
            self.context.deadline,
            self.context.limits.lock.max_artifact_bytes,
        )
    }
    pub(crate) fn resume_with_built(
        self,
        completed: CompletedSourceBuild,
    ) -> Result<LockedBuildInput<'lock>, LockedGraphError> {
        clock(self.context.deadline)?;
        let expected = self.expected_record()?;
        if completed.target_fingerprint() != *self.target.fingerprint()
            || expected
                .fingerprint()
                .map_err(|_| LockedGraphError::BuildMismatch)?
                != completed
                    .record()
                    .fingerprint()
                    .map_err(|_| LockedGraphError::BuildMismatch)?
        {
            return Err(LockedGraphError::BuildMismatch);
        }
        let (archive, _) = completed
            .into_parts()
            .map_err(|_| LockedGraphError::BuildMismatch)?;
        finish(self.root, self.input, self.source, archive, &self.context)
    }
}
fn clock(deadline: Instant) -> Result<(), LockedGraphError> {
    if Instant::now() >= deadline {
        Err(LockedGraphError::Deadline)
    } else {
        Ok(())
    }
}
fn finish<'lock>(
    root: &'lock BoundPythonLock,
    input: SourceBuildInput,
    source: LockedRegistryArtifact<'lock>,
    archive: InspectedWheelArchive,
    context: &RegistryDepotContext<'_>,
) -> Result<LockedBuildInput<'lock>, LockedGraphError> {
    clock(context.deadline)?;
    source
        .recheck_for(root)
        .map_err(|_| LockedGraphError::Revoked)?;
    if archive.filename() != input.output_filename
        || archive.digest().to_string() != input.output_digest
        || archive.size() != input.output_size
        || !archive
            .manifest()
            .tags()
            .iter()
            .any(|tag| context.supported.ordered().contains(tag))
    {
        return Err(LockedGraphError::BuildMismatch);
    }
    archive
        .check_limits(context.limits.wheel)
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    super::super::wheel_metadata::project_manifest_metadata(
        archive.manifest(),
        &input.name,
        &input.version,
        context.limits.metadata,
    )
    .map_err(|_| LockedGraphError::BuildMismatch)?;
    if input.source_is_sdist {
        let expected = build::expected(root, source.pin())?;
        let metadata: [u8; 32] = Sha256::digest(archive.manifest().core_metadata().raw()).into();
        if expected.output_filename != input.output_filename
            || expected.output_digest != input.output_digest
            || expected.output_size != input.output_size
            || expected.metadata_sha256 != metadata
        {
            return Err(LockedGraphError::BuildMismatch);
        }
    }
    clock(context.deadline)?;
    Ok(LockedBuildInput {
        input,
        archive,
        evidence: LockedBuildInputEvidence {
            source,
            deadline: context.deadline,
        },
    })
}

pub(crate) async fn acquire_build_input<'lock, 'config>(
    root: &'lock BoundPythonLock,
    parent_source: PinId,
    input_name: &str,
    context: RegistryDepotContext<'config>,
) -> Result<LockedBuildInputPreparation<'lock, 'config>, LockedGraphError> {
    clock(context.deadline)?;
    root.check_limits(context.limits.lock)
        .map_err(|_| LockedGraphError::Revoked)?;
    let record = root
        .source_build(parent_source)
        .ok_or(LockedGraphError::MissingBuild)?;
    record
        .validate()
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    let mut matching = record
        .inputs
        .iter()
        .chain(
            record
                .observation
                .iter()
                .flat_map(|value| &value.initial_inputs),
        )
        .filter(|input| input.name == input_name);
    let input = matching
        .next()
        .ok_or(LockedGraphError::BuildMismatch)?
        .clone();
    if matching.any(|other| other != &input) {
        return Err(LockedGraphError::BuildMismatch);
    }
    let pin = input
        .content_pin()
        .map_err(|_| LockedGraphError::BuildMismatch)?;
    let slot = root
        .existing_pin(pin.id())
        .map_err(|_| LockedGraphError::Revoked)?
        .ok_or(LockedGraphError::BuildMismatch)?;
    if slot.pin() != &pin {
        return Err(LockedGraphError::BuildMismatch);
    }
    let registries = context
        .endpoints
        .iter()
        .map(|endpoint| endpoint.project.registry)
        .collect::<Vec<_>>();
    let order = pakro_index::python_source::ordered_python_source_indices(
        &registries,
        context.source_policy,
    )
    .map_err(|_| LockedGraphError::RegistryMismatch)?;
    let endpoint = order
        .iter()
        .map(|index| &context.endpoints[*index])
        .find(|endpoint| {
            endpoint.project.registry.id().as_str() == pin.file().registry_id()
                && canonical_registry_base_fingerprint(endpoint.project.registry.canonical_base())
                    == *pin.file().canonical_base_fingerprint()
        })
        .ok_or(LockedGraphError::RegistryMismatch)?;
    let replay_context = LockedReplayContext {
        client: context.client,
        project_session: PythonSourceSession {
            registry: endpoint.project.registry,
            auth_scope: endpoint.project.auth_scope,
            credentials: endpoint.project.credentials,
            auth_origins: endpoint.project.auth_origins,
            transport: endpoint.project.transport,
        },
        file_session: SourceFileSession {
            credential_ref: endpoint.file.credential_ref,
            auth_scope: endpoint.file.auth_scope,
            credentials: endpoint.file.credentials,
            auth_origins: endpoint.file.auth_origins,
            transport: endpoint.file.transport,
        },
        policy: context.fetch,
        project_limits: context.limits.project,
        source_limits: context.limits.source,
        lock_limits: context.limits.lock,
        cache_root: context.cache_root,
        scratch_root: context.scratch_root,
        deadline: context.deadline,
    };
    let source = if root.model().is_some_and(|model| model.has_locators()) {
        acquire_locked_url(root, slot, replay_context).await
    } else {
        acquire_locked_registry_file(slot, replay_context).await
    }
    .map_err(LockedGraphError::Replay)?;
    clock(context.deadline)?;
    if pin.file().kind() == RegistryArtifactKind::Sdist {
        build::expected(root, &pin)?;
        // Source output reuse needs actual source/runtime preparation, just
        // as a runtime node does. This continuation retains the source owner
        // until the caller supplies a checked CompletedSourceBuild.
        let target = FrozenRegistryTarget::new(
            context.environment,
            context.supported,
            pakro_index::python_markers::PythonMarkerScope::Environment,
            context.semantics,
            Default::default(),
        )?;
        return Ok(LockedBuildInputPreparation::NeedsBuild(
            LockedBuildInputContinuation {
                root,
                input,
                source,
                context,
                target,
            },
        ));
    }
    let mut source = source;
    let copy = build::copy_source(
        &mut source,
        context.scratch_root,
        context.deadline,
        context.limits.lock.max_artifact_bytes,
    )?;
    let archive = inspect_owned_wheel(
        copy,
        pin.digest(),
        &WheelExpectation {
            filename: pin.file().filename(),
            name: pin.file().project(),
            version: pin.file().version(),
            supported: context.supported,
        },
        context.limits.wheel,
    )
    .map_err(|_| LockedGraphError::BuildMismatch)?;
    Ok(LockedBuildInputPreparation::Ready(finish(
        root, input, source, archive, &context,
    )?))
}
