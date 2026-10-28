//! Narrow, explicitly requested recovery of the four PBS bytecode files.

use super::manifest::{DiskIdentity, LanguageManifest, LockRegistration};
use super::repository::{PublishedLanguage, manifest_name, registration_name, uid};
use crate::artifact_snapshot::{VerifiedArtifact, cached_verified_artifact};
use crate::packages::python_environment::prefix::verify::{
    InventoryRoot, PrefixInventoryLimits, TreeInventory, TreeInventoryEntry, TreeInventoryKind,
    observe_tree_inventory,
};
use crate::programs::OperationContext;
use eyre::{ContextCompat, Result, ensure};
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::{Mode, fchmod};
use nix::unistd::{UnlinkatFlags, unlinkat};
use pakro_store::control::lease::{LeaseMode, ObjectLease, StableLock};
use pakro_store::control::{
    ControlLimits, ControlNamespace, ControlSnapshot, FileIdentity, ReadStore, RootLimits,
    StoreWriter, WriterControl, ensure_entry_identity, open_directory,
    open_existing_readonly_regular, rename_noclobber,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const MAX_BYTECODE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 16 * 1024 * 1024 * 1024;

pub(crate) const BYTECODE_PATHS: [&str; 4] = [
    "lib/python3.11/encodings/__pycache__/__init__.cpython-311.pyc",
    "lib/python3.11/encodings/__pycache__/aliases.cpython-311.pyc",
    "lib/python3.11/encodings/__pycache__/utf_8.cpython-311.pyc",
    "lib/python3.11/site-packages/_distutils_hack/__pycache__/__init__.cpython-311.pyc",
];

#[derive(Debug)]
pub(crate) struct BytecodeRecoveryReport {
    pub instance: String,
    pub paths: Vec<PathBuf>,
    pub unchanged_entries: usize,
    pub restored: bool,
    pub already_healthy: bool,
    pub resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryStep {
    AfterStage(usize),
    BeforePreparedManifest,
    AfterPreparedManifest,
    AfterJournal,
    AfterChmod(usize),
    AfterCapture(usize),
    AfterInstall(usize),
    BeforeManifestCas,
    AfterManifestCas,
    AfterBackupCleanup(usize),
    BeforeJournalRemove,
}

pub(crate) fn restore_bytecode(
    context: &OperationContext,
    target: &PublishedLanguage,
    dry_run: bool,
) -> Result<BytecodeRecoveryReport> {
    ensure!(target.source_kind() == super::source::PythonRuntimeSource::Prebuilt,
        "the four-file bytecode recovery applies only to the original PBS runtime recipe");
    restore_bytecode_inner(context, target, dry_run, &mut |_| Ok(()))
}

/// Conservative, read-only roots for unfinished narrow bytecode recovery.
/// Reading a journal grants neither recovery nor deletion authority.
pub(crate) struct PendingBytecodeProtection {
    pub language: [u8; 32],
    pub journals: Vec<String>,
}
pub(crate) fn pending_protections(
    context: &OperationContext,
) -> Result<Vec<PendingBytecodeProtection>> {
    let Some(read) = ReadStore::open_readonly(
        &context.requested_data_root().join("languages"),
        uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?
    else {
        return Ok(Vec::new());
    };
    let control = read.control(ControlLimits::default())?;
    let mut roots = Vec::new();
    let mut total = 0usize;
    for name in control.list_names(ControlNamespace::Journals, 65536, 8 * 1024 * 1024)? {
        if !name.starts_with("bytecode-recovery-") {
            continue;
        }
        context.checkpoint()?;
        let snapshot = control
            .read(ControlNamespace::Journals, &name)?
            .context("PendingRecovery: bytecode journal disappeared")?;
        total = total
            .checked_add(snapshot.bytes().len())
            .filter(|value| *value <= 64 * 1024 * 1024)
            .context("bytecode recovery projection byte budget exceeded")?;
        let journal: RecoveryJournal = serde_json::from_slice(snapshot.bytes())
            .map_err(|error| eyre::eyre!("PendingRecovery: damaged bytecode journal: {error}"))?;
        ensure!(
            journal.schema == 1
                && <[u8; 32]>::from(Sha256::digest(journal.manifest.as_bytes()))
                    == journal.manifest_sha256,
            "PendingRecovery: bytecode journal schema or original digest differs"
        );
        journal.manifest_identity.validate()?;
        let original: LanguageManifest = serde_json::from_str(&journal.manifest)?;
        let spec = super::repository::LanguageRepository::spec_from_manifest(&original)?;
        super::publication::validate_manifest(&spec, &original)?;
        super::publication::validate_manifest_location(
            &original,
            &read.root_path().join(spec.key().filename()).join("payload"),
        )?;
        ensure!(
            journal.key == original.key
                && name == format!("bytecode-recovery-{}.json", spec.key().filename()),
            "PendingRecovery: bytecode journal object identity differs"
        );
        journal.prepared.identity.validate()?;
        let expected = original_entries(&original)?;
        ensure!(
            journal.files.len() == expected.len(),
            "PendingRecovery: bytecode journal file set differs"
        );
        let mut names = std::collections::BTreeSet::from([name]);
        ensure!(
            journal.prepared.name.starts_with("bytecode-manifest-")
                && journal.prepared.name.len() <= 128
                && journal
                    .prepared
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && names.insert(journal.prepared.name.clone()),
            "PendingRecovery: invalid prepared bytecode manifest name"
        );
        for (file, expected) in journal.files.iter().zip(expected) {
            file.stage_identity.validate()?;
            ensure!(
                file.original == expected
                    && file.damaged.relative == expected.relative
                    && file.damaged.uid == expected.uid
                    && file.damaged.mode == expected.mode
                    && matches!(file.damaged.kind,TreeInventoryKind::File{size,..} if size<=MAX_BYTECODE_BYTES),
                "PendingRecovery: unknown bytecode recovery file facts"
            );
            for name in [&file.stage, &file.backup] {
                ensure!(
                    name.len() <= 128
                        && name.starts_with("bytecode-")
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        && names.insert(name.clone()),
                    "PendingRecovery: unsafe bytecode recovery staging name"
                );
            }
        }
        ensure!(
            <[u8; 32]>::from(Sha256::digest(serde_json::to_vec(&final_manifest(
                &original,
                &journal.files
            )?)?))
                == journal.prepared.sha256,
            "PendingRecovery: prepared bytecode manifest digest differs"
        );
        roots.push(PendingBytecodeProtection {
            language: journal.key,
            journals: names.into_iter().collect(),
        });
    }
    roots.sort_unstable_by_key(|root| root.language);
    Ok(roots)
}

/// A restart record never grants authority itself. Every field is checked
/// against the requested instance, its stable EX lock and the verified archive.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryJournal {
    schema: u32,
    key: [u8; 32],
    manifest: String,
    manifest_sha256: [u8; 32],
    manifest_identity: DiskIdentity,
    prepared: PreparedManifest,
    files: Vec<RecoveryFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedManifest {
    name: String,
    identity: DiskIdentity,
    sha256: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryFile {
    original: TreeInventoryEntry,
    damaged: TreeInventoryEntry,
    stage: String,
    stage_identity: DiskIdentity,
    backup: String,
}

struct RecoveryPermit<'a> {
    context: &'a OperationContext,
    target: &'a PublishedLanguage,
    writer: StoreWriter,
    lock: StableLock,
    held: ObjectLease,
    allocation: File,
    root: File,
    manifest: LanguageManifest,
    snapshot: ControlSnapshot,
    manifest_identity: DiskIdentity,
    _manifest_pin: File,
}

impl RecoveryPermit<'_> {
    fn check(&self) -> Result<()> {
        self.context.checkpoint()?;
        self.writer.check_binding()?;
        self.held.check_binding()?;
        ensure!(
            self.held.mode() == LeaseMode::Exclusive,
            "bytecode recovery lost EX protection"
        );
        self.lock.check_location(
            self.writer.build_handle(),
            &format!("{}.lock", self.target.key().filename()),
        )?;
        ensure_entry_identity(
            self.writer.root_handle(),
            &self.target.key().filename(),
            self.manifest.allocation.file_identity(),
        )?;
        ensure_entry_identity(
            &self.allocation,
            "payload",
            self.manifest.payload.file_identity(),
        )?;
        ensure!(
            FileIdentity::of(&self.allocation)? == self.manifest.allocation.file_identity()
                && FileIdentity::of(&self.root)? == self.manifest.payload.file_identity(),
            "bytecode recovery allocation identity changed"
        );
        for file in [&self.allocation, &self.root] {
            let metadata = file.metadata()?;
            ensure!(
                metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o7777 == 0o700,
                "bytecode recovery allocation type, owner or mode changed"
            );
        }
        Ok(())
    }

    fn inventory(&self) -> Result<TreeInventory> {
        observe_tree_inventory(self, PrefixInventoryLimits::default())
    }

    fn parent(&self, baseline: &TreeInventory, path: &Path) -> Result<File> {
        self.check()?;
        let mut parent = self.root.try_clone()?;
        let mut relative = PathBuf::new();
        for component in path
            .parent()
            .context("bytecode has no parent")?
            .components()
        {
            let Component::Normal(name) = component else {
                eyre::bail!("unsafe bytecode parent")
            };
            relative.push(name);
            let expected = baseline
                .entries()
                .iter()
                .find(|entry| entry.relative == relative)
                .context("bytecode parent is not in the published inventory")?;
            ensure!(
                matches!(expected.kind, TreeInventoryKind::Directory),
                "bytecode parent is not a directory"
            );
            let next = open_directory(&parent, name)?;
            let metadata = next.metadata()?;
            ensure!(
                [metadata.dev(), metadata.ino()] == expected.identity
                    && metadata.uid() == expected.uid
                    && metadata.mode() & 0o7777 == expected.mode,
                "bytecode recovery parent identity, owner or mode changed"
            );
            ensure_entry_identity(
                &parent,
                name.to_str().context("bytecode parent is not UTF-8")?,
                FileIdentity::of(&next)?,
            )?;
            parent = next;
        }
        Ok(parent)
    }

    fn recheck_parent(&self, baseline: &TreeInventory, path: &Path, held: &File) -> Result<()> {
        ensure!(
            FileIdentity::of(&self.parent(baseline, path)?)? == FileIdentity::of(held)?,
            "bytecode recovery parent binding changed"
        );
        self.check()
    }
}

impl InventoryRoot for RecoveryPermit<'_> {
    fn root_file(&self) -> &File {
        &self.root
    }
    fn root_path(&self) -> &Path {
        self.target.payload_root()
    }
    fn allocation_id(&self) -> &str {
        self.target
            .payload_root()
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("validated language allocation")
    }
    fn expected_uid(&self) -> u32 {
        uid()
    }
    fn check_binding(&self) -> Result<()> {
        self.check()
    }
}

fn acquire<'a>(
    context: &'a OperationContext,
    target: &'a PublishedLanguage,
    dry_run: bool,
) -> Result<RecoveryPermit<'a>> {
    context.checkpoint()?;
    ensure!(
        dry_run || context.policy().writes.libraries,
        "operation policy forbids bytecode recovery"
    );
    let path = context.requested_data_root().join("languages");
    // A dry run or malformed target must never initialize an absent library.
    ensure!(
        ReadStore::open_readonly(
            &path,
            uid(),
            RootLimits::default(),
            context.policy().deadline
        )?
        .is_some(),
        "published language library is missing"
    );
    let writer = StoreWriter::open_writer(
        &path,
        uid(),
        RootLimits::default(),
        context.policy().deadline,
    )?;
    let registration = writer
        .control(ControlLimits::default())?
        .read(
            ControlNamespace::Ownership,
            &registration_name(target.spec()),
        )?
        .context("language protection registration is missing")?;
    let registered: LockRegistration = serde_json::from_slice(registration.bytes())?;
    ensure!(
        registered.schema == 1 && registered.key == target.key().fingerprint(),
        "language protection registration differs from recovery target"
    );
    registered.lock.validate()?;
    let lock = StableLock::open_existing(
        writer.build_handle(),
        &format!("{}.lock", target.key().filename()),
        uid(),
        registered.lock.file_identity(),
    )?;
    let control = writer.control(ControlLimits::default())?;
    let held = control
        .try_object(&target.key().filename(), &lock, LeaseMode::Exclusive)?
        .context("language is in use by an active Pakro operation")?;
    control.ensure_catalog()?;
    let pin = open_existing_readonly_regular(
        control.namespace_handle(ControlNamespace::Catalog),
        &manifest_name(target.spec()),
        uid(),
    )?;
    let manifest_identity = DiskIdentity::from_file_identity(FileIdentity::of(&pin)?);
    let snapshot = control
        .read(ControlNamespace::Catalog, &manifest_name(target.spec()))?
        .context("language Ready manifest disappeared")?;
    ensure!(
        snapshot.file_identity() == manifest_identity.file_identity(),
        "language Ready manifest inode changed while pinned"
    );
    ensure_entry_identity(
        control.namespace_handle(ControlNamespace::Catalog),
        &manifest_name(target.spec()),
        manifest_identity.file_identity(),
    )?;
    let mut pinned = Vec::new();
    (&pin)
        .take(ControlLimits::default().max_file_bytes + 1)
        .read_to_end(&mut pinned)?;
    ensure!(
        pinned == snapshot.bytes(),
        "language Ready manifest changed while pinned"
    );
    let manifest: LanguageManifest = serde_json::from_slice(snapshot.bytes())?;
    super::publication::validate_manifest(target.spec(), &manifest)?;
    super::publication::validate_manifest_location(&manifest, target.payload_root())?;
    ensure!(
        manifest.lock == registered.lock,
        "language Ready lock registration differs"
    );
    let current_registration = control
        .read(
            ControlNamespace::Ownership,
            &registration_name(target.spec()),
        )?
        .context("language protection registration disappeared")?;
    ensure!(
        current_registration.bytes() == registration.bytes(),
        "language protection registration changed"
    );
    ensure_entry_identity(
        writer.root_handle(),
        &target.key().filename(),
        manifest.allocation.file_identity(),
    )?;
    let allocation = open_directory(writer.root_handle(), target.key().filename().as_str())?;
    ensure_entry_identity(&allocation, "payload", manifest.payload.file_identity())?;
    let root = open_directory(&allocation, "payload")?;
    drop(control);
    let permit = RecoveryPermit {
        context,
        target,
        writer,
        lock,
        held,
        allocation,
        root,
        manifest,
        snapshot,
        manifest_identity,
        _manifest_pin: pin,
    };
    permit.check()?;
    Ok(permit)
}

