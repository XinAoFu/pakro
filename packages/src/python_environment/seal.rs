//! Local closure of the real writer capability after complete D4 verification.
//! Durable publication additionally requires runtime protection and catalog CAS.

use std::path::Path;

use eyre::{Context, Result, ensure};

use super::prefix::UnpublishedPrefix;
use super::prefix::control::lease::{GenerationIdentity, GenerationIdentityFacts};
use super::prefix::verify::{PrefixInventoryLimits, verify_prefix_inventory};
use super::runtime_startup::EmptyEnvironmentLayout;
use super::runtime_startup::installed::EnvironmentVerificationReceipt;
use crate::packages::python_wheel_install::WheelMaterializationReceipt;

pub(crate) struct LocallySealedEnvironment {
    prefix: UnpublishedPrefix,
    layout: EmptyEnvironmentLayout,
    wheels: WheelMaterializationReceipt,
    verified: EnvironmentVerificationReceipt,
    identity: GenerationIdentity,
}

impl std::fmt::Debug for LocallySealedEnvironment {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("LocallySealedEnvironment")
            .field("allocation", &self.prefix.allocation_id())
            .field("files", &self.verified.complete_files().len())
            .finish_non_exhaustive()
    }
}

impl LocallySealedEnvironment {
    pub(crate) fn allocation_id(&self) -> &str {
        self.prefix.allocation_id()
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        self.prefix.final_path()
    }
    pub(crate) fn verification(&self) -> &EnvironmentVerificationReceipt {
        &self.verified
    }
    pub(crate) fn identity_facts(&self) -> Result<GenerationIdentityFacts> {
        self.identity.facts()
    }
    pub(crate) fn recheck(&self, limits: PrefixInventoryLimits) -> Result<()> {
        ensure!(
            self.layout.allocation_id() == self.allocation_id()
                && self.wheels.allocation_id() == self.allocation_id()
                && self.verified.allocation_id() == self.allocation_id(),
            "seal allocation evidence differs"
        );
        self.identity.facts()?;
        let actual = verify_prefix_inventory(
            &self.prefix,
            self.verified.complete_files(),
            self.verified.complete_directories(),
            limits,
        )
        .context("seal inventory changed")?;
        ensure!(
            actual.inventory_sha256() == self.verified.inventory_sha256(),
            "seal inventory digest changed"
        );
        Ok(())
    }
}

pub(crate) fn seal_verified_environment(
    layout: EmptyEnvironmentLayout,
    wheels: WheelMaterializationReceipt,
    prefix: UnpublishedPrefix,
    verified: EnvironmentVerificationReceipt,
    limits: PrefixInventoryLimits,
) -> Result<LocallySealedEnvironment> {
    ensure!(
        layout.allocation_id() == prefix.allocation_id()
            && wheels.allocation_id() == prefix.allocation_id()
            && verified.allocation_id() == prefix.allocation_id()
            && layout.final_prefix() == prefix.final_path()
            && wheels.final_prefix() == prefix.final_path()
            && verified.final_prefix() == prefix.final_path(),
        "seal allocation evidence differs"
    );
    ensure!(
        verified.sources() == wheels.sources()
            && verified.files() == wheels.files()
            && verified.directories() == wheels.directories(),
        "seal receipt coverage differs"
    );
    let identity = prefix
        .generation_identity()
        .context("seal inventory is unavailable")?;
    let sealed = LocallySealedEnvironment {
        prefix,
        layout,
        wheels,
        verified,
        identity,
    };
    sealed.recheck(limits)?;
    Ok(sealed)
}
