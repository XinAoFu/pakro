//! Closed cleanup authority. Data-only journal facts cannot construct this type.

use super::*;
use crate::packages::python_environment::catalog::read_catalog_metadata;
use crate::packages::python_environment::model::CatalogCodecLimits;
use crate::packages::python_environment::prefix::control::lease::GenerationIdentityFacts;
use crate::packages::python_environment::prefix::control::ownership::events::{
    OwnershipEntryFacts, OwnershipRecoveryPhase, OwnershipRecoveryState, validate_ownership_events,
};
use crate::packages::python_environment::prefix::control::scan::NamespaceScanLimits;
use std::os::unix::ffi::OsStrExt;

#[path = "cleanup/tree.rs"]
pub(super) mod tree;

enum Position {
    Final,
    Captured {
        directory: File,
        name: String,
    },
    RootMissing {
        directory: Option<File>,
        name: String,
    },
}

pub(super) enum CleanupCheckpoint<'a> {
    BeforeCapture,
    Captured,
    CapturedDurable,
    BeforeUnlink(&'a Path),
    Unlinked(&'a Path),
    RootUnlinked,
    Complete,
}

enum Authority<'build> {
    Building(&'build Flock<File>),
    Restart(super::super::control::lease::CleanupLease),
}

pub(in super::super) struct CleanupWitness<'control, 'store, 'build> {
    control: &'control StoreControl<'store>,
    authority: Authority<'build>,
    facts: GenerationIdentityFacts,
    nonce: uuid::Uuid,
    store: File,
    root: Option<File>,
    leases: File,
    build_directory: File,
    position: Position,
    expected_uid: u32,
}