fn paths() -> Vec<PathBuf> {
    BYTECODE_PATHS.iter().map(PathBuf::from).collect()
}
fn journal_name(target: &PublishedLanguage) -> String {
    format!("bytecode-recovery-{}.json", target.key().filename())
}

fn original_entries(manifest: &LanguageManifest) -> Result<Vec<TreeInventoryEntry>> {
    ensure!(
        manifest.descriptor.language_version.full_version() == "3.11.17",
        "bytecode recovery supports only Python 3.11.17"
    );
    BYTECODE_PATHS
        .iter()
        .map(|path| {
            let entry = manifest
                .inventory
                .entries()
                .iter()
                .find(|entry| entry.relative == Path::new(path))
                .context("published inventory is missing an expected bytecode member")?;
            let TreeInventoryKind::File { size, .. } = entry.kind else {
                eyre::bail!("published bytecode member is not a regular file")
            };
            ensure!(
                size <= MAX_BYTECODE_BYTES && entry.uid == uid() && entry.mode & !0o777 == 0,
                "published bytecode facts exceed recovery limits"
            );
            Ok(entry.clone())
        })
        .collect()
}

struct ControlledRead<'a, R> {
    permit: &'a RecoveryPermit<'a>,
    input: R,
    remaining: u64,
}
impl<R: Read> Read for ControlledRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.permit
            .check()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let amount = buffer
            .len()
            .min(self.remaining.saturating_add(1).min(usize::MAX as u64) as usize);
        let count = self.input.read(&mut buffer[..amount])?;
        self.remaining = self
            .remaining
            .checked_sub(count as u64)
            .ok_or_else(|| io::Error::other("recovery archive exceeds decoded byte budget"))?;
        Ok(count)
    }
}

