//! Plan and copy selected runtime inputs into an owned final prefix.
//!
//! Planning and controlled copy follow independently observed behavior RED.
//! A successful copy/startup result is not D3 normal-site layout or E publication.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Context, ContextCompat, Result, bail, ensure};
use sha2::{Digest as _, Sha256};

use super::super::python_wheel::WheelScheme;
use super::super::python_wheel_plan::{
    WheelInstallScheme, WheelReservedDestination, WheelReservedKind,
};
use crate::python_runtime::PythonRuntimeDescriptor;

use super::prefix::UnpublishedPrefix;
use super::runtime_source::{BoundRuntimeSources, RuntimeInputPurpose};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeBootstrapLimits {
    pub max_files: usize,
    pub max_directories: usize,
    pub max_generated_bytes: usize,
    pub max_total_bytes: u64,
    pub max_path_bytes: usize,
}
impl Default for RuntimeBootstrapLimits {
    fn default() -> Self {
        Self {
            max_files: 4096,
            max_directories: 1_048_576,
            max_generated_bytes: 64 * 1024,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
            max_path_bytes: 16_384,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeBootstrapFileKind {
    Executable,
    LoaderPayload,
    Header,
    Configuration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeBootstrapFile {
    relative: PathBuf,
    sha256: [u8; 32],
    size: u64,
    mode: u32,
    kind: RuntimeBootstrapFileKind,
}
impl RuntimeBootstrapFile {
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
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
    pub(crate) fn kind(&self) -> RuntimeBootstrapFileKind {
        self.kind
    }
}

pub(crate) struct RuntimeBootstrapPlan {
    sources: BoundRuntimeSources,
    final_prefix: PathBuf,
    allocation: String,
    executable: PathBuf,
    files: Vec<RuntimeBootstrapFile>,
    scheme: WheelInstallScheme,
    reserved: Vec<WheelReservedDestination>,
    writes: Vec<RuntimeBootstrapBytes>,
}

enum RuntimeBootstrapBytes {
    Input(usize),
    Generated(Vec<u8>),
}
impl RuntimeBootstrapPlan {
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }
    pub(crate) fn files(&self) -> &[RuntimeBootstrapFile] {
        &self.files
    }
    pub(crate) fn scheme_candidate(&self) -> &WheelInstallScheme {
        &self.scheme
    }
    pub(crate) fn reserved(&self) -> &[WheelReservedDestination] {
        &self.reserved
    }
}
impl std::fmt::Debug for RuntimeBootstrapPlan {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("RuntimeBootstrapPlan")
            .field("allocation", &self.allocation)
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}

pub(crate) struct BootstrappedRuntime {
    sources: BoundRuntimeSources,
    final_prefix: PathBuf,
    allocation: String,
    executable: PathBuf,
    observed: PythonRuntimeDescriptor,
    files: Vec<RuntimeBootstrapFile>,
    scheme: WheelInstallScheme,
    reserved: Vec<WheelReservedDestination>,
}
impl BootstrappedRuntime {
    pub(super) fn source_binding(&self) -> &BoundRuntimeSources {
        &self.sources
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }
    pub(crate) fn observed_site_disabled(&self) -> &PythonRuntimeDescriptor {
        &self.observed
    }
    pub(crate) fn files(&self) -> &[RuntimeBootstrapFile] {
        &self.files
    }
    pub(crate) fn scheme_candidate(&self) -> &WheelInstallScheme {
        &self.scheme
    }
    pub(crate) fn reserved(&self) -> &[WheelReservedDestination] {
        &self.reserved
    }
}
impl std::fmt::Debug for BootstrappedRuntime {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("BootstrappedRuntime")
            .field("allocation", &self.allocation)
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn prepare_runtime_bootstrap(
    sources: BoundRuntimeSources,
    prefix: &UnpublishedPrefix,
    limits: RuntimeBootstrapLimits,
) -> Result<RuntimeBootstrapPlan> {
    ensure!(
        limits.max_files != 0
            && limits.max_directories != 0
            && limits.max_generated_bytes != 0
            && limits.max_total_bytes != 0
            && limits.max_path_bytes != 0,
        "bootstrap limit values must be nonzero"
    );
    ensure!(
        sources.layout().target_prefix == prefix.final_path(),
        "bootstrap final prefix differs from source observations"
    );
    prefix
        .check_binding()
        .context("unpublished prefix cannot receive runtime bootstrap")?;
    ensure!(
        prefix.final_path().as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
        "bootstrap limit: final prefix path exceeds budget"
    );
    let projected_files = sources
        .inputs()
        .len()
        .checked_add(2)
        .context("bootstrap limit: file count overflow")?;
    ensure!(
        projected_files <= limits.max_files,
        "bootstrap limit: file count exceeded"
    );
    ensure!(
        sources.layout().base_prefix.is_some() && sources.layout().base_exec_prefix.is_some(),
        "runtime requires complete copied-prefix inputs before legacy bootstrap"
    );
    let final_prefix = prefix.final_path().to_path_buf();
    let target = &sources.layout().target_paths;
    let scheme = WheelInstallScheme::new(
        final_prefix.clone(),
        [
            (WheelScheme::Purelib, "purelib"),
            (WheelScheme::Platlib, "platlib"),
            (WheelScheme::Headers, "headers"),
            (WheelScheme::Scripts, "scripts"),
            (WheelScheme::Data, "data"),
        ]
        .into_iter()
        .map(|(kind, key)| {
            target
                .get(key)
                .cloned()
                .map(|path| (kind, path))
                .context("bootstrap destination scheme is incomplete")
        })
        .collect::<Result<_>>()?,
    )?;
    let scripts = scheme
        .root(WheelScheme::Scripts)
        .context("bootstrap destination scripts root is absent")?;
    let executable = scripts.join("python");
    let original = sources.observed_runtime().executable.as_path();
    let original_parent = original
        .parent()
        .context("bootstrap executable source has no parent")?;
    let mut output = BTreeMap::new();
    let mut total = 0u64;
    for (index, input) in sources.inputs().iter().enumerate() {
        let (paths, kind, mode) = match input.purpose() {
            RuntimeInputPurpose::Executable => {
                ensure!(
                    input.path() == original,
                    "bootstrap executable input differs from runtime binding"
                );
                (
                    vec![scripts.join("python"), scripts.join("python3")],
                    RuntimeBootstrapFileKind::Executable,
                    0o755,
                )
            }
            RuntimeInputPurpose::LoaderPayload => {
                let relative = relative_path(original_parent, input.path())?;
                (
                    vec![contained_path(&final_prefix, &scripts.join(relative))?],
                    RuntimeBootstrapFileKind::LoaderPayload,
                    input.mode(),
                )
            }
            RuntimeInputPurpose::Header => {
                let suffix = ["include", "platinclude"]
                    .iter()
                    .filter_map(|key| sources.layout().base_paths.get(*key))
                    .find_map(|root| input.path().strip_prefix(root).ok())
                    .context(
                        "bootstrap destination header is not in observed source include roots",
                    )?;
                (
                    vec![
                        scheme
                            .root(WheelScheme::Headers)
                            .context("bootstrap destination headers root is absent")?
                            .join(suffix),
                    ],
                    RuntimeBootstrapFileKind::Header,
                    input.mode(),
                )
            }
            RuntimeInputPurpose::SysconfigData => {
                bail!("bootstrap sysconfig data needs its directly observed input contract")
            }
        };
        for path in paths {
            add_planned_file(
                &mut output,
                &final_prefix,
                path,
                RuntimeBootstrapFile {
                    relative: PathBuf::new(),
                    sha256: *input.sha256(),
                    size: input.size(),
                    mode,
                    kind,
                },
                RuntimeBootstrapBytes::Input(index),
                limits,
                &mut total,
            )?;
        }
    }
    let cfg = render_pyvenv_cfg(
        original_parent,
        &sources.observed_runtime().language_version,
    )?;
    ensure!(
        cfg.len() <= limits.max_generated_bytes,
        "bootstrap limit: generated configuration exceeds budget"
    );
    let cfg_size = cfg.len() as u64;
    let cfg_sha = Sha256::digest(&cfg).into();
    add_planned_file(
        &mut output,
        &final_prefix,
        final_prefix.join("pyvenv.cfg"),
        RuntimeBootstrapFile {
            relative: PathBuf::new(),
            sha256: cfg_sha,
            size: cfg_size,
            mode: 0o644,
            kind: RuntimeBootstrapFileKind::Configuration,
        },
        RuntimeBootstrapBytes::Generated(cfg),
        limits,
        &mut total,
    )?;
    let mut directories = BTreeSet::new();
    for relative in output.keys() {
        let mut parent = relative.parent();
        while let Some(path) = parent {
            ensure!(
                !output.contains_key(path),
                "bootstrap destination has a file-directory conflict"
            );
            if !directories.contains(path) {
                ensure!(
                    directories.len() < limits.max_directories,
                    "bootstrap limit: directory count exceeded"
                );
                directories.insert(path.to_path_buf());
            }
            if path.as_os_str().is_empty() {
                break;
            }
            parent = path.parent();
        }
    }
    let mut reserved: Vec<_> = directories
        .into_iter()
        .map(|relative| WheelReservedDestination {
            path: final_prefix.join(relative),
            kind: WheelReservedKind::Directory,
        })
        .collect();
    let mut files = Vec::with_capacity(output.len());
    let mut writes = Vec::with_capacity(output.len());
    for (relative, (file, bytes)) in output {
        reserved.push(WheelReservedDestination {
            path: final_prefix.join(relative),
            kind: WheelReservedKind::File,
        });
        files.push(file);
        writes.push(bytes);
    }
    Ok(RuntimeBootstrapPlan {
        sources,
        final_prefix,
        allocation: prefix.allocation_id().into(),
        executable,
        files,
        scheme,
        reserved,
        writes,
    })
}

fn render_pyvenv_cfg(
    home: &Path,
    language: &crate::python_runtime::PythonVersion,
) -> Result<Vec<u8>> {
    let home_text = home
        .to_str()
        .context("bootstrap configuration home is not UTF-8")?;
    ensure!(
        home.is_absolute()
            && home_text.trim() == home_text
            && !home
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            && !home_text
                .chars()
                .any(|character| character.is_control()
                    || matches!(character, '\u{2028}' | '\u{2029}')),
        "bootstrap configuration home is relative or contains line injection"
    );
    ensure!(
        language.major == 3,
        "bootstrap configuration requires Python 3 language version"
    );
    Ok(format!(
        "home = {home_text}\ninclude-system-site-packages = false\nversion = {}\n",
        language.full_version()
    )
    .into_bytes())
}

fn relative_path(base: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(
        base.is_absolute() && path.is_absolute(),
        "bootstrap destination requires absolute source bindings"
    );
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = base
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in &base[common..] {
        relative.push("..");
    }
    for part in &path[common..] {
        relative.push(part.as_os_str());
    }
    Ok(relative)
}

fn contained_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::Normal(_) => {
                result.push(component.as_os_str())
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                ensure!(result.pop(), "bootstrap destination escapes its root")
            }
            _ => bail!("bootstrap destination has an unsupported component"),
        }
    }
    ensure!(
        result.starts_with(root) && result != root,
        "bootstrap destination escapes final prefix"
    );
    Ok(result)
}

fn add_planned_file(
    output: &mut BTreeMap<PathBuf, (RuntimeBootstrapFile, RuntimeBootstrapBytes)>,
    root: &Path,
    path: PathBuf,
    mut file: RuntimeBootstrapFile,
    bytes: RuntimeBootstrapBytes,
    limits: RuntimeBootstrapLimits,
    total: &mut u64,
) -> Result<()> {
    let path = contained_path(root, &path)?;
    ensure!(
        path.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
        "bootstrap limit: destination path exceeds budget"
    );
    ensure!(
        output.len() < limits.max_files,
        "bootstrap limit: file count exceeded"
    );
    *total = total
        .checked_add(file.size)
        .filter(|total| *total <= limits.max_total_bytes)
        .context("bootstrap limit: total bytes exceeded")?;
    let relative = path
        .strip_prefix(root)
        .context("bootstrap destination is outside final prefix")?
        .to_path_buf();
    ensure!(
        !output.contains_key(&relative),
        "bootstrap destination has duplicate runtime outputs"
    );
    file.relative = relative.clone();
    output.insert(relative, (file, bytes));
    Ok(())
}

pub(crate) async fn bootstrap_runtime(
    plan: RuntimeBootstrapPlan,
    prefix: &mut UnpublishedPrefix,
    timeout: Duration,
) -> Result<BootstrappedRuntime> {
    ensure!(
        !timeout.is_zero(),
        "bootstrap startup timeout must be positive"
    );
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .context("bootstrap startup timeout overflow")?;
    bootstrap_runtime_at_deadline(plan, prefix, deadline).await
}

pub(super) async fn bootstrap_runtime_at_deadline(
    plan: RuntimeBootstrapPlan,
    prefix: &mut UnpublishedPrefix,
    deadline: tokio::time::Instant,
) -> Result<BootstrappedRuntime> {
    ensure!(
        tokio::time::Instant::now() < deadline,
        "bootstrap startup deadline expired"
    );
    ensure!(
        plan.final_prefix == prefix.final_path() && plan.allocation == prefix.allocation_id(),
        "bootstrap allocation differs from its bound prefix"
    );
    prefix
        .check_binding()
        .context("bootstrap unpublished prefix is unavailable")?;
    ensure!(
        plan.files.len() == plan.writes.len(),
        "bootstrap output inventory is incomplete"
    );
    let result = copy_attempt(plan, prefix, deadline).await;
    if result.is_err() {
        prefix.mark_failed();
    }
    result
}

async fn copy_attempt(
    plan: RuntimeBootstrapPlan,
    prefix: &mut UnpublishedPrefix,
    deadline: tokio::time::Instant,
) -> Result<BootstrappedRuntime> {
    let RuntimeBootstrapPlan {
        mut sources,
        final_prefix,
        allocation,
        executable,
        files,
        scheme,
        reserved,
        writes,
    } = plan;
    let mut directories: Vec<_> = reserved
        .iter()
        .filter(|slot| slot.kind == WheelReservedKind::Directory)
        .map(|slot| slot.path.as_path())
        .collect();
    directories.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    for directory in directories {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "bootstrap startup deadline expired"
        );
        let relative = directory
            .strip_prefix(&final_prefix)
            .context("bootstrap write directory escaped final prefix")?;
        prefix
            .create_dir_all(relative)
            .context("bootstrap write directory failed")?;
    }
    let mut completed = Vec::with_capacity(files.len());
    for (file, bytes) in files.iter().zip(writes) {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "bootstrap startup deadline expired"
        );
        let output = prefix
            .create_file_exclusive(&file.relative, file.mode)
            .context("bootstrap write exclusive file failed")?;
        let mut writer = BootstrapWriter {
            output,
            sha256: Sha256::new(),
            size: 0,
            maximum: file.size,
        };
        match bytes {
            RuntimeBootstrapBytes::Input(index) => {
                let observed = sources
                    .copy_input(index, &mut writer)
                    .context("bootstrap write input snapshot failed")?;
                ensure!(
                    observed.sha256() == &file.sha256 && observed.size() == file.size,
                    "bootstrap write snapshot disagrees with output plan"
                );
            }
            RuntimeBootstrapBytes::Generated(bytes) => {
                for chunk in bytes.chunks(64 * 1024) {
                    writer
                        .write_all(chunk)
                        .context("bootstrap write generated configuration failed")?;
                }
            }
        }
        completed.push(writer.finish(file)?);
    }
    let handle = crate::python_runtime::PythonRuntimeHandle::from_existing_executable(&executable)
        .context("bootstrap startup executable binding failed")?;
    let observed =
        crate::python_runtime::probe_at_deadline(&handle, deadline, 1024 * 1024, 64 * 1024)
            .await
            .context("bootstrap startup fixed observation failed")?;
    let original = sources.observed_runtime();
    ensure!(
        observed.executable == executable
            && observed.implementation == original.implementation
            && observed.language_version == original.language_version
            && observed.implementation_version == original.implementation_version
            && observed.markers == original.markers
            && observed.soabi == original.soabi
            && observed.ext_suffix == original.ext_suffix
            && observed.abiflags == original.abiflags
            && observed.extension_suffixes == original.extension_suffixes
            && observed.gil_disabled == original.gil_disabled
            && observed.compatibility == original.compatibility,
        "bootstrap startup runtime identity changed"
    );
    prefix
        .check_binding()
        .context("bootstrap startup final prefix binding changed")?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "bootstrap startup deadline expired"
    );
    Ok(BootstrappedRuntime {
        final_prefix,
        allocation,
        executable,
        observed,
        files: completed,
        scheme,
        reserved,
        sources,
    })
}

