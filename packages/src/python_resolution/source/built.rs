//! Generated wheels retain their original owned source and actual execution
//! receipt. No generated file is relabelled as a registry wheel selection.
use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;
use crate::packages::python_source_build::CompletedSourceBuild;
use crate::packages::python_wheel_source::InspectedWheelArchive;
use sha2::{Digest as _, Sha256};
use std::io::{Read, Seek, SeekFrom};

impl BuildOwner<'_> {
    pub(super) fn pin(&self) -> &ContentPin {
        match self {
            Self::Fresh(value) => value.pin(),
            Self::Locked(value) => value.pin(),
        }
    }
    pub(super) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        match self {
            Self::Fresh(value) => value
                .recheck_for(root)
                .map_err(|_| RegistryDepotError::Revoked),
            Self::Locked(value) => value
                .recheck_for(root)
                .map_err(|_| RegistryDepotError::Revoked),
        }
    }
}
impl Read for BuildOwner<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Fresh(value) => value.read(out),
            Self::Locked(value) => value.read(out),
        }
    }
}
impl Seek for BuildOwner<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Fresh(value) => value.seek(position),
            Self::Locked(value) => value.seek(position),
        }
    }
}

pub(crate) struct BuiltRegistryWheel<'root> {
    source: BuildOwner<'root>,
    archive: InspectedWheelArchive,
    facts: PythonDependencyMetadataFacts,
    record: SourceBuildRecord,
}
impl BuiltRegistryWheel<'_> {
    pub(super) fn pin(&self) -> &ContentPin {
        self.source.pin()
    }
    pub(super) fn manifest(&self) -> &WheelManifest {
        self.archive.manifest()
    }
    pub(super) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(super) fn record(&self) -> &SourceBuildRecord {
        &self.record
    }
    pub(super) fn output_digest(&self) -> Digest {
        self.archive.digest()
    }
    pub(super) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        self.source.recheck_for(root)
    }
}

pub(super) struct RetainedBuiltRegistryWheel<'root> {
    source: BuildOwner<'root>,
    facts: PythonDependencyMetadataFacts,
    record: SourceBuildRecord,
    output_digest: Digest,
}
impl<'root> BuiltRegistryWheel<'root> {
    pub(super) fn into_build_evidence(
        self,
    ) -> (InspectedWheelArchive, RetainedBuiltRegistryWheel<'root>) {
        let Self {
            source,
            archive,
            facts,
            record,
        } = self;
        let output_digest = archive.digest();
        (
            archive,
            RetainedBuiltRegistryWheel {
                source,
                facts,
                record,
                output_digest,
            },
        )
    }
}
impl RetainedBuiltRegistryWheel<'_> {
    pub(super) fn pin(&self) -> &ContentPin {
        self.source.pin()
    }
    pub(super) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(super) fn record(&self) -> &SourceBuildRecord {
        &self.record
    }
    pub(super) fn output_digest(&self) -> Digest {
        self.output_digest
    }
    pub(super) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        self.source.recheck_for(root)
    }
}

impl<'root, 'config> RegistryBuildContinuation<'root, 'config> {
    /// Consume real backend completion and continue the same solver attempt.
    /// Original root requirements and accumulated budgets are never reset.
    pub(crate) async fn resume_with_built(
        mut self,
        completed: CompletedSourceBuild,
    ) -> RegistryDepotResult<RegistryPreparation<'root, 'config>> {
        self.depot.check_live()?;
        self.source.recheck_for(self.depot.root)?;
        let target = super::super::locked_graph::FrozenRegistryTarget::new(
            self.depot.context.environment,
            self.depot.context.supported,
            PythonMarkerScope::Environment,
            self.depot.context.semantics,
            Default::default(),
        )
        .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        if completed.target_fingerprint() != *target.fingerprint() {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let (archive, record) = completed
            .into_parts()
            .map_err(|_| RegistryDepotError::Build)?;
        self.depot.check_live()?;
        archive
            .check_limits(self.depot.context.limits.wheel)
            .map_err(|_| RegistryDepotError::Build)?;
        record.validate().map_err(|_| RegistryDepotError::Build)?;
        let pin = self.source.pin();
        let metadata: [u8; 32] = Sha256::digest(archive.manifest().core_metadata().raw()).into();
        if pin.file().kind()
            != crate::packages::python_resolution::lock_model::RegistryArtifactKind::Sdist
            || record.source_digest != pin.digest().to_string()
            || record.output_filename != archive.filename()
            || record.output_digest != archive.digest().to_string()
            || record.output_size != archive.size()
            || record.metadata_sha256 != metadata
            || !archive
                .manifest()
                .tags()
                .iter()
                .any(|tag| self.depot.context.supported.ordered().contains(tag))
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let facts = super::super::wheel_metadata::project_manifest_metadata(
            archive.manifest(),
            pin.file().project(),
            pin.file().version(),
            self.depot.context.limits.metadata,
        )
        .map_err(|_| RegistryDepotError::Build)?;
        self.source.recheck_for(self.depot.root)?;
        self.depot.retained_bytes = self
            .depot
            .retained_bytes
            .checked_add(archive.size())
            .filter(|n| *n <= self.depot.context.limits.max_retained_bytes)
            .ok_or(RegistryDepotError::Limit)?;
        self.depot.charge(
            serde_json::to_vec(&record)
                .map_err(|_| RegistryDepotError::Build)?
                .len(),
        )?;
        let owner = RegistryMetadataOwner::Built(BuiltRegistryWheel {
            source: self.source,
            archive,
            facts,
            record,
        });
        self.depot.charge_metadata(&owner)?;
        self.depot.insert_metadata(self.candidate, owner)?;
        self.depot.check_live()?;
        self.depot.solve().await
    }
}