fn archive_bytes(
    permit: &RecoveryPermit<'_>,
    original: &[TreeInventoryEntry],
) -> Result<Vec<Vec<u8>>> {
    let source = permit.target.spec().plan().source();
    let cache = permit
        .context
        .requested_cache_root()
        .join("python/native-v1");
    let mut artifact: VerifiedArtifact = cached_verified_artifact(
        source.filename(),
        source.digest(),
        &cache,
        MAX_ARCHIVE_BYTES,
    )?
    .context("verified local Python archive is missing; recovery does not download")?;
    ensure!(
        artifact.filename() == source.filename() && artifact.digest() == source.digest(),
        "recovery archive identity differs"
    );
    ensure!(
        source.filename().ends_with(".tar.gz"),
        "bytecode recovery requires the original gzip archive"
    );
    artifact.rewind()?;
    let mut input = ControlledRead {
        permit,
        input: flate2::read::GzDecoder::new(&mut artifact),
        remaining: MAX_DECODED_BYTES,
    };
    let mut found: Vec<Option<Vec<u8>>> = vec![None; 4];
    {
        let mut archive = jdx_tar::Archive::new(&mut input);
        for (count, entry) in archive.entries()?.enumerate() {
            permit.check()?;
            ensure!(count < 1_048_576, "recovery archive entry budget exceeded");
            let mut entry = entry?;
            let member = entry.path()?;
            let raw = member.as_os_str().as_encoded_bytes();
            ensure!(
                !member.is_absolute()
                    && !raw.contains(&0)
                    && !raw.contains(&b'\\')
                    && !raw
                        .split(|byte| *byte == b'/')
                        .any(|part| matches!(part, b"." | b"..")),
                "unsafe recovery archive member"
            );
            let mut components = member.components();
            ensure!(
                matches!(components.next(), Some(Component::Normal(_))),
                "recovery archive has no bundle root"
            );
            let relative: PathBuf = components.collect();
            let Some(index) = original.iter().position(|fact| fact.relative == relative) else {
                continue;
            };
            ensure!(
                found[index].is_none() && entry.entry_type() == jdx_tar::EntryType::File,
                "duplicate or non-file recovery archive member"
            );
            let TreeInventoryKind::File { size, sha256 } = original[index].kind else {
                unreachable!()
            };
            ensure!(
                entry.size() == size
                    && size <= MAX_BYTECODE_BYTES
                    && entry.header().mode() & !0o777 == 0
                    && entry.header().mode() & !0o022 == original[index].mode,
                "original archive bytecode metadata differs from published inventory"
            );
            let mut bytes = Vec::new();
            (&mut entry)
                .take(MAX_BYTECODE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 == size && <[u8; 32]>::from(Sha256::digest(&bytes)) == sha256,
                "original archive bytecode digest differs from published inventory"
            );
            found[index] = Some(bytes);
        }
    }
    io::copy(&mut input, &mut io::sink())?;
    permit.check()?;
    found
        .into_iter()
        .map(|entry| entry.context("original archive bytecode member is missing"))
        .collect()
}

