//! Complete private runtime bytes in a real unpublished allocation.
//! This snapshot grants no permanent immutability or publication authority.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use eyre::{Context, Result, ensure};
use nix::fcntl::{AtFlags, OFlag, open, openat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat};
use sha2::{Digest as _, Sha256};

use super::prefix::UnpublishedPrefix;
use super::prefix::verify::{
    PrefixExpectedDirectory, PrefixExpectedFile, PrefixInventoryLimits, verify_prefix_inventory,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeSnapshotLimits {
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}
impl Default for RuntimeSnapshotLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_048_576,
            max_depth: 256,
            max_path_bytes: 16_384,
            max_total_path_bytes: 64 * 1024 * 1024,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
        }
    }
}

pub(crate) struct OwnedRuntimeSnapshot {
    prefix: UnpublishedPrefix,
    entrypoint: PathBuf,
    files: Vec<PrefixExpectedFile>,
    directories: Vec<PrefixExpectedDirectory>,
    inventory_sha256: [u8; 32],
    content_tree_sha256: [u8; 32],
}
impl OwnedRuntimeSnapshot {
    pub(crate) fn root(&self) -> &Path {
        self.prefix.final_path()
    }
    pub(crate) fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }
    pub(crate) fn files(&self) -> &[PrefixExpectedFile] {
        &self.files
    }
    pub(crate) fn directories(&self) -> &[PrefixExpectedDirectory] {
        &self.directories
    }
    pub(crate) fn inventory_sha256(&self) -> &[u8; 32] {
        &self.inventory_sha256
    }
    pub(crate) fn content_tree_sha256(&self) -> &[u8; 32] {
        &self.content_tree_sha256
    }
    pub(super) fn recheck(&self, limits: PrefixInventoryLimits) -> Result<()> {
        let actual = verify_prefix_inventory(&self.prefix, &self.files, &self.directories, limits)
            .context("runtime snapshot recheck inventory changed")?;
        ensure!(
            actual.inventory_sha256() == &self.inventory_sha256,
            "runtime snapshot recheck instance identity changed"
        );
        Ok(())
    }
}
impl std::fmt::Debug for OwnedRuntimeSnapshot {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("OwnedRuntimeSnapshot")
            .field("files", &self.files.len())
            .field("directories", &self.directories.len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn snapshot_runtime(
    source_root: &Path,
    executable_relative: &Path,
    destination: UnpublishedPrefix,
    limits: RuntimeSnapshotLimits,
) -> Result<OwnedRuntimeSnapshot> {
    snapshot_inner(
        source_root,
        executable_relative,
        destination,
        limits,
        || Ok(()),
    )
}

#[cfg(test)]
fn snapshot_with_source_hook(
    source_root: &Path,
    executable_relative: &Path,
    destination: UnpublishedPrefix,
    limits: RuntimeSnapshotLimits,
    hook: impl FnOnce() -> Result<()>,
) -> Result<OwnedRuntimeSnapshot> {
    snapshot_inner(source_root, executable_relative, destination, limits, hook)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    device: nix::libc::dev_t,
    inode: nix::libc::ino_t,
    mode: nix::libc::mode_t,
    links: nix::libc::nlink_t,
    owner: nix::libc::uid_t,
    group: nix::libc::gid_t,
    size: nix::libc::off_t,
    modified: nix::libc::time_t,
    modified_ns: nix::libc::c_long,
    changed: nix::libc::time_t,
    changed_ns: nix::libc::c_long,
}
impl SourceStamp {
    fn of(stat: FileStat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            links: stat.st_nlink,
            owner: stat.st_uid,
            group: stat.st_gid,
            size: stat.st_size,
            modified: stat.st_mtime,
            modified_ns: stat.st_mtime_nsec,
            changed: stat.st_ctime,
            changed_ns: stat.st_ctime_nsec,
        }
    }
    fn file(file: &File) -> Result<Self> {
        Ok(Self::of(fstat(file)?))
    }
    fn kind(self) -> SFlag {
        SFlag::from_bits_truncate(self.mode) & SFlag::S_IFMT
    }
    fn identity(self) -> (nix::libc::dev_t, nix::libc::ino_t) {
        (self.device, self.inode)
    }
}

struct SourceRoot {
    path: PathBuf,
    file: File,
    observed: SourceStamp,
}
impl SourceRoot {
    fn check_binding(&self) -> Result<()> {
        let current = open_absolute_directory(&self.path)?;
        ensure!(
            SourceStamp::file(&current)? == self.observed
                && SourceStamp::file(&self.file)? == self.observed,
            "runtime snapshot source root changed"
        );
        Ok(())
    }
    fn open_relative(&self, relative: &Path) -> Result<File> {
        let mut directory = self.file.try_clone()?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                eyre::bail!("runtime snapshot source path is unsafe");
            };
            let mut flags =
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
            if components.peek().is_some() {
                flags |= OFlag::O_DIRECTORY;
            }
            directory = File::from(openat(&directory, name, flags, Mode::empty())?);
        }
        Ok(directory)
    }
    fn observe_relative(&self, relative: &Path) -> Result<SourceStamp> {
        if relative.as_os_str().is_empty() {
            return SourceStamp::file(&self.file);
        }
        let parent = self.open_relative(relative.parent().unwrap_or_else(|| Path::new("")))?;
        let name = relative
            .file_name()
            .ok_or_else(|| eyre::eyre!("runtime snapshot source entry has no name"))?;
        Ok(SourceStamp::of(fstatat(
            &parent,
            name,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?))
    }
    fn resolve_link(&self, relative: &Path) -> Result<PathBuf> {
        self.check_binding()?;
        let canonical = self.path.join(relative).canonicalize()?;
        let contained = canonical
            .strip_prefix(&self.path)
            .wrap_err("runtime snapshot source symlink escapes its root")?;
        Ok(contained.into())
    }
}

