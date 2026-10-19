//! Explicit digest-bound base runtime inputs, separate from layout value DTOs.
//!
//! Inputs are pinned before execution, observed under one deadline, and retained
//! as anonymous snapshots. This does not validate or publish an environment.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Context, ContextCompat, Result, bail, ensure};
use pakro_index::artifact::SourceArtifact;
use sha2::{Digest as _, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use crate::artifact_snapshot::VerifiedArtifact;
use crate::python_runtime::{PythonRuntimeDescriptor, PythonRuntimeHandle};

use super::prefix::UnpublishedPrefix;
use super::runtime_layout::{RuntimeLayoutLimits, RuntimeLayoutObservation};

pub(super) mod startup;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeInputPurpose {
    Executable,
    LoaderPayload,
    Header,
    SysconfigData,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeInputExpectation {
    pub path: PathBuf,
    pub purpose: RuntimeInputPurpose,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mode: u32,
}

/// This explicit caller intent is not an already-verified reader capability.
#[derive(Debug)]
pub(crate) enum RuntimeSourceTrust<'a> {
    Managed {
        source: &'a SourceArtifact,
        root: &'a Path,
    },
    ExplicitExisting,
}

#[derive(Debug)]
pub(crate) struct RuntimeSourceBinding<'a> {
    pub trust: RuntimeSourceTrust<'a>,
    pub authorized_roots: &'a [PathBuf],
    pub inputs: &'a [RuntimeInputExpectation],
}

pub(crate) struct BoundRuntimeSources {
    observed: PythonRuntimeDescriptor,
    layout: RuntimeLayoutObservation,
    inputs: Vec<RuntimeInputReceipt>,
    _snapshots: Vec<VerifiedArtifact>,
    _managed_source: Option<SourceArtifact>,
}

impl BoundRuntimeSources {
    pub(crate) fn observed_runtime(&self) -> &PythonRuntimeDescriptor {
        &self.observed
    }
    pub(crate) fn layout(&self) -> &RuntimeLayoutObservation {
        &self.layout
    }
    pub(crate) fn inputs(&self) -> &[RuntimeInputReceipt] {
        &self.inputs
    }

    pub(crate) fn copy_input(
        &mut self,
        index: usize,
        output: &mut dyn Write,
    ) -> Result<RuntimeInputReceipt> {
        let receipt = self
            .inputs
            .get(index)
            .context("runtime input index is out of range")?
            .clone();
        let snapshot = self
            ._snapshots
            .get_mut(index)
            .context("runtime input index has no verified snapshot")?;
        snapshot
            .rewind()
            .context("runtime input snapshot seek failed")?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let request = (receipt.size - size)
                .saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let count = snapshot
                .read(&mut buffer[..request])
                .context("runtime input snapshot read failed")?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .filter(|size| *size <= receipt.size)
                .context("runtime input snapshot size changed")?;
            hasher.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .context("runtime input write failed")?;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        ensure!(
            size == receipt.size && digest == receipt.sha256,
            "runtime input snapshot statistics changed"
        );
        Ok(receipt)
    }
}

