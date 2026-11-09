//! Resume one original P2 package-preparation record using its real owners.
use super::*;
use crate::libraries::package::{lease, manifest};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, observe_tree_inventory,
};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    FileIdentity, ensure_entry_identity, open_directory, open_trusted_store_with_deadline,
    validate_owned_directory,
};
use std::{collections::BTreeSet, fs::File};

struct OriginalPackage<'a> {
    context: &'a OperationContext,
    store: &'a File,
    leases: &'a File,
    build: &'a File,
    root: File,
    path: PathBuf,
    manifest: &'a manifest::PackageManifest,
    namespace: &'a PreparingPackages,
    lock: StableLock,
    held: ObjectLease,
}
impl InventoryRoot for OriginalPackage<'_> {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        &self.manifest.storage.allocation
    }
    fn expected_uid(&self) -> u32 {
        lease::uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.context.checkpoint()?;
        let current = open_trusted_store_with_deadline(
            &manifest::store_path(self.context),
            lease::uid(),
            false,
            self.context.policy().deadline,
        )?;
        ensure!(
            lease::pair(&current)? == self.namespace.store
                && lease::pair(self.store)? == self.namespace.store
                && lease::pair(self.leases)? == self.namespace.leases
                && lease::pair(self.build)? == self.namespace.build,
            "PendingRecovery: package store or lock namespace changed"
        );
        for directory in [self.leases, self.build, &self.root] {
            validate_owned_directory(directory, lease::uid())?;
        }
        ensure_entry_identity(self.store, ".leases", FileIdentity::of(self.leases)?)?;
        ensure_entry_identity(self.leases, "build", FileIdentity::of(self.build)?)?;
        self.lock.check_location(
            self.build,
            &format!("{}.lock", self.manifest.storage.allocation),
        )?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive
                && self.held.identity() == self.lock.identity(),
            "PendingRecovery: package recovery lacks original exclusive protection"
        );
        ensure_entry_identity(
            self.store,
            &self.manifest.storage.allocation,
            lease::identity(self.manifest.storage.root_identity),
        )?;
        ensure!(
            lease::pair(&self.root)? == self.manifest.storage.root_identity,
            "PendingRecovery: package allocation changed"
        );
        Ok(())
    }
}

/// Validate every original member before making any additional member Ready.
/// The project lock remains held by the caller throughout this operation.
pub(super) fn resume(context: &OperationContext, journal: &Journal) -> Result<()> {
    let group = journal
        .preparing_packages
        .as_ref()
        .context("package preparation is missing")?;
    let package_path = manifest::store_path(context);
    let store = open_trusted_store_with_deadline(
        &package_path,
        lease::uid(),
        false,
        context.policy().deadline,
    )?;
    let leases = open_directory(&store, ".leases")?;
    let build = open_directory(&leases, "build")?;
    ensure!(
        lease::pair(&store)? == group.store
            && lease::pair(&leases)? == group.leases
            && lease::pair(&build)? == group.build,
        "PendingRecovery: package store identity changed"
    );
    let member_keys = group
        .members
        .iter()
        .map(|item| item.key())
        .collect::<BTreeSet<_>>();
    let external = group
        .members
        .iter()
        .flat_map(|item| item.facts.dependencies.iter().copied())
        .filter(|key| !member_keys.contains(key))
        .collect::<BTreeSet<_>>();
    let mut held_dependencies = Vec::new();
    for key in external {
        let dependency = lease::acquire_package(context, key)?
            .context("PendingRecovery: external package dependency is missing")?;
        dependency.check_binding()?;
        held_dependencies.push(dependency);
    }
    let languages = group
        .members
        .iter()
        .filter_map(|item| item.language)
        .collect::<BTreeSet<_>>();
    let mut held_languages = Vec::new();
    if !languages.is_empty() {
        let repository =
            crate::libraries::language::repository::LanguageRepository::open_readonly(context)?;
        let available = repository.enumerate_published().map_err(|error| {
            eyre::eyre!("PendingRecovery: language enumeration failed: {error:?}")
        })?;
        for key in languages {
            let language = available
                .iter()
                .find(|item| item.key().fingerprint() == key)
                .context("PendingRecovery: required Python is missing")?;
            let held = repository
                .acquire_by_key(&language.key())?
                .context("PendingRecovery: required Python is not Ready")?;
            held.check_binding()?;
            held_languages.push(held);
        }
    }
    let mut ordered = group.members.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| item.key());
    let mut owners = Vec::new();
    for member in ordered {
        let lock = StableLock::open_existing(
            &build,
            &format!("{}.lock", member.storage.allocation),
            lease::uid(),
            lease::identity(member.storage.lock),
        )?;
        let held = lock.acquire(LeaseMode::Exclusive, context.policy().deadline)?;
        let root = open_directory(&store, member.storage.allocation.as_str())?;
        let owner = OriginalPackage {
            context,
            store: &store,
            leases: &leases,
            build: &build,
            root,
            path: package_path.join(&member.storage.allocation),
            manifest: member,
            namespace: group,
            lock,
            held,
        };
        owner.check_binding()?;
        ensure!(
            observe_tree_inventory(&owner, PrefixInventoryLimits::default())?
                == member.storage.inventory,
            "PendingRecovery: original package inventory changed"
        );
        lease::validate_package_facts(&owner.root, member)?;
        owners.push(owner);
    }
    let writer = StoreWriter::open_existing(
        &package_path,
        lease::uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?;
    ensure!(
        lease::pair(writer.root_handle())? == group.store,
        "PendingRecovery: package writer store changed"
    );
    for owner in &owners {
        owner.check_binding()?;
        let bytes = serde_json::to_vec(owner.manifest)?;
        let mut control = writer.control(ControlLimits::default())?;
        let name = manifest::package_name(&owner.manifest.key());
        if let Some(existing) = control.read(ControlNamespace::Catalog, &name)? {
            ensure!(
                existing.bytes() == bytes,
                "PendingRecovery: Ready package differs from original prepared member"
            );
        } else {
            ensure!(
                control
                    .read(ControlNamespace::Journals, &format!("deleting-{name}"))?
                    .is_none(),
                "PendingRecovery: prepared package has an unfinished deletion; run pakro prune before installing again"
            );
            control.replace(ControlNamespace::Catalog, &name, None, &bytes)?;
        }
    }
    for owner in &owners {
        owner.check_binding()?;
    }
    for dependency in &held_dependencies {
        dependency.check_binding()?;
    }
    for language in &held_languages {
        language.check_binding()?;
    }
    Ok(())
}