fn metadata_stamp(value: &Metadata) -> [u64; 11] {
    [
        value.dev(),
        value.ino(),
        value.uid() as u64,
        value.mode() as u64,
        value.nlink(),
        value.len(),
        value.mtime() as u64,
        value.mtime_nsec() as u64,
        value.ctime() as u64,
        value.ctime_nsec() as u64,
        value.is_file() as u64,
    ]
}

fn leaf(
    permit: &RecoveryPermit<'_>,
    parent: &File,
    name: &str,
    relative: &Path,
) -> Result<Option<(File, TreeInventoryEntry)>> {
    permit.check()?;
    let file = match openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.uid() == uid()
            && before.nlink() == 1
            && before.len() <= MAX_BYTECODE_BYTES,
        "bytecode recovery file has unknown type, owner, links or size"
    );
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 64 * 1024];
    let mut reader = &file;
    loop {
        permit.check()?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|size| *size <= before.len())
            .context("bytecode recovery file changed while reading")?;
        digest.update(&buffer[..read]);
    }
    ensure!(
        count == before.len() && metadata_stamp(&before) == metadata_stamp(&file.metadata()?),
        "bytecode recovery file changed while reading"
    );
    ensure_entry_identity(parent, name, FileIdentity::of(&file)?)?;
    Ok(Some((
        file,
        TreeInventoryEntry {
            relative: relative.to_owned(),
            identity: [before.dev(), before.ino()],
            uid: before.uid(),
            mode: before.mode() & 0o7777,
            kind: TreeInventoryKind::File {
                size: count,
                sha256: digest.finalize().into(),
            },
        },
    )))
}

#[derive(Debug, PartialEq, Eq)]
enum FileState {
    Pending,
    Captured,
    Installed,
}

fn restored_fact(file: &RecoveryFile) -> TreeInventoryEntry {
    let mut expected = file.original.clone();
    expected.identity = [file.stage_identity.device, file.stage_identity.inode];
    expected
}

fn final_manifest(original: &LanguageManifest, files: &[RecoveryFile]) -> Result<LanguageManifest> {
    let replacements: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.original.relative.clone(),
                [file.stage_identity.device, file.stage_identity.inode],
            )
        })
        .collect();
    let mut final_manifest = original.clone();
    final_manifest.inventory = original.inventory.with_file_identities(&replacements)?;
    Ok(final_manifest)
}

