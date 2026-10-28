//! Generated wheels retain their original owned source and actual execution
//! receipt. No generated file is relabelled as a registry wheel selection.
use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;
use crate::packages::python_source_build::CompletedSourceBuild;
use crate::packages::python_wheel_source::InspectedWheelArchive;
use pakro_resolver::solve::{Pep440, VersionAdapter};
use sha2::{Digest as _, Sha256};
use std::io::{Read, Seek, SeekFrom};

pub(crate) struct PublishedRegistryWheel<'root> {
    source: BuildOwner<'root>,
    package: crate::libraries::package::lease::PackageLease,
    language: crate::libraries::language::lease::LanguageLease,
    facts: PythonDependencyMetadataFacts,
    metadata: crate::packages::python_wheel::WheelCoreMetadata,
    record: SourceBuildRecord,
    output_digest: Digest,
}
impl PublishedRegistryWheel<'_> {
    pub(super) fn pin(&self) -> &ContentPin {
        self.source.pin()
    }
    pub(super) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(super) fn metadata(&self) -> &crate::packages::python_wheel::WheelCoreMetadata {
        &self.metadata
    }
    pub(super) fn record(&self) -> &SourceBuildRecord {
        &self.record
    }
    pub(super) fn output_digest(&self) -> Digest {
        self.output_digest
    }
    pub(super) fn package(&self) -> &crate::libraries::package::lease::PackageLease {
        &self.package
    }
    pub(super) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        self.source.recheck_for(root)?;
        self.language
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        self.package
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        if self.package.manifest().language != self.language.key().fingerprint()
            || self.package.manifest().facts.source != hex::encode(self.pin().id().as_bytes())
            || self.package.manifest().facts.artifact != self.record.output_digest
            || self.package.manifest().filename != self.record.output_filename
            || root.source_build(self.pin().id()) != Some(&self.record)
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        self.source.recheck_for(root)
    }
    pub(super) fn into_parts(self) -> (crate::libraries::package::lease::PackageLease, Self) {
        (self.package.clone(), self)
    }
}
impl<'root> PublishedRegistryWheel<'root> {
    pub(in crate::packages::python_resolution::source) fn from_locked_published(
        source: super::super::replay::LockedRegistryArtifact<'root>,
        package: crate::libraries::package::lease::PackageLease,
        language: crate::libraries::language::lease::LanguageLease,
        root: &'root BoundPythonLock,
        target: &super::super::locked_graph::FrozenRegistryTarget,
        wheel_limits: crate::packages::python_wheel::WheelLimits,
        metadata_limits: PythonMetadataLimits,
    ) -> RegistryDepotResult<Self> {
        use crate::packages::python_resolution::lock_model::RuntimePin;
        source
            .recheck_for(root)
            .map_err(|_| RegistryDepotError::Revoked)?;
        language
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        package
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        let runtime = RuntimePin::new(
            language.key().fingerprint(),
            1,
            *language.target().fingerprint(),
        )
        .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let pin = source.pin();
        let expected = root
            .source_build(pin.id())
            .ok_or(RegistryDepotError::Build)?
            .clone();
        expected.validate().map_err(|_| RegistryDepotError::Build)?;
        let model = root.model().ok_or(RegistryDepotError::IdentityMismatch)?;
        if target.fingerprint() != language.target().fingerprint()
            || !model.graphs().iter().any(|graph| {
                graph.key().config_scope().is_empty()
                    && graph.key().group() == "shared-runtime"
                    && graph.runtime() == runtime
                    && graph
                        .nodes()
                        .iter()
                        .any(|node| node.source_pin() == pin.id())
            })
            || pin.file().kind()
                != crate::packages::python_resolution::lock_model::RegistryArtifactKind::Sdist
            || expected.source_digest != pin.digest().to_string()
            || package.manifest().language != language.key().fingerprint()
            || package.manifest().facts.source != hex::encode(pin.id().as_bytes())
            || package.manifest().name() != pin.file().project()
            || !matches!((Pep440::parse_version(package.manifest().version()), Pep440::parse_version(pin.file().version())), (Some(left), Some(right)) if left == right)
            || package.manifest().facts.artifact != expected.output_digest
            || package.manifest().filename != expected.output_filename
            || expected.output_size > wheel_limits.max_archive_bytes
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let filename = pakro_index::parse_wheel_filename(&expected.output_filename)
            .ok_or(RegistryDepotError::IdentityMismatch)?;
        if target
            .supported()
            .best_rank(&filename.tags)
            .map_err(|_| RegistryDepotError::IdentityMismatch)?
            .is_none()
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let metadata = package
            .read_wheel_metadata(wheel_limits.max_metadata_bytes)
            .map_err(|_| RegistryDepotError::Build)?;
        if <[u8; 32]>::from(Sha256::digest(&metadata)) != expected.metadata_sha256 {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let metadata = crate::packages::python_wheel::parse_installed_core_metadata(
            metadata,
            pin.file().project(),
            pin.file().version(),
            wheel_limits,
        )
        .map_err(|_| RegistryDepotError::Build)?;
        let facts = super::super::wheel_metadata::project_core_metadata(
            &metadata,
            pin.file().project(),
            pin.file().version(),
            metadata_limits,
        )
        .map_err(|_| RegistryDepotError::Build)?;
        let output_digest = expected
            .output_digest
            .parse()
            .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let owner = Self {
            source: BuildOwner::Locked(source),
            package,
            language,
            facts,
            metadata,
            record: expected,
            output_digest,
        };
        owner.recheck_for(root)?;
        Ok(owner)
    }
}

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
impl<'root> BuiltRegistryWheel<'root> {
    pub(in crate::packages::python_resolution::source) fn from_locked_output(
        source: super::super::replay::LockedRegistryArtifact<'root>,
        archive: InspectedWheelArchive,
        record: SourceBuildRecord,
        metadata_limits: PythonMetadataLimits,
    ) -> RegistryDepotResult<Self> {
        source.recheck().map_err(|_| RegistryDepotError::Revoked)?;
        record.validate().map_err(|_| RegistryDepotError::Build)?;
        let pin = source.pin();
        let metadata: [u8; 32] = Sha256::digest(archive.manifest().core_metadata().raw()).into();
        if record.source_digest != pin.digest().to_string()
            || record.output_filename != archive.filename()
            || record.output_digest != archive.digest().to_string()
            || record.output_size != archive.size()
            || record.metadata_sha256 != metadata
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let facts = super::super::wheel_metadata::project_manifest_metadata(
            archive.manifest(),
            pin.file().project(),
            pin.file().version(),
            metadata_limits,
        )
        .map_err(|_| RegistryDepotError::Build)?;
        Ok(Self {
            source: BuildOwner::Locked(source),
            archive,
            facts,
            record,
        })
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
    /// Resume with one genuinely published package. The old source receipt
    /// describes its original ZIP; the lease proves the installed tree. No
    /// tree hash or synthesized archive is substituted for that ZIP digest.
    pub(crate) async fn resume_with_published(
        mut self,
        package: crate::libraries::package::lease::PackageLease,
        language: crate::libraries::language::lease::LanguageLease,
    ) -> RegistryDepotResult<RegistryPreparation<'root, 'config>> {
        self.depot.check_live()?;
        let target = super::super::locked_graph::FrozenRegistryTarget::new(
            self.depot.context.environment,
            self.depot.context.supported,
            PythonMarkerScope::Environment,
            self.depot.context.semantics,
            Default::default(),
        )
        .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let BuildOwner::Locked(source) = self.source else {
            return Err(RegistryDepotError::IdentityMismatch);
        };
        let published = PublishedRegistryWheel::from_locked_published(
            source,
            package,
            language,
            self.depot.root,
            &target,
            self.depot.context.limits.wheel,
            self.depot.context.limits.metadata,
        )?;
        let output_size = published.record().output_size;
        let owner = RegistryMetadataOwner::Published(published);
        owner.recheck_for(self.depot.root)?;
        self.depot.retained_bytes = self
            .depot
            .retained_bytes
            .checked_add(output_size)
            .filter(|n| *n <= self.depot.context.limits.max_retained_bytes)
            .ok_or(RegistryDepotError::Limit)?;
        self.depot.charge_metadata(&owner)?;
        self.depot.insert_metadata(self.candidate, owner)?;
        self.depot.solve().await
    }

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