fn pair(file: &File) -> Result<[u64; 2]> {
    let metadata = file.metadata()?;
    Ok([metadata.dev(), metadata.ino()])
}
fn missing(parent: &File, name: &str) -> Result<()> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) => Ok(()),
        Ok(_) => bail!("cleanup namespace unexpectedly contains another entry"),
        Err(error) => Err(error.into()),
    }
}
fn optional_directory(parent: &File, name: &str) -> Result<Option<File>> {
    match open_directory(parent, name) {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::ENOENT) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl<'control, 'store, 'build> CleanupWitness<'control, 'store, 'build> {
    fn original(
        control: &'control StoreControl<'store>,
        prefix: &'build UnpublishedPrefix,
        journal: &JournalRecord,
    ) -> Result<Self> {
        prefix.check_namespace()?;
        let facts = journal
            .generation()
            .ok_or_else(|| eyre::eyre!("cleanup requires a generation journal"))?
            .identity()
            .clone();
        let witness = Self {
            control,
            authority: Authority::Building(&prefix.binding._build_lease),
            facts,
            nonce: journal.nonce(),
            store: prefix.binding.store_root.try_clone()?,
            root: Some(prefix.binding.root.try_clone()?),
            leases: open_directory(&prefix.binding.store_root, ".leases")?,
            build_directory: prefix.binding._build_directory.try_clone()?,
            position: Position::Final,
            expected_uid: prefix.expected_uid,
        };
        ensure!(
            prefix.allocation_id() == witness.facts.allocation,
            "cleanup allocation identity differs"
        );
        witness.check_stream(journal.nonce(), &witness.facts, &witness.store)?;
        Ok(witness)
    }

    pub(super) fn restart(
        control: &'control StoreControl<'store>,
        lease: super::super::control::lease::CleanupLease,
        journal: &JournalRecord,
        state: &OwnershipRecoveryState,
    ) -> Result<Self> {
        let (store, _path, expected_uid) = control.recovery_store_anchor()?;
        let facts = journal
            .generation()
            .ok_or_else(|| eyre::eyre!("cleanup requires a generation journal"))?
            .identity()
            .clone();
        ensure!(lease.facts() == &facts, "cleanup lease and journal differ");
        lease.check_binding()?;
        let final_root = optional_directory(&store, &facts.allocation)?;
        let (root, position) = match state.capture() {
            None => {
                ensure!(
                    state.phase() == OwnershipRecoveryPhase::Building,
                    "cleanup progress has no capture identity"
                );
                let root = final_root
                    .ok_or_else(|| eyre::eyre!("missing root without durable capture progress"))?;
                (Some(root), Position::Final)
            }
            Some(capture) => {
                let name = std::str::from_utf8(capture.quarantine())
                    .map_err(|_| eyre::eyre!("invalid quarantine identifier"))?
                    .to_owned();
                let directory = optional_directory(&store, ".quarantine")?;
                let captured = match &directory {
                    Some(directory) => optional_directory(directory, &name)?,
                    None => None,
                };
                match (final_root, captured) {
                    (Some(root), None)
                        if state.phase() == OwnershipRecoveryPhase::CapturePending =>
                    {
                        (Some(root), Position::Final)
                    }
                    (None, Some(root))
                        if matches!(
                            state.phase(),
                            OwnershipRecoveryPhase::CapturePending
                                | OwnershipRecoveryPhase::Captured
                        ) =>
                    {
                        (
                            Some(root),
                            Position::Captured {
                                directory: directory.expect("opened captured parent"),
                                name,
                            },
                        )
                    }
                    (None, None) => {
                        ensure!(
                            capture.captured()
                                && matches!(
                                    state.phase(),
                                    OwnershipRecoveryPhase::Captured
                                        | OwnershipRecoveryPhase::RootRemoved
                                        | OwnershipRecoveryPhase::Complete
                                )
                                && state.entries().iter().all(|entry| entry.removed())
                                && state.pending_create().is_none()
                                && state.pending_remove().is_none(),
                            "missing root lacks durable complete captured removal"
                        );
                        (None, Position::RootMissing { directory, name })
                    }
                    _ => bail!("cleanup root namespace conflicts with durable capture progress"),
                }
            }
        };
        let leases = open_directory(&store, ".leases")?;
        let build_directory = open_directory(&leases, "build")?;
        let witness = Self {
            control,
            authority: Authority::Restart(lease),
            facts,
            nonce: journal.nonce(),
            store,
            root,
            leases,
            build_directory,
            position,
            expected_uid,
        };
        witness.check_stream(journal.nonce(), &witness.facts, &witness.store)?;
        Ok(witness)
    }

    pub(super) fn root_file(&self) -> Option<&File> {
        self.root.as_ref()
    }
    pub(super) fn expected_uid(&self) -> u32 {
        self.expected_uid
    }

    fn root(&self) -> Result<&File> {
        self.root
            .as_ref()
            .ok_or_else(|| eyre::eyre!("cleanup root is already removed"))
    }

    pub(in super::super) fn check_stream(
        &self,
        nonce: uuid::Uuid,
        identity: &GenerationIdentityFacts,
        store: &File,
    ) -> Result<()> {
        ensure!(
            nonce == self.nonce && identity == &self.facts,
            "cleanup stream and actual authority differ"
        );
        let (device, inode) = self.control.store_identity()?;
        ensure!(
            [device, inode] == self.facts.store
                && pair(store)? == self.facts.store
                && pair(&self.store)? == self.facts.store,
            "cleanup store identity changed"
        );
        for directory in [&self.leases, &self.build_directory] {
            validate_owned_directory(directory, self.expected_uid)?;
            ensure!(
                directory.metadata()?.mode() & 0o7777 == 0o700,
                "cleanup lease namespace permissions changed"
            );
        }
        ensure!(
            pair(&self.leases)? == self.facts.leases
                && pair(&self.build_directory)? == self.facts.build,
            "cleanup lease identity changed"
        );
        ensure_entry_identity(store, ".leases", FileIdentity::of(&self.leases)?)?;
        ensure_entry_identity(
            &self.leases,
            "build",
            FileIdentity::of(&self.build_directory)?,
        )?;
        match &self.authority {
            Authority::Building(build) => {
                validate_owned_regular(build, self.expected_uid)?;
                ensure!(
                    pair(build)? == self.facts.lock && build.metadata()?.mode() & 0o7777 == 0o600,
                    "cleanup build lease identity or permissions changed"
                );
                ensure_entry_identity(
                    &self.build_directory,
                    &format!("{}.lock", self.facts.allocation),
                    FileIdentity::of(build)?,
                )?;
            }
            Authority::Restart(lease) => {
                ensure!(lease.facts() == &self.facts, "cleanup lease facts changed");
                lease.check_binding()?;
            }
        }
        if let Some(root) = &self.root {
            validate_owned_directory(root, self.expected_uid)?;
            ensure!(
                pair(root)? == self.facts.generation && root.metadata()?.mode() & 0o7777 == 0o700,
                "cleanup root identity or permissions changed"
            );
        } else {
            ensure!(
                matches!(self.position, Position::RootMissing { .. }),
                "cleanup has no actual root outside proven root removal"
            );
        }
        match &self.position {
            Position::Final => ensure_entry_identity(
                store,
                &self.facts.allocation,
                FileIdentity::of(self.root()?)?,
            )?,
            Position::Captured { directory, name } => {
                validate_quarantine(store, directory, self.expected_uid)?;
                missing(store, &self.facts.allocation)?;
                ensure_entry_identity(directory, name, FileIdentity::of(self.root()?)?)?;
            }
            Position::RootMissing { directory, name } => {
                missing(store, &self.facts.allocation)?;
                if let Some(directory) = directory {
                    validate_quarantine(store, directory, self.expected_uid)?;
                    missing(directory, name)?;
                } else {
                    missing(store, ".quarantine")?;
                }
            }
        }
        Ok(())
    }
}
fn validate_quarantine(store: &File, directory: &File, uid: u32) -> Result<()> {
    validate_owned_directory(directory, uid)?;
    ensure!(
        directory.metadata()?.mode() & 0o7777 == 0o700,
        "cleanup quarantine permissions changed"
    );
    ensure_entry_identity(store, ".quarantine", FileIdentity::of(directory)?)
}

fn event_limits(prefix: &UnpublishedPrefix, observer: &Observer) -> OwnershipEventLimits {
    OwnershipEventLimits {
        prefix: prefix.limits,
        final_prefix_bytes: prefix.final_path.as_os_str().as_encoded_bytes().len(),
        max_event_bytes: observer.limits.max_event_bytes,
        max_events: observer.limits.max_events,
        max_total_path_bytes: observer.limits.max_total_path_bytes,
    }
}

fn fresh_uncommitted(
    control: &StoreControl<'_>,
    journal: &JournalRecord,
    limits: JournalLimits,
) -> Result<()> {
    let records = read_journal_metadata(control, limits)?;
    ensure!(
        records.iter().any(|record| record == journal),
        "cleanup requires the exact fresh durable journal"
    );
    let catalog = read_catalog_metadata(
        control,
        CatalogCodecLimits {
            model: limits.model,
            ..CatalogCodecLimits::default()
        },
    )?;
    ensure!(
        catalog.applied_transaction(journal.nonce()).is_none(),
        "committed transaction cannot be rolled back as unpublished output"
    );
    let generation = journal
        .generation()
        .ok_or_else(|| eyre::eyre!("clear selection has no prefix to clean"))?;
    let allocation = &generation.identity().allocation;
    ensure!(
        catalog.generation(allocation).is_none(),
        "catalog record still protects this environment generation"
    );
    ensure!(
        !records
            .iter()
            .any(|record| record.nonce() != journal.nonce()
                && record
                    .generation()
                    .is_some_and(|other| &other.identity().allocation == allocation)),
        "another pending transaction still protects this generation"
    );
    // The namespace inventory is bounded independently of the owned subtree.
    // A malformed entry anywhere is never treated as safe missing evidence.
    control.scan_environment_namespace(NamespaceScanLimits::default())?;
    Ok(())
}

fn verify_entry(
    parent: &File,
    name: &std::ffi::OsStr,
    tracked: TrackedEntry,
    fact: &OwnershipEntryFacts,
    expected_uid: u32,
) -> Result<()> {
    ensure_tracked_identity(parent, name, tracked)?;
    let actual = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    let kind = match tracked.kind {
        EntryKind::Directory => OwnershipEntryKind::Directory,
        EntryKind::File => OwnershipEntryKind::File,
        EntryKind::Symlink => eyre::bail!("journaled prefixes cannot contain layout symlinks"),
    };
    ensure!(
        kind == fact.kind()
            && [actual.st_dev as u64, actual.st_ino as u64] == fact.identity()
            && pair(parent)? == fact.parent(),
        "durable ownership identity differs from the actual prefix"
    );
    ensure!(
        actual.st_uid == expected_uid && actual.st_mode & 0o7000 == 0,
        "owned cleanup entry has changed owner or privileged mode"
    );
    let mode = u64::from(actual.st_mode & 0o7777);
    match kind {
        OwnershipEntryKind::Directory => {
            ensure!(mode == 0o755, "owned cleanup directory permissions changed")
        }
        OwnershipEntryKind::File => {
            ensure!(
                actual.st_nlink == 1,
                "owned cleanup file has another hard link"
            );
            if let Some(finished) = fact.finished() {
                ensure!(
                    mode == u64::from(finished.mode())
                        && u64::try_from(actual.st_size).ok() == Some(finished.size()),
                    "owned cleanup file differs from durable finished stats"
                );
            }
        }
    }
    Ok(())
}

fn match_inventory(prefix: &UnpublishedPrefix, state: &OwnershipRecoveryState) -> Result<()> {
    ensure!(
        state.phase() == OwnershipRecoveryPhase::Building
            && state.capture().is_none()
            && !state.requires_retention(),
        "uncertain creation or prior cleanup requires fresh recovery"
    );
    ensure!(
        state.entries().len() == prefix.entries.len(),
        "durable ownership inventory differs from the actual prefix"
    );
    for fact in state.entries() {
        ensure!(
            !fact.removed(),
            "building ownership entry was already removed"
        );
        let path = Path::new(std::ffi::OsStr::from_bytes(fact.relative()));
        let tracked = prefix
            .entries
            .get(path)
            .ok_or_else(|| eyre::eyre!("durable ownership path is not tracked by this prefix"))?;
        let parent = prefix.open_owned_parent(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("missing owned entry name"))?;
        verify_entry(&parent, name, *tracked, fact, prefix.expected_uid)?;
    }
    prefix.validate_tracked_tree()?;
    Ok(())
}

fn emit<'a>(
    observer: &mut Observer,
    witness: &CleanupWitness<'_, '_, '_>,
    event: impl FnOnce(u64) -> OwnershipEventRef<'a>,
) -> Result<u64> {
    let sequence = observer.stream.next_sequence()?;
    let bytes = observer.encoder.encode(event(sequence))?;
    let receipt = observer.stream.append_cleanup(witness, &bytes)?;
    ensure!(
        receipt.sequence() == sequence,
        "cleanup frame sequence changed"
    );
    Ok(sequence)
}