fn state(
    permit: &RecoveryPermit<'_>,
    baseline: &TreeInventory,
    file: &RecoveryFile,
    committed: bool,
) -> Result<FileState> {
    let parent = permit.parent(baseline, &file.original.relative)?;
    let name = file
        .original
        .relative
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid bytecode name")?;
    let actual = leaf(permit, &parent, name, &file.original.relative)?;
    let journal = permit.writer.namespace_handle(ControlNamespace::Journals);
    let stage = leaf(permit, journal, &file.stage, &file.original.relative)?;
    let backup = leaf(permit, journal, &file.backup, &file.original.relative)?;
    if let Some((_, fact)) = &stage {
        let mut expected = restored_fact(file);
        ensure!(
            fact.mode == 0o600 || fact.mode == expected.mode,
            "bytecode stage permissions changed"
        );
        expected.mode = fact.mode;
        ensure!(
            *fact == expected,
            "bytecode stage has unknown content or identity"
        );
    }
    if let Some((_, fact)) = &backup {
        ensure!(
            *fact == file.damaged,
            "captured bytecode has unknown content or identity"
        );
    }
    permit.recheck_parent(baseline, &file.original.relative, &parent)?;
    match (actual, stage, backup) {
        (Some((_, fact)), Some(_), None) if fact == file.damaged && !committed => {
            Ok(FileState::Pending)
        }
        (None, Some(_), Some(_)) if !committed => Ok(FileState::Captured),
        (Some((_, fact)), None, backup)
            if fact == restored_fact(file) && (backup.is_some() || committed) =>
        {
            Ok(FileState::Installed)
        }
        _ => eyre::bail!("bytecode recovery encountered an unknown or changed file state"),
    }
}

fn validate_journal(
    permit: &RecoveryPermit<'_>,
    journal: &RecoveryJournal,
) -> Result<LanguageManifest> {
    ensure!(
        journal.schema == 1
            && journal.key == permit.target.key().fingerprint()
            && <[u8; 32]>::from(Sha256::digest(journal.manifest.as_bytes()))
                == journal.manifest_sha256,
        "bytecode recovery journal identity changed"
    );
    journal.manifest_identity.validate()?;
    let original: LanguageManifest = serde_json::from_str(&journal.manifest)?;
    super::publication::validate_manifest(permit.target.spec(), &original)?;
    super::publication::validate_manifest_location(&original, permit.target.payload_root())?;
    ensure!(
        original.lock == permit.manifest.lock
            && original.allocation == permit.manifest.allocation
            && original.payload == permit.manifest.payload,
        "bytecode recovery journal belongs to another allocation"
    );
    let expected = original_entries(&original)?;
    ensure!(
        journal.files.len() == 4,
        "bytecode recovery journal file set changed"
    );
    let mut names = std::collections::BTreeSet::new();
    names.insert(journal_name(permit.target));
    journal.prepared.identity.validate()?;
    ensure!(
        journal.prepared.name.starts_with("bytecode-manifest-")
            && journal.prepared.name.len() <= 128
            && journal
                .prepared
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && names.insert(journal.prepared.name.clone()),
        "unsafe or duplicate prepared manifest name"
    );
    for (entry, original) in journal.files.iter().zip(expected) {
        entry.stage_identity.validate()?;
        ensure!(
            entry.original == original
                && entry.damaged.relative == original.relative
                && entry.damaged.uid == original.uid
                && entry.damaged.mode == original.mode
                && matches!(entry.damaged.kind, TreeInventoryKind::File { size, .. } if size <= MAX_BYTECODE_BYTES),
            "bytecode recovery journal contains unknown file facts"
        );
        for name in [&entry.stage, &entry.backup] {
            ensure!(
                name.len() <= 128
                    && name.starts_with("bytecode-")
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && names.insert(name.clone()),
                "unsafe or duplicate bytecode recovery staging name"
            );
        }
    }
    ensure!(
        <[u8; 32]>::from(Sha256::digest(serde_json::to_vec(&final_manifest(
            &original,
            &journal.files
        )?)?))
            == journal.prepared.sha256,
        "prepared manifest digest differs from recorded bytecode facts"
    );
    Ok(original)
}

fn prepared_manifest_snapshot(
    permit: &RecoveryPermit<'_>,
    control: &WriterControl<'_>,
    original: &LanguageManifest,
    journal: &RecoveryJournal,
) -> Result<ControlSnapshot> {
    permit.check()?;
    let prepared = control
        .read(ControlNamespace::Journals, &journal.prepared.name)?
        .context("prepared language Ready manifest disappeared")?;
    ensure!(
        prepared.file_identity() == journal.prepared.identity.file_identity(),
        "prepared language Ready manifest identity changed"
    );
    ensure!(
        prepared.sha256() == &journal.prepared.sha256
            && prepared.bytes() == serde_json::to_vec(&final_manifest(original, &journal.files)?)?,
        "prepared language Ready manifest bytes changed"
    );
    check_snapshot(
        permit,
        control,
        ControlNamespace::Journals,
        &journal.prepared.name,
        &prepared,
    )?;
    Ok(prepared)
}

fn check_published_prepared(
    permit: &RecoveryPermit<'_>,
    control: &WriterControl<'_>,
    original: &LanguageManifest,
    journal: &RecoveryJournal,
    snapshot: &ControlSnapshot,
) -> Result<()> {
    ensure!(
        snapshot.file_identity() == journal.prepared.identity.file_identity(),
        "published language Ready manifest identity differs from prepared inode"
    );
    ensure!(
        snapshot.sha256() == &journal.prepared.sha256
            && snapshot.bytes() == serde_json::to_vec(&final_manifest(original, &journal.files)?)?,
        "published language Ready manifest differs from prepared bytes"
    );
    check_snapshot(
        permit,
        control,
        ControlNamespace::Catalog,
        &manifest_name(permit.target.spec()),
        snapshot,
    )?;
    ensure!(
        control
            .read(ControlNamespace::Journals, &journal.prepared.name)?
            .is_none(),
        "published language Ready manifest has an unknown prepared source"
    );
    Ok(())
}

