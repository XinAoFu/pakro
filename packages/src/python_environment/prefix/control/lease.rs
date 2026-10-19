//! Local generation identity and the shared build/live/collection lock.

use super::*;
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::open_existing_readonly_regular;

#[derive(Debug)]
pub(crate) struct GenerationIdentity {
    allocation: String,
    store_path: PathBuf,
    store: File,
    generation: File,
    leases: File,
    build: File,
    lock: File,
}

/// Persistable identity facts only; these fields never reconstruct a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationIdentityFacts {
    pub allocation: String,
    pub store: [u64; 2],
    pub generation: [u64; 2],
    pub leases: [u64; 2],
    pub build: [u64; 2],
    pub lock: [u64; 2],
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum GenerationLeaseMode {
    Live,
    Collect,
}

#[derive(Debug)]
pub(crate) struct GenerationLease {
    identity: GenerationIdentity,
    held: ObjectLease,
}

/// An exclusive existing-lock lease for recovery after generation capture.
/// This type cannot be converted into a live lease or a prefix writer.
#[derive(Debug)]
pub(crate) struct CleanupLease {
    facts: GenerationIdentityFacts,
    store_path: PathBuf,
    store: File,
    leases: File,
    build: File,
    held: ObjectLease,
}

impl UnpublishedPrefix {
    pub(crate) fn generation_identity(&self) -> Result<GenerationIdentity> {
        self.check_binding()?;
        let leases = open_directory(&self.binding.store_root, ".leases")?;
        let build = open_directory(&leases, "build")?;
        for directory in [&leases, &build] {
            validate_owned_directory(directory, self.expected_uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "generation lease directory is not private"
            );
        }
        ensure_entry_identity(
            &self.binding.store_root,
            ".leases",
            FileIdentity::of(&leases)?,
        )?;
        ensure_entry_identity(&leases, "build", FileIdentity::of(&build)?)?;
        ensure!(
            FileIdentity::of(&build)? == FileIdentity::of(&self.binding._build_directory)?,
            "generation lease directory identity changed"
        );
        let lock_name = format!("{}.lock", self.allocation_id);
        ensure_entry_identity(
            &build,
            &lock_name,
            FileIdentity::of(&self.binding._build_lease)?,
        )?;
        validate_owned_regular(&self.binding._build_lease, self.expected_uid)?;
        ensure!(
            self.binding._build_lease.metadata()?.mode() & 0o7777 == 0o600,
            "generation lease file is not private"
        );
        Ok(GenerationIdentity {
            allocation: self.allocation_id.clone(),
            store_path: self
                .final_path
                .parent()
                .ok_or_else(|| eyre::eyre!("generation store path is absent"))?
                .to_path_buf(),
            store: self.binding.store_root.try_clone()?,
            generation: self.binding.root.try_clone()?,
            leases,
            build,
            lock: self.binding._build_lease.try_clone()?,
        })
    }
}

impl StoreControl<'_> {
    pub(crate) fn pin_generation_identity(
        &self,
        facts: &GenerationIdentityFacts,
    ) -> Result<GenerationIdentity> {
        let (leases, build, lock) = self.open_existing_generation_lock(facts)?;
        let generation = open_directory(&self.store.root, facts.allocation.as_str())?;
        ensure_identity_pair(&generation, facts.generation)?;
        let identity = GenerationIdentity {
            allocation: facts.allocation.clone(),
            store_path: self.store.root_path.clone(),
            store: self.store.root.try_clone()?,
            generation,
            leases,
            build,
            lock,
        };
        self.check_binding()?;
        identity.check_binding(self.store.expected_uid)?;
        Ok(identity)
    }

    pub(crate) fn try_cleanup_lease(
        &self,
        facts: &GenerationIdentityFacts,
    ) -> Result<Option<CleanupLease>> {
        let (leases, build, lock) = self.open_existing_generation_lock(facts)?;
        let stable = StableLock::open_existing(
            &build,
            &format!("{}.lock", facts.allocation),
            self.store.expected_uid,
            FileIdentity::of(&lock)?,
        )?;
        let Some(held) = self
            .core
            .try_object(&facts.allocation, &stable, LeaseMode::Exclusive)?
        else {
            return Ok(None);
        };
        let lease = CleanupLease {
            facts: facts.clone(),
            store_path: self.store.root_path.clone(),
            store: self.store.root.try_clone()?,
            leases,
            build,
            held,
        };
        self.check_binding()?;
        lease.check_binding()?;
        Ok(Some(lease))
    }

    fn open_existing_generation_lock(
        &self,
        facts: &GenerationIdentityFacts,
    ) -> Result<(File, File, File)> {
        validate_generation_allocation(&facts.allocation)?;
        self.check_binding()?;
        ensure_identity_pair(&self.store.root, facts.store)?;
        let leases = open_directory(&self.store.root, ".leases")?;
        let build = open_directory(&leases, "build")?;
        for directory in [&leases, &build] {
            validate_owned_directory(directory, self.store.expected_uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "generation lease directory is not private"
            );
        }
        ensure_identity_pair(&leases, facts.leases)?;
        ensure_identity_pair(&build, facts.build)?;
        ensure_entry_identity(&self.store.root, ".leases", FileIdentity::of(&leases)?)?;
        ensure_entry_identity(&leases, "build", FileIdentity::of(&build)?)?;
        let name = format!("{}.lock", facts.allocation);
        let lock = open_existing_readonly_regular(&build, &name, self.store.expected_uid)?;
        validate_owned_regular(&lock, self.store.expected_uid)?;
        ensure!(
            lock.metadata()?.mode() & 0o7777 == 0o600,
            "generation lease file is not private"
        );
        ensure_identity_pair(&lock, facts.lock)?;
        ensure_entry_identity(&build, &name, FileIdentity::of(&lock)?)?;
        Ok((leases, build, lock))
    }

    pub(crate) fn try_generation_lease(
        &self,
        expected: &GenerationIdentity,
        mode: GenerationLeaseMode,
    ) -> Result<Option<GenerationLease>> {
        self.check_binding()?;
        ensure!(
            FileIdentity::of(&self.store.root)? == FileIdentity::of(&expected.store)?,
            "generation lease store identity mismatch"
        );
        expected.check_binding(self.store.expected_uid)?;
        let name = format!("{}.lock", expected.allocation);
        let stable = StableLock::open_existing(
            &expected.build,
            &name,
            self.store.expected_uid,
            FileIdentity::of(&expected.lock)?,
        )?;
        let operation = match mode {
            GenerationLeaseMode::Live => LeaseMode::Shared,
            GenerationLeaseMode::Collect => LeaseMode::Exclusive,
        };
        let Some(held) = self
            .core
            .try_object(&expected.allocation, &stable, operation)?
        else {
            return Ok(None);
        };
        let lease = GenerationLease {
            identity: expected.pin()?,
            held,
        };
        self.check_binding()?;
        lease.check_binding()?;
        Ok(Some(lease))
    }
}

