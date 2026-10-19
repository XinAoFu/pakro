//! Real observations authorize publication only together with their live owner.

use super::lease::LanguageLease;
use super::manifest::{DiskIdentity, LanguageManifest, LockRegistration};
use super::repository::{
    LanguageBuildOwner, LanguageInstallSpec, LanguageRepository, manifest_name, registration_name,
};
use crate::packages::python_environment::prefix::verify::{TreeInventory, TreeInventoryKind};
use crate::packages::python_environment::runtime_target::freeze_observed_target;
use crate::packages::python_resolution::source::locked_graph::{
    FrozenRegistryTarget, FrozenTargetLimits,
};
use crate::plugins::core::python::native::extract::NativeExtractionReceipt;
use crate::python_runtime::{PythonRuntimeHandle, probe_at_deadline};
use eyre::{Context, ContextCompat, Result, ensure};
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode;
use pakro_index::python_markers::{MarkerSemantics, PythonMarkerScope};
use pakro_store::control::{
    ControlLimits, ControlNamespace, FileIdentity, ensure_entry_identity, open_directory,
};
use std::path::{Component, Path};

pub(crate) struct VerifiedLanguage {
    owner: LanguageBuildOwner,
    manifest: LanguageManifest,
}

pub(crate) async fn verify_language(
    owner: LanguageBuildOwner,
    receipt: NativeExtractionReceipt,
) -> Result<VerifiedLanguage> {
    receipt.validate_owner(&owner)?;
    let inventory = owner.capture_inventory()?;
    ensure!(
        &inventory == receipt.inventory(),
        "native extraction inventory changed before verification"
    );
    let executable = receipt.descriptor().executable.clone();
    let relative = executable
        .strip_prefix(owner.final_path())
        .context("runtime entrypoint is outside its reserved payload")?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && inventory.contains_file(relative),
        "runtime entrypoint is not a regular file in its inventory"
    );
    let handle = PythonRuntimeHandle::from_existing_executable(&executable)?;
    ensure!(
        handle.executable() == executable,
        "runtime entrypoint changed during canonical binding"
    );
    let descriptor = tokio::select! {
        _ = owner.cancellation_token().cancelled_owned() => return Err(eyre::eyre!("language verification cancelled")),
        result = probe_at_deadline(&handle, owner.deadline().into(), 1024 * 1024, 64 * 1024) => result?,
    };
    owner.checkpoint()?;
    owner.spec.plan().validate_observed(&descriptor)?;
    ensure!(
        descriptor == *receipt.descriptor(),
        "runtime observations changed after extraction"
    );
    ensure!(
        owner.capture_inventory()? == inventory,
        "runtime changed while its process was being verified"
    );
    let manifest = LanguageManifest {
        schema: 1,
        key: owner.spec.key().fingerprint(),
        target: owner.spec.key().target(),
        artifact: owner.spec.plan().source().digest().to_string(),
        filename: owner.spec.plan().source().filename().to_owned(),
        source: owner.spec.plan().source().url().to_string(),
        patch_sysconfig: owner.spec.patch_sysconfig(),
        lock: DiskIdentity::from_file_identity(owner.lock.identity()),
        allocation: DiskIdentity::from_file_identity(FileIdentity::of(&owner.allocation)?),
        payload: DiskIdentity::from_file_identity(FileIdentity::of(&owner.root)?),
        entrypoint: relative.to_owned(),
        inventory,
        descriptor,
    };
    validate_manifest(&owner.spec, &manifest)?;
    validate_manifest_location(&manifest, owner.final_path())?;
    owner.checkpoint()?;
    Ok(VerifiedLanguage { owner, manifest })
}

pub(super) fn validate_manifest(
    spec: &LanguageInstallSpec,
    manifest: &LanguageManifest,
) -> Result<FrozenRegistryTarget> {
    manifest.lock.validate()?;
    manifest.allocation.validate()?;
    manifest.payload.validate()?;
    ensure!(
        manifest.schema == 1
            && manifest.key == spec.key().fingerprint()
            && manifest.target == spec.key().target(),
        "language manifest identity does not match its requested key"
    );
    ensure!(
        manifest.artifact == spec.plan().source().digest().to_string()
            && manifest.filename == spec.plan().source().filename()
            && manifest.patch_sysconfig == spec.patch_sysconfig(),
        "language manifest source or recipe differs from the request"
    );
    spec.plan().validate_source_locator(&manifest.source)?;
    ensure!(
        !manifest.entrypoint.as_os_str().is_empty()
            && !manifest.entrypoint.is_absolute()
            && manifest
                .entrypoint
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && manifest.inventory.contains_file(&manifest.entrypoint),
        "language manifest entrypoint is not contained in its inventory"
    );
    spec.plan().validate_observed(&manifest.descriptor)?;
    freeze_observed_target(
        &manifest.descriptor,
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        FrozenTargetLimits::default(),
    )
    .map_err(Into::into)
}