struct BootstrapWriter<'a> {
    output: super::prefix::PrefixFile<'a>,
    sha256: Sha256,
    size: u64,
    maximum: u64,
}

impl std::io::Write for BootstrapWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        ensure_io_budget(bytes.len(), self.maximum.saturating_sub(self.size))?;
        let count = self.output.write(bytes)?;
        self.sha256.update(&bytes[..count]);
        self.size = self
            .size
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("bootstrap output byte count overflow"))?;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

fn ensure_io_budget(request: usize, remaining: u64) -> std::io::Result<()> {
    if request as u64 > remaining {
        return Err(std::io::Error::other(
            "bootstrap output exceeds frozen size",
        ));
    }
    Ok(())
}

impl BootstrapWriter<'_> {
    fn finish(self, expected: &RuntimeBootstrapFile) -> Result<RuntimeBootstrapFile> {
        let sha256: [u8; 32] = self.sha256.finalize().into();
        ensure!(
            sha256 == expected.sha256 && self.size == expected.size,
            "bootstrap write final byte statistics disagree with plan"
        );
        let observed = self
            .output
            .finish()
            .context("bootstrap write completion failed")?;
        ensure!(
            observed.relative_path() == expected.relative
                && observed.size() == expected.size
                && observed.mode() == expected.mode,
            "bootstrap write sealed receipt disagrees with plan"
        );
        Ok(RuntimeBootstrapFile {
            relative: observed.relative_path().into(),
            sha256,
            size: observed.size(),
            mode: observed.mode(),
            kind: expected.kind,
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