fn capture(
    observer: &mut Observer,
    witness: &mut CleanupWitness<'_, '_, '_>,
    state: &OwnershipRecoveryState,
    store_path: &Path,
    hook: &mut impl FnMut(CleanupCheckpoint<'_>) -> Result<()>,
    recovery: &mut Option<Recovery>,
) -> Result<u64> {
    let (operation, name) = match state.capture() {
        Some(capture) => (
            capture.operation(),
            std::str::from_utf8(capture.quarantine())
                .map_err(|_| eyre::eyre!("invalid quarantine identifier"))?
                .to_owned(),
        ),
        None => {
            let name = format!("q-{}", uuid::Uuid::now_v7().simple());
            let root = witness.facts.generation;
            let operation = emit(observer, witness, |op| OwnershipEventRef::CaptureIntent {
                op,
                quarantine: name.as_bytes(),
                root,
            })?;
            (operation, name)
        }
    };
    if matches!(witness.position, Position::Final) {
        let quarantine =
            open_private_directory(&witness.store, ".quarantine", witness.expected_uid)?;
        witness.store.sync_all()?; // Make the new parent durable before rename.
        witness.check_stream(witness.nonce, &witness.facts, &witness.store)?;
        hook(CleanupCheckpoint::BeforeCapture)?;
        rename_noclobber(
            &witness.store,
            std::ffi::OsStr::new(&witness.facts.allocation),
            &quarantine,
            std::ffi::OsStr::new(&name),
        )?;
        *recovery = Some(Recovery {
            directory: quarantine.try_clone()?,
            name: name.clone(),
            path: store_path.join(".quarantine").join(&name),
        });
        witness.position = Position::Captured {
            directory: quarantine,
            name: name.clone(),
        };
        witness.store.sync_all()?;
        let Position::Captured { directory, .. } = &witness.position else {
            unreachable!("captured root")
        };
        directory.sync_all()?;
        hook(CleanupCheckpoint::Captured)?;
        let actual = open_directory(directory, name.as_str())?;
        let identity = FileIdentity::of(&actual)?;
        if identity != FileIdentity::of(witness.root()?)? {
            ensure_entry_identity(directory, &name, identity)?;
            match rename_noclobber(
                directory,
                std::ffi::OsStr::new(&name),
                &witness.store,
                std::ffi::OsStr::new(&witness.facts.allocation),
            ) {
                Ok(()) => {
                    *recovery = None;
                    directory.sync_all()?;
                    witness.store.sync_all()?;
                    bail!("cleanup captured a changed identity; captured entry restored");
                }
                Err(_) => bail!("cleanup captured a changed identity; captured entry retained"),
            }
        }
    }
    witness.check_stream(witness.nonce, &witness.facts, &witness.store)?;
    if !state.capture().is_some_and(|capture| capture.captured()) {
        emit(observer, witness, |_| OwnershipEventRef::Captured {
            op: operation,
        })?;
        hook(CleanupCheckpoint::CapturedDurable)?;
    }
    Ok(operation)
}

fn unlink_entry(
    observer: &mut Observer,
    witness: &CleanupWitness<'_, '_, '_>,
    tree: &mut tree::Tree<'_>,
    relative: &Path,
    pending: Option<u64>,
    hook: &mut impl FnMut(CleanupCheckpoint<'_>) -> Result<()>,
) -> Result<()> {
    let fact = *tree
        .facts
        .get(relative)
        .ok_or_else(|| eyre::eyre!("missing durable cleanup entry facts"))?;
    let parent = tree.open_parent(witness.root()?, relative)?;
    let name = relative
        .file_name()
        .ok_or_else(|| eyre::eyre!("missing cleanup entry name"))?;
    let tracked = tree.entries.get(relative).copied();
    if let Some(tracked) = tracked {
        verify_entry(&parent, name, tracked, fact, witness.expected_uid)?;
    } else {
        ensure!(
            pending.is_some(),
            "owned cleanup entry is missing without remove intent"
        );
        missing_leaf(&parent, name)?;
    }
    let operation = match pending {
        Some(operation) => operation,
        None => emit(observer, witness, |op| OwnershipEventRef::RemoveIntent {
            op,
            kind: fact.kind(),
            relative: relative.as_os_str().as_encoded_bytes(),
            identity: fact.identity(),
        })?,
    };
    if let Some(tracked) = tracked {
        hook(CleanupCheckpoint::BeforeUnlink(relative))?;
        witness.check_stream(witness.nonce, &witness.facts, &witness.store)?;
        verify_entry(&parent, name, tracked, fact, witness.expected_uid)?;
        let flags = match tracked.kind {
            EntryKind::Directory => nix::unistd::UnlinkatFlags::RemoveDir,
            EntryKind::File => nix::unistd::UnlinkatFlags::NoRemoveDir,
            EntryKind::Symlink => eyre::bail!("journaled prefixes cannot contain layout symlinks"),
        };
        nix::unistd::unlinkat(&parent, name, flags)?;
        parent.sync_all()?;
        hook(CleanupCheckpoint::Unlinked(relative))?;
    }
    emit(observer, witness, |_| OwnershipEventRef::Removed {
        op: operation,
    })?;
    tree.entries.remove(relative);
    Ok(())
}

fn missing_leaf(parent: &File, name: &std::ffi::OsStr) -> Result<()> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) => Ok(()),
        Ok(_) => bail!("already removed cleanup entry was recreated"),
        Err(error) => Err(error.into()),
    }
}