impl CleanupLease {
    pub(crate) fn facts(&self) -> &GenerationIdentityFacts {
        &self.facts
    }

    pub(crate) fn check_binding(&self) -> Result<()> {
        let uid = nix::unistd::geteuid().as_raw();
        let current = open_trusted_store(&self.store_path, uid, false)?;
        ensure_identity_pair(&current, self.facts.store)?;
        ensure_identity_pair(&self.store, self.facts.store)?;
        for directory in [&self.leases, &self.build] {
            validate_owned_directory(directory, uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "generation lease directory is not private"
            );
        }
        ensure_identity_pair(&self.leases, self.facts.leases)?;
        ensure_identity_pair(&self.build, self.facts.build)?;
        ensure_entry_identity(&self.store, ".leases", FileIdentity::of(&self.leases)?)?;
        ensure_entry_identity(&self.leases, "build", FileIdentity::of(&self.build)?)?;
        self.held.check_binding()?;
        let identity = self.held.identity();
        ensure!(
            [identity.device as u64, identity.inode as u64] == self.facts.lock,
            "generation lease identity facts mismatch"
        );
        ensure_entry_identity(
            &self.build,
            &format!("{}.lock", self.facts.allocation),
            identity,
        )?;
        Ok(())
    }
}

fn validate_generation_allocation(allocation: &str) -> Result<()> {
    ensure!(
        allocation.len() == 34
            && allocation.starts_with("g-")
            && allocation.as_bytes()[2..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "invalid generation lease allocation identifier"
    );
    Ok(())
}

fn ensure_identity_pair(file: &File, expected: [u64; 2]) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        [metadata.dev(), metadata.ino()] == expected,
        "generation lease identity facts mismatch"
    );
    Ok(())
}

impl GenerationIdentity {
    pub(crate) fn facts(&self) -> Result<GenerationIdentityFacts> {
        self.check_binding(nix::unistd::geteuid().as_raw())?;
        let pair = |file: &File| -> Result<[u64; 2]> {
            let metadata = file.metadata()?;
            Ok([metadata.dev(), metadata.ino()])
        };
        Ok(GenerationIdentityFacts {
            allocation: self.allocation.clone(),
            store: pair(&self.store)?,
            generation: pair(&self.generation)?,
            leases: pair(&self.leases)?,
            build: pair(&self.build)?,
            lock: pair(&self.lock)?,
        })
    }

    fn check_binding(&self, uid: u32) -> Result<()> {
        let store = open_trusted_store(&self.store_path, uid, false)?;
        ensure!(
            FileIdentity::of(&store)? == FileIdentity::of(&self.store)?,
            "generation lease store identity changed"
        );
        for directory in [&self.generation, &self.leases, &self.build] {
            validate_owned_directory(directory, uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "generation lease directory is not private"
            );
        }
        ensure_entry_identity(
            &self.store,
            &self.allocation,
            FileIdentity::of(&self.generation)?,
        )?;
        ensure_entry_identity(&self.store, ".leases", FileIdentity::of(&self.leases)?)?;
        ensure_entry_identity(&self.leases, "build", FileIdentity::of(&self.build)?)?;
        validate_owned_regular(&self.lock, uid)?;
        ensure!(
            self.lock.metadata()?.mode() & 0o7777 == 0o600,
            "generation lease file is not private"
        );
        ensure_entry_identity(
            &self.build,
            &format!("{}.lock", self.allocation),
            FileIdentity::of(&self.lock)?,
        )?;
        Ok(())
    }
    fn pin(&self) -> Result<Self> {
        Ok(Self {
            allocation: self.allocation.clone(),
            store_path: self.store_path.clone(),
            store: self.store.try_clone()?,
            generation: self.generation.try_clone()?,
            leases: self.leases.try_clone()?,
            build: self.build.try_clone()?,
            lock: self.lock.try_clone()?,
        })
    }
}

impl GenerationLease {
    pub(crate) fn check_binding(&self) -> Result<()> {
        self.identity
            .check_binding(nix::unistd::geteuid().as_raw())?;
        self.held.check_binding()?;
        ensure!(
            self.held.identity() == FileIdentity::of(&self.identity.lock)?,
            "generation lease held file identity changed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reopen_tests;
