//! Ordinary installed wheel metadata held by its actual package and lock.
use super::*;
use crate::libraries::package::lease::PackageLease;
use crate::packages::python_resolution::lock_session::ExistingFileSlot;

pub(crate) struct HeldRegistryWheel<'root> {
    slot: ExistingFileSlot<'root>,
    package: PackageLease,
    metadata: crate::packages::python_wheel::WheelCoreMetadata,
    facts: PythonDependencyMetadataFacts,
}
impl<'root> HeldRegistryWheel<'root> {
    pub(super) fn new(
        slot: ExistingFileSlot<'root>,
        package: PackageLease,
        language: [u8; 32],
    ) -> RegistryDepotResult<Self> {
        slot.recheck().map_err(|_| RegistryDepotError::Revoked)?;
        package
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        let pin = slot.pin();
        let manifest = package.manifest();
        if manifest.conda.is_some()
            || manifest.language != Some(language)
            || manifest.facts.source != hex::encode(pin.id().as_bytes())
            || manifest.facts.artifact != pin.digest().to_string()
            || manifest.filename != pin.file().filename()
            || manifest.name() != pin.file().project()
        {
            return Err(RegistryDepotError::IdentityMismatch);
        }
        let bytes = package
            .read_wheel_metadata(WheelLimits::default().max_metadata_bytes)
            .map_err(|_| RegistryDepotError::Revoked)?;
        let metadata = crate::packages::python_wheel::parse_installed_core_metadata(
            bytes,
            pin.file().project(),
            pin.file().version(),
            WheelLimits::default(),
        )
        .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        let facts = super::super::wheel_metadata::project_core_metadata(
            &metadata,
            pin.file().project(),
            pin.file().version(),
            PythonMetadataLimits::default(),
        )
        .map_err(|_| RegistryDepotError::IdentityMismatch)?;
        package
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)?;
        Ok(Self {
            slot,
            package,
            metadata,
            facts,
        })
    }
    pub(super) fn pin(&self) -> &ContentPin {
        self.slot.pin()
    }
    pub(super) fn metadata(&self) -> &crate::packages::python_wheel::WheelCoreMetadata {
        &self.metadata
    }
    pub(super) fn facts(&self) -> &PythonDependencyMetadataFacts {
        &self.facts
    }
    pub(super) fn package(&self) -> &PackageLease {
        &self.package
    }
    pub(super) fn recheck_for(&self, root: &BoundPythonLock) -> RegistryDepotResult<()> {
        self.slot
            .recheck_for(root)
            .map_err(|_| RegistryDepotError::Revoked)?;
        self.package
            .check_binding()
            .map_err(|_| RegistryDepotError::Revoked)
    }
}