fn validate_tree(
    permit: &RecoveryPermit<'_>,
    original: &LanguageManifest,
    journal: &RecoveryJournal,
    committed: bool,
) -> Result<TreeInventory> {
    let observed = permit.inventory()?;
    original
        .inventory
        .ensure_same_root_and_unaffected_entries(&observed, &paths())?;
    for file in &journal.files {
        state(permit, &original.inventory, file, committed)?;
    }
    Ok(observed)
}

fn final_inventory(
    permit: &RecoveryPermit<'_>,
    original: &LanguageManifest,
    journal: &RecoveryJournal,
) -> Result<TreeInventory> {
    final_inventory_with_state(permit, original, journal, true)
}

fn final_inventory_with_state(
    permit: &RecoveryPermit<'_>,
    original: &LanguageManifest,
    journal: &RecoveryJournal,
    committed: bool,
) -> Result<TreeInventory> {
    let observed = validate_tree(permit, original, journal, committed)?;
    ensure!(
        observed.entries().len() == original.inventory.entries().len(),
        "restored bytecode inventory is incomplete"
    );
    for file in &journal.files {
        ensure!(
            observed
                .entries()
                .iter()
                .any(|entry| *entry == restored_fact(file)),
            "restored bytecode differs from the original archive"
        );
    }
    Ok(observed)
}

fn manifest_unchanged(
    permit: &RecoveryPermit<'_>,
    control: &WriterControl<'_>,
    expected: &ControlSnapshot,
    identity: &DiskIdentity,
) -> Result<()> {
    ensure!(
        expected.file_identity() == identity.file_identity(),
        "language Ready manifest pin changed"
    );
    check_snapshot(
        permit,
        control,
        ControlNamespace::Catalog,
        &manifest_name(permit.target.spec()),
        expected,
    )
}

fn check_snapshot(
    permit: &RecoveryPermit<'_>,
    control: &WriterControl<'_>,
    namespace: ControlNamespace,
    name: &str,
    expected: &ControlSnapshot,
) -> Result<()> {
    permit.check()?;
    ensure_entry_identity(
        control.namespace_handle(namespace),
        name,
        expected.file_identity(),
    )?;
    let current = control
        .read(namespace, name)?
        .context("bytecode recovery control manifest or journal disappeared")?;
    ensure!(
        current.file_identity() == expected.file_identity()
            && current.sha256() == expected.sha256()
            && current.bytes() == expected.bytes(),
        "bytecode recovery control manifest or journal changed"
    );
    Ok(())
}

fn report(
    permit: &RecoveryPermit<'_>,
    count: usize,
    restored: bool,
    healthy: bool,
    resumed: bool,
) -> BytecodeRecoveryReport {
    BytecodeRecoveryReport {
        instance: permit.target.key().filename(),
        paths: paths(),
        unchanged_entries: count - 4,
        restored,
        already_healthy: healthy,
        resumed,
    }
}