impl std::fmt::Debug for BoundRuntimeSources {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("BoundRuntimeSources")
            .field("implementation", &self.observed.implementation)
            .field("language", &self.observed.language_version)
            .field("input_count", &self.inputs.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeInputReceipt {
    path: PathBuf,
    purpose: RuntimeInputPurpose,
    sha256: [u8; 32],
    size: u64,
    mode: u32,
}

impl RuntimeInputReceipt {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn purpose(&self) -> RuntimeInputPurpose {
        self.purpose
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
}

#[cfg(unix)]
pub(crate) async fn capture_runtime_sources(
    selected: &PythonRuntimeHandle,
    descriptor: &PythonRuntimeDescriptor,
    source: RuntimeSourceBinding<'_>,
    prefix: &UnpublishedPrefix,
    limits: RuntimeLayoutLimits,
    timeout: Duration,
) -> Result<BoundRuntimeSources> {
    ensure!(
        !timeout.is_zero(),
        "runtime source timeout must be positive"
    );
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .context("runtime source timeout overflow")?;
    capture_runtime_sources_at_deadline(selected, descriptor, source, prefix, limits, deadline)
        .await
}

#[cfg(unix)]
pub(super) async fn capture_runtime_sources_at_deadline(
    selected: &PythonRuntimeHandle,
    descriptor: &PythonRuntimeDescriptor,
    source: RuntimeSourceBinding<'_>,
    prefix: &UnpublishedPrefix,
    limits: RuntimeLayoutLimits,
    deadline: tokio::time::Instant,
) -> Result<BoundRuntimeSources> {
    use std::io::Seek;
    use tokio::time::Instant;
    ensure!(Instant::now() < deadline, "runtime source deadline expired");
    super::runtime_layout::validate_limits(limits).context("runtime source limit is invalid")?;
    prefix
        .check_binding()
        .context("unpublished prefix cannot bind runtime sources")?;
    ensure!(
        source.inputs.len() <= limits.max_input_files
            && source.authorized_roots.len() <= limits.max_paths,
        "runtime source limit exceeded"
    );
    let mut total = 0u64;
    for input in source.inputs {
        ensure!(
            input.size <= limits.max_file_bytes,
            "runtime source limit exceeded"
        );
        total = total
            .checked_add(input.size)
            .filter(|value| *value <= limits.max_total_bytes)
            .context("runtime source limit exceeded")?;
    }
    ensure!(
        !source.authorized_roots.is_empty(),
        "authorized runtime roots are empty"
    );
    let mut roots = Vec::new();
    for root in source.authorized_roots {
        validate_source_path(root, limits)?;
        let canonical = root
            .canonicalize()
            .context("authorized runtime roots cannot be resolved")?;
        let opened = open_source_directory(&canonical)
            .context("authorized runtime roots cannot be opened")?;
        ensure!(
            opened.metadata()?.is_dir(),
            "authorized runtime roots are not directories"
        );
        roots.push(canonical);
    }
    let managed = match source.trust {
        RuntimeSourceTrust::ExplicitExisting => None,
        RuntimeSourceTrust::Managed { source, root } => {
            let root = root
                .canonicalize()
                .context("authorized runtime roots managed binding is invalid")?;
            ensure!(
                roots.contains(&root) && selected.executable().starts_with(&root),
                "authorized runtime roots differ from managed binding"
            );
            Some(source.clone())
        }
    };
    let mut declared_paths = std::collections::BTreeSet::new();
    for input in source.inputs {
        ensure!(
            declared_paths.insert(&input.path),
            "duplicate runtime input"
        );
    }
    let executable_count = source
        .inputs
        .iter()
        .filter(|input| input.purpose == RuntimeInputPurpose::Executable)
        .count();
    ensure!(
        executable_count == 1,
        "runtime executable input must occur exactly once"
    );
    let mut identities = std::collections::BTreeSet::new();
    let mut checked = Vec::new();
    for input in source.inputs {
        validate_source_path(&input.path, limits)?;
        let canonical = input
            .path
            .canonicalize()
            .context("runtime input cannot be resolved")?;
        ensure!(
            roots.iter().any(|root| canonical.starts_with(root)),
            "runtime input escapes authorized runtime roots"
        );
        ensure!(
            identities.insert(canonical.clone()),
            "duplicate runtime input"
        );
        if input.purpose == RuntimeInputPurpose::Executable {
            ensure!(
                canonical == selected.executable(),
                "runtime executable input differs from selected executable"
            );
        }
        ensure!(input.mode <= 0o777, "runtime input mode is invalid");
        let file = open_source_file(&canonical).context("runtime input could not be opened")?;
        let metadata = file
            .metadata()
            .context("runtime input metadata unavailable")?;
        ensure!(
            metadata.is_file()
                && metadata.len() == input.size
                && metadata.permissions().mode() & 0o777 == input.mode
                && metadata.permissions().mode() & 0o7000 == 0,
            "runtime input size, mode or type disagrees"
        );
        if input.purpose == RuntimeInputPurpose::Executable {
            ensure!(
                input.mode & 0o111 != 0,
                "runtime executable input is not executable"
            );
        }
        checked.push((input, canonical, file, metadata.dev(), metadata.ino()));
    }
    let scratch = tempfile::tempdir().context("runtime input scratch creation failed")?;
    let mut inputs = Vec::new();
    let mut snapshots = Vec::new();
    for (input, _, file, _, _) in &mut checked {
        ensure!(Instant::now() < deadline, "runtime source timeout expired");
        let snapshot = crate::artifact_snapshot::spool_verified_reader(
            file,
            "runtime-input",
            pakro_store::Digest::Sha256(input.sha256),
            limits.max_file_bytes,
            scratch.path(),
        )
        .context("runtime input digest or snapshot disagrees")?;
        ensure!(
            snapshot.size() == input.size,
            "runtime input snapshot size disagrees"
        );
        inputs.push(RuntimeInputReceipt {
            path: input.path.clone(),
            purpose: input.purpose,
            sha256: input.sha256,
            size: input.size,
            mode: input.mode,
        });
        snapshots.push(snapshot);
    }
    let observed = crate::python_runtime::probe_at_deadline(
        selected,
        deadline,
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
    )
    .await
    .context("runtime descriptor changed or repeated probe failed")?;
    ensure!(
        observed == *descriptor,
        "runtime descriptor changed from supplied observation"
    );
    let raw = crate::python_runtime::probe_layout(
        selected,
        prefix.final_path(),
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("runtime source layout observation failed")?;
    let layout =
        super::runtime_layout::decode_runtime_layout(&raw, &observed, prefix.final_path(), limits)?;
    for (input, canonical, file, device, inode) in &mut checked {
        ensure!(Instant::now() < deadline, "runtime source timeout expired");
        let now = input
            .path
            .canonicalize()
            .context("runtime input changed during observation")?;
        ensure!(
            &now == canonical,
            "runtime input changed during observation"
        );
        let current =
            open_source_file(canonical).context("runtime input changed during observation")?;
        let metadata = current.metadata()?;
        ensure!(
            metadata.dev() == *device
                && metadata.ino() == *inode
                && metadata.len() == input.size
                && metadata.permissions().mode() & 0o777 == input.mode
                && metadata.permissions().mode() & 0o7000 == 0,
            "runtime input changed during observation"
        );
        file.rewind()?;
        ensure!(
            hash_input(file, input.size)? == input.sha256,
            "runtime input changed during observation"
        );
        match input.purpose {
            RuntimeInputPurpose::Executable => {}
            RuntimeInputPurpose::LoaderPayload => {
                ensure!(
                    matches!(&layout.loader,super::runtime_layout::RuntimeLoaderObservation::Observed {images} if images.contains(canonical)),
                    "loader observation binding does not include runtime input"
                );
            }
            RuntimeInputPurpose::Header => {
                ensure!(["python_h", "plat_python_h"].iter().filter_map(|key|layout.files.get(*key)).any(|fact|matches!(&fact.status,super::runtime_layout::RuntimeLayoutPathStatus::Present {canonical:observed,regular_file:true,size,mode} if observed.as_path() == canonical.as_path() && *size == input.size && *mode == input.mode)), "runtime header observation binding disagrees");
            }
            RuntimeInputPurpose::SysconfigData => {
                bail!("runtime sysconfig data observation is unavailable")
            }
        }
    }
    ensure!(Instant::now() < deadline, "runtime source timeout expired");
    prefix
        .check_binding()
        .context("unpublished prefix changed during runtime observation")?;
    Ok(BoundRuntimeSources {
        observed,
        layout,
        inputs,
        _snapshots: snapshots,
        _managed_source: managed,
    })
}

#[cfg(unix)]
fn validate_source_path(path: &Path, limits: RuntimeLayoutLimits) -> Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    ensure!(
        path.is_absolute()
            && !bytes.contains(&0)
            && !path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            && bytes.len() <= limits.max_path_bytes,
        "runtime input path or authorized runtime roots are invalid"
    );
    Ok(())
}

#[cfg(unix)]
fn open_source_directory(path: &Path) -> Result<std::fs::File> {
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode;
    let mut directory = std::fs::File::from(open(
        Path::new("/"),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let metadata = directory.metadata()?;
        ensure!(
            (metadata.uid() == 0 || metadata.uid() == nix::unistd::geteuid().as_raw())
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
            "authorized runtime roots have an unsafe ancestor"
        );
        directory = std::fs::File::from(openat(
            &directory,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
    }
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir()
            && (metadata.uid() == 0 || metadata.uid() == nix::unistd::geteuid().as_raw())
            && metadata.mode() & 0o022 == 0,
        "authorized runtime roots have unsafe ownership or mode"
    );
    Ok(directory)
}

#[cfg(unix)]
fn open_source_file(path: &Path) -> Result<std::fs::File> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    let parent = open_source_directory(path.parent().context("runtime input has no parent")?)?;
    let file = std::fs::File::from(openat(
        &parent,
        path.file_name().context("runtime input has no name")?,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    ensure!(
        file.metadata()?.is_file(),
        "runtime input is not a regular file"
    );
    Ok(file)
}

fn hash_input(reader: &mut impl std::io::Read, expected_size: u64) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let request = (expected_size - size)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = reader
            .read(&mut buffer[..request])
            .context("runtime input read failed")?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .filter(|size| *size <= expected_size)
            .context("runtime input exceeds pinned size")?;
        hasher.update(&buffer[..count]);
    }
    ensure!(size == expected_size, "runtime input size differs from pin");
    Ok(hasher.finalize().into())
}

#[cfg(not(unix))]
pub(crate) async fn capture_runtime_sources(
    _selected: &PythonRuntimeHandle,
    _descriptor: &PythonRuntimeDescriptor,
    _source: RuntimeSourceBinding<'_>,
    _prefix: &UnpublishedPrefix,
    _limits: RuntimeLayoutLimits,
    _timeout: Duration,
) -> Result<BoundRuntimeSources> {
    bail!("runtime source capture requires macOS or Linux")
}

#[cfg(not(unix))]
pub(super) async fn capture_runtime_sources_at_deadline(
    _selected: &PythonRuntimeHandle,
    _descriptor: &PythonRuntimeDescriptor,
    _source: RuntimeSourceBinding<'_>,
    _prefix: &UnpublishedPrefix,
    _limits: RuntimeLayoutLimits,
    _deadline: tokio::time::Instant,
) -> Result<BoundRuntimeSources> {
    bail!("runtime source capture requires macOS or Linux")
}

#[cfg(all(test, unix))]
mod tests;