struct PlannedFile {
    logical: PathBuf,
    physical: PathBuf,
    observed: SourceStamp,
    size: u64,
    mode: u32,
}
struct SourceObservation {
    observed: SourceStamp,
    link_target: Option<PathBuf>,
}
struct Planner<'a> {
    source: &'a SourceRoot,
    limits: RuntimeSnapshotLimits,
    entries: usize,
    path_bytes: usize,
    total_bytes: u64,
    files: Vec<PlannedFile>,
    directories: Vec<PrefixExpectedDirectory>,
    observations: BTreeMap<PathBuf, SourceObservation>,
}
impl Planner<'_> {
    fn charge_name(&mut self, path: &Path) -> Result<()> {
        let length = path.as_os_str().as_encoded_bytes().len();
        ensure!(
            length <= self.limits.max_path_bytes
                && path.components().count() <= self.limits.max_depth,
            "runtime snapshot path or depth limit exceeded"
        );
        self.path_bytes = self
            .path_bytes
            .checked_add(length)
            .filter(|total| *total <= self.limits.max_total_path_bytes)
            .ok_or_else(|| eyre::eyre!("runtime snapshot total path limit exceeded"))?;
        Ok(())
    }
    fn observe(
        &mut self,
        physical: &Path,
        observed: SourceStamp,
        link_target: Option<PathBuf>,
    ) -> Result<()> {
        if let Some(previous) = self.observations.get(physical) {
            ensure!(
                previous.observed == observed && previous.link_target == link_target,
                "runtime snapshot source changed during planning"
            );
            return Ok(());
        }
        self.charge_name(physical)?;
        if let Some(target) = &link_target {
            self.charge_name(target)?;
        }
        self.observations.insert(
            physical.into(),
            SourceObservation {
                observed,
                link_target,
            },
        );
        Ok(())
    }
    fn walk(
        &mut self,
        opened: File,
        physical: &Path,
        logical: &Path,
        active: &mut Vec<(nix::libc::dev_t, nix::libc::ino_t)>,
    ) -> Result<()> {
        let observed = SourceStamp::file(&opened)?;
        self.observe(physical, observed, None)?;
        let metadata = opened.metadata()?;
        ensure!(
            metadata.mode() & 0o7000 == 0,
            "runtime snapshot source has privileged mode bits"
        );
        if metadata.is_file() {
            ensure!(
                metadata.len() <= self.limits.max_file_bytes,
                "runtime snapshot file byte limit exceeded"
            );
            self.total_bytes = self
                .total_bytes
                .checked_add(metadata.len())
                .filter(|total| *total <= self.limits.max_total_bytes)
                .ok_or_else(|| eyre::eyre!("runtime snapshot total byte limit exceeded"))?;
            self.charge_name(physical)?;
            self.files.push(PlannedFile {
                logical: logical.into(),
                physical: physical.into(),
                observed,
                size: metadata.len(),
                mode: metadata.mode() & 0o777 & !0o022,
            });
            return Ok(());
        }
        ensure!(metadata.is_dir(), "runtime snapshot source entry is unsafe");
        ensure!(
            metadata.mode() & 0o500 == 0o500,
            "runtime snapshot source directory is not readable and traversable"
        );
        nix::unistd::faccessat(
            &opened,
            ".",
            nix::unistd::AccessFlags::R_OK | nix::unistd::AccessFlags::X_OK,
            AtFlags::AT_EACCESS,
        )?;
        ensure!(
            !active.contains(&observed.identity()),
            "runtime snapshot source directory cycle"
        );
        active.push(observed.identity());
        if !logical.as_os_str().is_empty() {
            self.directories.push(PrefixExpectedDirectory {
                relative: logical.into(),
                mode: 0o755,
            });
        }
        let mut directory = nix::dir::Dir::openat(
            &opened,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        for entry in directory.iter() {
            let entry = entry?;
            let name_bytes = entry.file_name().to_bytes();
            if matches!(name_bytes, b"." | b"..") {
                continue;
            }
            self.entries = self
                .entries
                .checked_add(1)
                .filter(|count| *count <= self.limits.max_entries)
                .ok_or_else(|| eyre::eyre!("runtime snapshot entry limit exceeded"))?;
            let name = std::ffi::OsStr::from_bytes(name_bytes);
            let next_logical = logical.join(name);
            let next_physical = physical.join(name);
            self.charge_name(&next_logical)?;
            self.charge_name(&next_physical)?;
            let entry_stat = SourceStamp::of(fstatat(&opened, name, AtFlags::AT_SYMLINK_NOFOLLOW)?);
            if entry_stat.kind() == SFlag::S_IFLNK {
                ensure!(
                    entry_stat.mode & 0o7000 == 0,
                    "runtime snapshot source link has privileged mode bits"
                );
                let target = self.source.resolve_link(&next_physical)?;
                self.observe(&next_physical, entry_stat, Some(target.clone()))?;
                let target_file = self.source.open_relative(&target)?;
                self.walk(target_file, &target, &next_logical, active)?;
            } else {
                ensure!(
                    matches!(entry_stat.kind(), SFlag::S_IFREG | SFlag::S_IFDIR),
                    "runtime snapshot source entry is not regular or a directory"
                );
                let file = File::from(openat(
                    &opened,
                    name,
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
                ensure!(
                    SourceStamp::file(&file)? == entry_stat,
                    "runtime snapshot source entry changed while opening"
                );
                self.walk(file, &next_physical, &next_logical, active)?;
            }
        }
        ensure!(
            SourceStamp::file(&opened)? == observed,
            "runtime snapshot source directory changed while planning"
        );
        active.pop();
        Ok(())
    }
    fn check_sources(&self) -> Result<()> {
        self.source.check_binding()?;
        for (relative, expected) in &self.observations {
            ensure!(
                self.source.observe_relative(relative)? == expected.observed,
                "runtime snapshot source entry changed"
            );
            if let Some(target) = &expected.link_target {
                ensure!(
                    self.source.resolve_link(relative)? == *target,
                    "runtime snapshot source link target changed"
                );
            }
        }
        self.source.check_binding()?;
        Ok(())
    }
}

fn open_absolute_directory(path: &Path) -> Result<File> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "runtime snapshot source root is not an absolute safe path"
    );
    let mut file = File::from(open(
        Path::new("/"),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            file = File::from(openat(
                &file,
                name,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?);
        }
    }
    Ok(file)
}

fn inventory_limits(limits: RuntimeSnapshotLimits) -> PrefixInventoryLimits {
    PrefixInventoryLimits {
        max_files: limits.max_entries,
        max_directories: limits.max_entries,
        max_components: limits.max_depth,
        max_path_bytes: limits.max_path_bytes,
        max_total_path_bytes: limits.max_total_path_bytes,
        max_file_bytes: limits.max_file_bytes,
        max_total_bytes: limits.max_total_bytes,
    }
}

fn snapshot_inner(
    source_root: &Path,
    executable_relative: &Path,
    mut destination: UnpublishedPrefix,
    limits: RuntimeSnapshotLimits,
    before_copy: impl FnOnce() -> Result<()>,
) -> Result<OwnedRuntimeSnapshot> {
    ensure!(
        limits.max_entries != 0
            && limits.max_depth != 0
            && limits.max_path_bytes != 0
            && limits.max_total_path_bytes != 0
            && limits.max_file_bytes != 0
            && limits.max_total_bytes != 0,
        "runtime snapshot limits must be nonzero"
    );
    ensure!(
        !executable_relative.as_os_str().is_empty()
            && !executable_relative.is_absolute()
            && executable_relative
                .as_os_str()
                .as_encoded_bytes()
                .split(|byte| *byte == b'/')
                .all(|part| !part.is_empty() && !matches!(part, b"." | b".."))
            && executable_relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "runtime snapshot executable must be a safe relative path"
    );
    ensure!(
        executable_relative.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes
            && executable_relative.components().count() <= limits.max_depth,
        "runtime snapshot executable path or depth limit exceeded"
    );
    verify_prefix_inventory(&destination, &[], &[], inventory_limits(limits))
        .wrap_err("runtime snapshot destination must be an empty ready prefix")?;
    ensure!(
        source_root.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
        "runtime snapshot source root path limit exceeded"
    );
    let file = open_absolute_directory(source_root)?;
    let source = SourceRoot {
        path: source_root.canonicalize()?,
        observed: SourceStamp::file(&file)?,
        file,
    };
    source.check_binding()?;
    let final_root = destination.final_path().canonicalize()?;
    ensure!(
        !source.path.starts_with(&final_root) && !final_root.starts_with(&source.path),
        "runtime snapshot source and destination roots overlap"
    );
    let mut plan = Planner {
        source: &source,
        limits,
        entries: 0,
        path_bytes: 0,
        total_bytes: 0,
        files: Vec::new(),
        directories: Vec::new(),
        observations: BTreeMap::new(),
    };
    plan.walk(
        source.file.try_clone()?,
        Path::new(""),
        Path::new(""),
        &mut Vec::new(),
    )?;
    ensure!(
        plan.files
            .iter()
            .any(|file| file.logical == executable_relative && file.mode & 0o100 != 0),
        "runtime snapshot executable is missing, not regular or not executable"
    );
    for relative in plan.files.iter().map(|file| file.logical.as_path()).chain(
        plan.directories
            .iter()
            .map(|directory| directory.relative.as_path()),
    ) {
        ensure!(
            final_root
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add(1)
                .and_then(|length| length.checked_add(relative.as_os_str().as_encoded_bytes().len()))
                .is_some_and(|length| length <= limits.max_path_bytes),
            "runtime snapshot destination path limit exceeded"
        );
    }
    before_copy()?;
    plan.check_sources()?;
    plan.directories.sort_by(|a, b| a.relative.cmp(&b.relative));
    plan.files.sort_by(|a, b| a.logical.cmp(&b.logical));
    for directory in &plan.directories {
        destination.create_dir_all(&directory.relative)?;
    }
    let mut files = Vec::with_capacity(plan.files.len());
    for expected in &plan.files {
        let mut input = source.open_relative(&expected.physical)?;
        ensure!(
            input.metadata()?.is_file() && SourceStamp::file(&input)? == expected.observed,
            "runtime snapshot source file changed before copy"
        );
        let mut writer = destination.create_file_exclusive(&expected.logical, expected.mode)?;
        let mut hash = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let remaining = expected.size - copied;
            let request = usize::try_from(remaining.saturating_add(1).min(buffer.len() as u64))?;
            let read = input.read(&mut buffer[..request])?;
            if read == 0 {
                break;
            }
            let next = copied
                .checked_add(u64::try_from(read)?)
                .filter(|next| *next <= expected.size)
                .ok_or_else(|| eyre::eyre!("runtime snapshot source file grew during copy"))?;
            writer.write_all(&buffer[..read])?;
            hash.update(&buffer[..read]);
            copied = next;
        }
        ensure!(
            copied == expected.size && SourceStamp::file(&input)? == expected.observed,
            "runtime snapshot source file changed during copy"
        );
        let receipt = writer.finish()?;
        ensure!(
            receipt.relative_path() == expected.logical
                && receipt.size() == expected.size
                && receipt.mode() == expected.mode,
            "runtime snapshot output receipt changed"
        );
        files.push(PrefixExpectedFile {
            relative: expected.logical.clone(),
            sha256: hash.finalize().into(),
            size: copied,
            mode: expected.mode,
        });
    }
    let inventory = verify_prefix_inventory(
        &destination,
        &files,
        &plan.directories,
        inventory_limits(limits),
    )?;
    plan.check_sources()?;
    let content_tree_sha256 = content_tree_digest(executable_relative, &files, &plan.directories);
    Ok(OwnedRuntimeSnapshot {
        entrypoint: destination.final_path().join(executable_relative),
        prefix: destination,
        files,
        directories: plan.directories,
        inventory_sha256: *inventory.inventory_sha256(),
        content_tree_sha256,
    })
}

fn content_tree_digest(
    executable: &Path,
    files: &[PrefixExpectedFile],
    directories: &[PrefixExpectedDirectory],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"pakro-runtime-snapshot-content\0");
    digest.update(1u32.to_le_bytes());
    let executable = executable.as_os_str().as_encoded_bytes();
    digest.update((executable.len() as u64).to_le_bytes());
    digest.update(executable);
    let mut entries: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.relative.as_os_str().as_encoded_bytes(),
                b'f',
                file.mode,
                file.size,
                file.sha256,
            )
        })
        .chain(directories.iter().map(|directory| {
            (
                directory.relative.as_os_str().as_encoded_bytes(),
                b'd',
                directory.mode,
                0,
                [0; 32],
            )
        }))
        .collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (path, kind, mode, size, hash) in entries {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path);
        digest.update([kind]);
        digest.update(mode.to_le_bytes());
        digest.update(size.to_le_bytes());
        digest.update(hash);
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests;