fn restore_bytecode_inner(
    context: &OperationContext,
    target: &PublishedLanguage,
    dry_run: bool,
    hook: &mut impl FnMut(RecoveryStep) -> Result<()>,
) -> Result<BytecodeRecoveryReport> {
    let permit = acquire(context, target, dry_run)?;
    let mut control = permit.writer.control(ControlLimits::default())?;
    manifest_unchanged(
        &permit,
        &control,
        &permit.snapshot,
        &permit.manifest_identity,
    )?;
    let name = journal_name(target);
    let existing = control.read(ControlNamespace::Journals, &name)?;
    let resumed = existing.is_some();
    let (journal, journal_snapshot, original, committed) = if let Some(snapshot) = existing {
        let journal: RecoveryJournal = serde_json::from_slice(snapshot.bytes())?;
        let original = validate_journal(&permit, &journal)?;
        let committed = permit.manifest_identity != journal.manifest_identity;
        if !committed {
            ensure!(
                permit.snapshot.bytes() == journal.manifest.as_bytes()
                    && permit.snapshot.sha256() == &journal.manifest_sha256,
                "original language Ready manifest identity changed"
            );
            prepared_manifest_snapshot(&permit, &control, &original, &journal)?;
        } else {
            check_published_prepared(&permit, &control, &original, &journal, &permit.snapshot)?;
            let inventory = final_inventory(&permit, &original, &journal)?;
            ensure!(
                inventory == final_manifest(&original, &journal.files)?.inventory,
                "language inventory changed outside prepared bytecode recovery"
            );
        }
        validate_tree(&permit, &original, &journal, committed)?;
        // Retry never adopts the cache's current bytes without digest checking.
        archive_bytes(&permit, &original_entries(&original)?)?;
        (journal, snapshot, original, committed)
    } else {
        let original = permit.manifest.clone();
        let entries = original_entries(&original)?;
        let observed = permit.inventory()?;
        if observed == original.inventory {
            manifest_unchanged(
                &permit,
                &control,
                &permit.snapshot,
                &permit.manifest_identity,
            )?;
            return Ok(report(
                &permit,
                observed.entries().len(),
                false,
                true,
                false,
            ));
        }
        original
            .inventory
            .ensure_same_root_and_unaffected_entries(&observed, &paths())?;
        let mut damaged = Vec::new();
        for entry in &entries {
            let actual = observed
                .entries()
                .iter()
                .find(|actual| actual.relative == entry.relative)
                .context("damaged bytecode member is missing without a recovery journal")?;
            ensure!(
                actual.uid == entry.uid
                    && actual.mode == entry.mode
                    && matches!(actual.kind, TreeInventoryKind::File { size, .. } if size <= MAX_BYTECODE_BYTES),
                "damaged bytecode has unknown type, owner, mode or size"
            );
            damaged.push(actual.clone());
        }
        let bytes = archive_bytes(&permit, &entries)?;
        if dry_run {
            manifest_unchanged(
                &permit,
                &control,
                &permit.snapshot,
                &permit.manifest_identity,
            )?;
            ensure!(
                permit.inventory()? == observed,
                "language inventory changed during recovery check"
            );
            return Ok(report(
                &permit,
                original.inventory.entries().len(),
                false,
                false,
                false,
            ));
        }
        manifest_unchanged(
            &permit,
            &control,
            &permit.snapshot,
            &permit.manifest_identity,
        )?;
        ensure!(
            permit.inventory()? == observed,
            "language inventory changed before recovery staging"
        );
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut stages = Vec::new();
        let staged = (|| -> Result<(RecoveryJournal, ControlSnapshot)> {
            let mut files = Vec::new();
            for (index, ((original, damaged), bytes)) in
                entries.into_iter().zip(damaged).zip(bytes).enumerate()
            {
                let stage = format!("bytecode-stage-{nonce}-{index}");
                let backup = format!("bytecode-backup-{nonce}-{index}");
                let snapshot = control.replace(ControlNamespace::Journals, &stage, None, &bytes)?;
                let stage_identity = DiskIdentity::from_file_identity(snapshot.file_identity());
                stages.push(snapshot);
                let (pin, _) = leaf(
                    &permit,
                    control.namespace_handle(ControlNamespace::Journals),
                    &stage,
                    &original.relative,
                )?
                .context("new bytecode recovery stage disappeared")?;
                ensure!(
                    FileIdentity::of(&pin)? == stage_identity.file_identity(),
                    "new bytecode stage changed after publication"
                );
                files.push(RecoveryFile {
                    original,
                    damaged,
                    stage,
                    stage_identity,
                    backup,
                });
                hook(RecoveryStep::AfterStage(index))?;
            }
            hook(RecoveryStep::BeforePreparedManifest)?;
            let prepared_name = format!("bytecode-manifest-{nonce}");
            let prepared_snapshot = control.replace(
                ControlNamespace::Journals,
                &prepared_name,
                None,
                &serde_json::to_vec(&final_manifest(&original, &files)?)?,
            )?;
            let prepared = PreparedManifest {
                name: prepared_name,
                identity: DiskIdentity::from_file_identity(prepared_snapshot.file_identity()),
                sha256: *prepared_snapshot.sha256(),
            };
            stages.push(prepared_snapshot);
            hook(RecoveryStep::AfterPreparedManifest)?;
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &prepared.name,
                stages
                    .last()
                    .expect("prepared manifest snapshot was retained"),
            )?;
            let journal = RecoveryJournal {
                schema: 1,
                key: target.key().fingerprint(),
                manifest: std::str::from_utf8(permit.snapshot.bytes())?.to_owned(),
                manifest_sha256: *permit.snapshot.sha256(),
                manifest_identity: permit.manifest_identity.clone(),
                prepared,
                files,
            };
            let snapshot = control.replace(
                ControlNamespace::Journals,
                &name,
                None,
                &serde_json::to_vec(&journal)?,
            )?;
            Ok((journal, snapshot))
        })();
        let (journal, snapshot) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                // Before WAL publication there have been no payload effects.
                // Clean only this invocation's still-identical 0600 stages.
                // An ambiguous journal publication or a true crash retains
                // evidence; it never licenses adoption or broad cleanup.
                if permit.check().is_ok()
                    && control
                        .read(ControlNamespace::Journals, &name)
                        .is_ok_and(|entry| entry.is_none())
                {
                    for stage in stages.iter().rev() {
                        let _ = control.remove(stage);
                    }
                }
                return Err(error);
            }
        };
        hook(RecoveryStep::AfterJournal)?;
        (journal, snapshot, original, false)
    };
    check_snapshot(
        &permit,
        &control,
        ControlNamespace::Journals,
        &name,
        &journal_snapshot,
    )?;
    let prepared_snapshot = if committed {
        check_published_prepared(&permit, &control, &original, &journal, &permit.snapshot)?;
        None
    } else {
        Some(prepared_manifest_snapshot(
            &permit, &control, &original, &journal,
        )?)
    };
    if dry_run {
        manifest_unchanged(
            &permit,
            &control,
            &permit.snapshot,
            &permit.manifest_identity,
        )?;
        validate_tree(&permit, &original, &journal, committed)?;
        check_snapshot(
            &permit,
            &control,
            ControlNamespace::Journals,
            &name,
            &journal_snapshot,
        )?;
        return Ok(report(
            &permit,
            original.inventory.entries().len(),
            false,
            false,
            resumed,
        ));
    }
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids bytecode recovery"
    );
    let mut published_snapshot = None;
    if !committed {
        let prepared = prepared_snapshot
            .as_ref()
            .expect("pending recovery pins its prepared manifest");
        for (index, file) in journal.files.iter().enumerate() {
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &name,
                &journal_snapshot,
            )?;
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &journal.prepared.name,
                prepared,
            )?;
            manifest_unchanged(
                &permit,
                &control,
                &permit.snapshot,
                &permit.manifest_identity,
            )?;
            let current = state(&permit, &original.inventory, file, false)?;
            if current == FileState::Installed {
                continue;
            }
            let journals = control.namespace_handle(ControlNamespace::Journals);
            let (stage, _) = leaf(&permit, journals, &file.stage, &file.original.relative)?
                .context("bytecode recovery stage disappeared")?;
            fchmod(&stage, Mode::from_bits_truncate(file.original.mode as _))?;
            stage.sync_all()?;
            journals.sync_all()?;
            hook(RecoveryStep::AfterChmod(index))?;
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &name,
                &journal_snapshot,
            )?;
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &journal.prepared.name,
                prepared,
            )?;
            manifest_unchanged(
                &permit,
                &control,
                &permit.snapshot,
                &permit.manifest_identity,
            )?;
            let parent = permit.parent(&original.inventory, &file.original.relative)?;
            let leaf_name = file
                .original
                .relative
                .file_name()
                .context("bytecode member has no name")?;
            if current == FileState::Pending {
                ensure!(
                    state(&permit, &original.inventory, file, false)? == FileState::Pending,
                    "bytecode changed before capture"
                );
                rename_noclobber(
                    &parent,
                    leaf_name,
                    journals,
                    std::ffi::OsStr::new(&file.backup),
                )?;
                parent.sync_all()?;
                journals.sync_all()?;
                ensure!(
                    state(&permit, &original.inventory, file, false)? == FileState::Captured,
                    "captured bytecode state changed"
                );
                hook(RecoveryStep::AfterCapture(index))?;
            }
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &name,
                &journal_snapshot,
            )?;
            check_snapshot(
                &permit,
                &control,
                ControlNamespace::Journals,
                &journal.prepared.name,
                prepared,
            )?;
            manifest_unchanged(
                &permit,
                &control,
                &permit.snapshot,
                &permit.manifest_identity,
            )?;
            ensure!(
                state(&permit, &original.inventory, file, false)? == FileState::Captured,
                "bytecode changed before restoration"
            );
            permit.recheck_parent(&original.inventory, &file.original.relative, &parent)?;
            rename_noclobber(
                journals,
                std::ffi::OsStr::new(&file.stage),
                &parent,
                leaf_name,
            )?;
            parent.sync_all()?;
            journals.sync_all()?;
            ensure!(
                state(&permit, &original.inventory, file, false)? == FileState::Installed,
                "restored bytecode state changed"
            );
            hook(RecoveryStep::AfterInstall(index))?;
        }
        hook(RecoveryStep::BeforeManifestCas)?;
        check_snapshot(
            &permit,
            &control,
            ControlNamespace::Journals,
            &name,
            &journal_snapshot,
        )?;
        let inventory = final_inventory_with_state(&permit, &original, &journal, false)?;
        manifest_unchanged(
            &permit,
            &control,
            &permit.snapshot,
            &permit.manifest_identity,
        )?;
        ensure!(
            inventory == final_manifest(&original, &journal.files)?.inventory,
            "restored language inventory differs from prepared manifest"
        );
        check_snapshot(
            &permit,
            &control,
            ControlNamespace::Journals,
            &journal.prepared.name,
            prepared,
        )?;
        published_snapshot = Some(control.replace_from_journal(
            &manifest_name(target.spec()),
            &permit.snapshot,
            prepared,
        )?);
        hook(RecoveryStep::AfterManifestCas)?;
    }
    let ready_snapshot = published_snapshot.as_ref().unwrap_or(&permit.snapshot);
    check_published_prepared(&permit, &control, &original, &journal, ready_snapshot)?;
    check_snapshot(
        &permit,
        &control,
        ControlNamespace::Catalog,
        &manifest_name(target.spec()),
        ready_snapshot,
    )?;
    check_snapshot(
        &permit,
        &control,
        ControlNamespace::Journals,
        &name,
        &journal_snapshot,
    )?;
    let inventory = final_inventory(&permit, &original, &journal)?;
    let current = control
        .read(ControlNamespace::Catalog, &manifest_name(target.spec()))?
        .context("language Ready manifest disappeared after recovery")?;
    let mut expected = original.clone();
    expected.inventory = inventory;
    ensure!(
        current.bytes() == serde_json::to_vec(&expected)?,
        "recovered language Ready manifest changed"
    );
    // Remove only the exact backup inodes recorded before any payload change.
    for (index, file) in journal.files.iter().enumerate() {
        permit.check()?;
        check_published_prepared(&permit, &control, &original, &journal, ready_snapshot)?;
        check_snapshot(
            &permit,
            &control,
            ControlNamespace::Catalog,
            &manifest_name(target.spec()),
            ready_snapshot,
        )?;
        check_snapshot(
            &permit,
            &control,
            ControlNamespace::Journals,
            &name,
            &journal_snapshot,
        )?;
        let journals = control.namespace_handle(ControlNamespace::Journals);
        if let Some((pin, fact)) = leaf(&permit, journals, &file.backup, &file.original.relative)? {
            ensure!(
                fact == file.damaged,
                "recovery backup changed before cleanup"
            );
            ensure_entry_identity(journals, &file.backup, FileIdentity::of(&pin)?)?;
            unlinkat(journals, file.backup.as_str(), UnlinkatFlags::NoRemoveDir)?;
            journals.sync_all()?;
        }
        hook(RecoveryStep::AfterBackupCleanup(index))?;
    }
    hook(RecoveryStep::BeforeJournalRemove)?;
    final_inventory(&permit, &original, &journal)?;
    check_published_prepared(&permit, &control, &original, &journal, ready_snapshot)?;
    check_snapshot(
        &permit,
        &control,
        ControlNamespace::Catalog,
        &manifest_name(target.spec()),
        ready_snapshot,
    )?;
    control.remove(&journal_snapshot)?;
    permit.check()?;
    Ok(report(
        &permit,
        original.inventory.entries().len(),
        true,
        false,
        resumed,
    ))
}

#[cfg(test)]
mod tests;