pub(super) struct RunTarget<'a> {
    pub(super) store_path: &'a Path,
    pub(super) max_cleanup_entries: usize,
}

pub(super) fn run(
    observer: &mut Observer,
    witness: &mut CleanupWitness<'_, '_, '_>,
    state: &OwnershipRecoveryState,
    tree: &mut tree::Tree<'_>,
    target: RunTarget<'_>,
    hook: &mut impl FnMut(CleanupCheckpoint<'_>) -> Result<()>,
    recovery: &mut Option<Recovery>,
) -> Result<()> {
    witness.check_stream(witness.nonce, &witness.facts, &witness.store)?;
    if state.phase() == OwnershipRecoveryPhase::Complete {
        return Ok(());
    }
    if state.phase() == OwnershipRecoveryPhase::RootRemoved {
        let sequence = state
            .root_removed_sequence()
            .ok_or_else(|| eyre::eyre!("missing root removal sequence"))?;
        emit(observer, witness, |_| OwnershipEventRef::Complete {
            root_removed_sequence: sequence,
        })?;
        hook(CleanupCheckpoint::Complete)?;
        return Ok(());
    }
    let captured = capture(observer, witness, state, target.store_path, hook, recovery)?;
    if let Some(pending) = state.pending_remove() {
        let relative = Path::new(std::ffi::OsStr::from_bytes(pending.relative()));
        unlink_entry(
            observer,
            witness,
            tree,
            relative,
            Some(pending.operation()),
            hook,
        )?;
    }
    for relative in tree.removal_order() {
        unlink_entry(observer, witness, tree, &relative, None, hook)?;
    }
    if !matches!(witness.position, Position::RootMissing { .. }) {
        ensure!(
            directory_is_empty(witness.root()?, target.max_cleanup_entries)?,
            "untracked environment entry appeared during cleanup"
        );
        witness.check_stream(witness.nonce, &witness.facts, &witness.store)?;
        let Position::Captured { directory, name } =
            std::mem::replace(&mut witness.position, Position::Final)
        else {
            unreachable!("actual cleanup root was captured")
        };
        match nix::unistd::unlinkat(
            &directory,
            name.as_str(),
            nix::unistd::UnlinkatFlags::RemoveDir,
        ) {
            Ok(()) => {
                witness.position = Position::RootMissing {
                    directory: Some(directory),
                    name,
                }
            }
            Err(error) => {
                witness.position = Position::Captured { directory, name };
                return Err(error.into());
            }
        }
        if let Position::RootMissing {
            directory: Some(directory),
            ..
        } = &witness.position
        {
            directory.sync_all()?;
        }
        hook(CleanupCheckpoint::RootUnlinked)?;
    }
    let removed = emit(observer, witness, |_| OwnershipEventRef::RootRemoved {
        capture_op: captured,
    })?;
    emit(observer, witness, |_| OwnershipEventRef::Complete {
        root_removed_sequence: removed,
    })?;
    hook(CleanupCheckpoint::Complete)?;
    Ok(())
}

impl UnpublishedPrefix {
    pub(in super::super) fn discard_observed(&mut self) -> Result<()> {
        self.discard_observed_with_hook(|_| Ok(()))
    }
    pub(super) fn discard_observed_with_hook(
        &mut self,
        mut hook: impl FnMut(CleanupCheckpoint<'_>) -> Result<()>,
    ) -> Result<()> {
        let nonce = match &self.ownership {
            OwnershipRegistration::Attached(observer) => observer.journal.nonce(),
            _ => bail!("prefix has no available ownership observer"),
        };
        let OwnershipRegistration::Attached(mut observer) = std::mem::replace(
            &mut self.ownership,
            OwnershipRegistration::Retained { nonce },
        ) else {
            unreachable!("checked attached observer")
        };
        let mut recovery = None;
        let result = (|| {
            let store = EnvironmentStore {
                root_path: self
                    .final_path
                    .parent()
                    .ok_or_else(|| eyre::eyre!("missing environment store"))?
                    .to_path_buf(),
                expected_uid: self.expected_uid,
                limits: self.limits,
                root: self.binding.store_root.try_clone()?,
                kernel: self.binding.kernel.clone(),
            };
            store.with_control(observer.control_limits, |control| {
                fresh_uncommitted(control, &observer.journal, observer.journal_limits)?;
                let log = control
                    .read_ownership_stream(
                        &observer.journal,
                        observer.journal_limits,
                        observer.limits.stream,
                    )?
                    .ok_or_else(|| eyre::eyre!("durable ownership stream disappeared"))?;
                ensure!(
                    observer.stream.next_sequence()? == log.records().len() as u64 + 1,
                    "ownership stream writer position differs from durable history"
                );
                let state = validate_ownership_events(&log, event_limits(self, &observer))?;
                match_inventory(self, &state)?;
                let mut witness = CleanupWitness::original(control, self, &observer.journal)?;
                let mut tree = tree::Tree::load(
                    witness.root.as_ref(),
                    &state,
                    self.limits,
                    self.final_path.as_os_str().as_encoded_bytes().len(),
                    self.expected_uid,
                )?;
                run(
                    &mut observer,
                    &mut witness,
                    &state,
                    &mut tree,
                    RunTarget {
                        store_path: &store.root_path,
                        max_cleanup_entries: self.limits.max_cleanup_entries,
                    },
                    &mut hook,
                    &mut recovery,
                )
            })
        })();
        if result.is_ok() {
            self.entries.clear();
            self.discarded = true;
            self.recovery = None;
        } else {
            self.tainted = true;
            self.recovery = recovery;
        }
        result
    }
}