pub(super) fn validate_manifest_location(manifest: &LanguageManifest, root: &Path) -> Result<()> {
    ensure!(
        manifest.descriptor.executable == root.join(&manifest.entrypoint),
        "language executable belongs to another payload"
    );
    let stdlib = Path::new(
        manifest
            .descriptor
            .sysconfig_paths
            .get("stdlib")
            .context("observed runtime standard library is missing")?,
    );
    let relative = stdlib
        .strip_prefix(root)
        .context("observed standard library is outside the reserved runtime")?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && manifest
                .inventory
                .entries()
                .iter()
                .any(|entry| entry.relative == relative
                    && matches!(entry.kind, TreeInventoryKind::Directory)),
        "observed standard library is not a directory in the complete runtime"
    );
    Ok(())
}

fn sync_payload(owner: &LanguageBuildOwner, inventory: &TreeInventory) -> Result<()> {
    for entry in inventory.entries().iter().rev() {
        owner.checkpoint()?;
        let parent = owner.open_parent(&entry.relative)?;
        let name = entry
            .relative
            .file_name()
            .and_then(|name| name.to_str())
            .context("language inventory path is not UTF-8")?;
        let identity = FileIdentity {
            device: entry.identity[0] as _,
            inode: entry.identity[1] as _,
        };
        ensure_entry_identity(&parent, name, identity)?;
        match entry.kind {
            TreeInventoryKind::File { .. } => {
                let file = std::fs::File::from(openat(
                    &parent,
                    name,
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                    Mode::empty(),
                )?);
                ensure!(
                    FileIdentity::of(&file)? == identity,
                    "language file changed before durability sync"
                );
                file.sync_all()?;
            }
            TreeInventoryKind::Directory => open_directory(&parent, name)?.sync_all()?,
            TreeInventoryKind::Symlink { .. } => {}
        }
        parent.sync_all()?;
    }
    owner.root.sync_all()?;
    owner.allocation.sync_all()?;
    owner.writer.root_handle().sync_all()?;
    owner.checkpoint()
}

pub(crate) fn publish_language(verified: VerifiedLanguage) -> Result<LanguageLease> {
    publish_language_inner(verified, || Ok(()))
}
#[cfg(test)]
pub(super) fn publish_language_with_after_cas(
    verified: VerifiedLanguage,
    after_cas: impl FnOnce() -> Result<()>,
) -> Result<LanguageLease> {
    publish_language_inner(verified, after_cas)
}
fn publish_language_inner(
    verified: VerifiedLanguage,
    after_cas: impl FnOnce() -> Result<()>,
) -> Result<LanguageLease> {
    let VerifiedLanguage { owner, manifest } = verified;
    owner.checkpoint()?;
    ensure!(
        owner.capture_inventory()? == manifest.inventory,
        "verified runtime inventory changed before publication"
    );
    sync_payload(&owner, &manifest.inventory)?;
    let mut control = owner.writer.control(ControlLimits::default())?;
    let registration = control
        .read(ControlNamespace::Ownership, &registration_name(&owner.spec))?
        .context("language publication lock registration is missing")?;
    let registration: LockRegistration = serde_json::from_slice(registration.bytes())?;
    ensure!(
        registration.schema == 1
            && registration.key == owner.spec.key().fingerprint()
            && registration.lock == manifest.lock,
        "language publication lock registration changed"
    );
    ensure!(
        control
            .read(ControlNamespace::Catalog, &manifest_name(&owner.spec))?
            .is_none(),
        "language key is already published"
    );
    owner.checkpoint()?;
    ensure!(
        owner.capture_inventory()? == manifest.inventory,
        "language content changed at publication"
    );
    // From the first operation which may expose Ready, even an error can mean
    // rename succeeded and a later fsync/check failed. Drop must retain payload.
    owner.retain_allocation.set(true);
    control.replace(
        ControlNamespace::Catalog,
        &manifest_name(&owner.spec),
        None,
        &serde_json::to_vec(&manifest)?,
    )?;
    after_cas()?;
    owner.checkpoint()?;
    let spec = owner.spec.clone();
    let repository = LanguageRepository::open_readonly(&owner.context)?;
    drop(control);
    drop(owner);
    repository
        .acquire_existing(&spec)?
        .context("published language disappeared before acquiring its use lease")
}
