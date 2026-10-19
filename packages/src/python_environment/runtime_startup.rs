//! Actual site-code admission and separate normal empty-environment facts.
//!
//! Compilable wrong baselines await independently observed stage RED. Public
//! input values never construct a cfg/normal evidence or publication capability.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) mod installed;

use base64::{Engine as _, prelude::BASE64_STANDARD};
use eyre::{Context, ContextCompat, Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

use super::super::python_wheel_plan::WheelInstallScheme;
use super::super::python_wheel_plan::WheelReservedDestination;
use super::super::python_wheel_plan::WheelReservedKind;
use super::prefix::UnpublishedPrefix;
use super::prefix::verify::{
    PrefixExpectedDirectory, PrefixExpectedFile, PrefixInventoryLimits, verify_prefix_inventory,
};
use super::runtime_bootstrap::BootstrappedRuntime;

mod json_budget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupFilePin {
    pub path: PathBuf,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupCacheBinding {
    Missing { path: PathBuf },
    Pinned(StartupFilePin),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartupLoaderKind {
    Source,
    Frozen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupAdapterBinding {
    pub source: StartupFilePin,
    pub cache: Option<StartupCacheBinding>,
    pub loader: StartupLoaderKind,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeStartupLimits {
    pub max_source_bytes: u64,
    pub max_cache_bytes: u64,
    pub max_request_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_paths: usize,
    pub max_path_bytes: usize,
    pub max_code_nodes: usize,
    pub max_code_depth: usize,
    pub max_code_bytes: usize,
    pub max_literal_bytes: usize,
}

impl Default for RuntimeStartupLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 8 * 1024 * 1024,
            max_cache_bytes: 8 * 1024 * 1024,
            max_request_bytes: 32 * 1024 * 1024,
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_paths: 4096,
            max_path_bytes: 16_384,
            max_code_nodes: 65_536,
            max_code_depth: 64,
            max_code_bytes: 8 * 1024 * 1024,
            max_literal_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartupTranscript {
    schema: u32,
    observation_mode: String,
    executable: PathBuf,
    prefix: PathBuf,
    exec_prefix: PathBuf,
    base_prefix: PathBuf,
    base_exec_prefix: PathBuf,
    loader: StartupLoaderKind,
    site_origin: String,
    cache_path: Option<PathBuf>,
    source_sha256: String,
    cache_sha256: Option<String>,
    code_selection: String,
    cfg_path: PathBuf,
    home: PathBuf,
    user_site_enabled: bool,
    no_site: bool,
    ignore_environment: bool,
    dont_write_bytecode: bool,
    requested_sites: Vec<PathBuf>,
    suppressed_hooks: Vec<String>,
    sys_path: Vec<PathBuf>,
    default_paths: BTreeMap<String, PathBuf>,
    final_paths: BTreeMap<String, PathBuf>,
}

pub(crate) struct CfgRecognitionEvidence {
    allocation: String,
    final_prefix: PathBuf,
    executable: PathBuf,
    source_binding: StartupAdapterBinding,
    transcript: StartupTranscript,
    // Owned immutable bytes are retained through normal verification, never
    // reopened from a cache pathname and never exposed as a caller reader.
    snapshots: super::runtime_source::startup::StartupSnapshots,
}

impl std::fmt::Debug for CfgRecognitionEvidence {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("CfgRecognitionEvidence")
            .field("allocation", &self.allocation)
            .field("loader", &self.source_binding.loader)
            .finish_non_exhaustive()
    }
}

impl CfgRecognitionEvidence {
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn requested_sites(&self) -> &[PathBuf] {
        &self.transcript.requested_sites
    }
}

#[derive(Debug)]
pub(crate) struct EmptyEnvironmentLayout {
    runtime: BootstrappedRuntime,
    recognition: CfgRecognitionEvidence,
    normal: StartupTranscript,
    actual_scheme: WheelInstallScheme,
    site_disabled: super::runtime_layout::RuntimeLayoutObservation,
    normal_directories: Vec<PathBuf>,
    reserved: Vec<WheelReservedDestination>,
}

impl EmptyEnvironmentLayout {
    pub(crate) fn final_prefix(&self) -> &Path {
        self.runtime.final_prefix()
    }
    pub(crate) fn allocation_id(&self) -> &str {
        self.runtime.allocation_id()
    }
    pub(crate) fn executable(&self) -> &Path {
        self.runtime.executable()
    }
    pub(crate) fn actual_scheme(&self) -> &WheelInstallScheme {
        &self.actual_scheme
    }
    pub(crate) fn reserved(&self) -> &[WheelReservedDestination] {
        &self.reserved
    }
}

pub(crate) async fn establish_cfg_recognition(
    runtime: &BootstrappedRuntime,
    prefix: &UnpublishedPrefix,
    binding: StartupAdapterBinding,
    limits: RuntimeStartupLimits,
    timeout: Duration,
) -> Result<CfgRecognitionEvidence> {
    validate_limits(limits)?;
    ensure!(
        !timeout.is_zero(),
        "startup limit: timeout must be positive"
    );
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .context("startup limit: deadline overflow")?;
    recognize_at_deadline(runtime, prefix, binding, limits, deadline).await
}

pub(super) async fn recognize_at_deadline(
    runtime: &BootstrappedRuntime,
    prefix: &UnpublishedPrefix,
    binding: StartupAdapterBinding,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
) -> Result<CfgRecognitionEvidence> {
    ensure!(
        runtime.final_prefix() == prefix.final_path()
            && runtime.allocation_id() == prefix.allocation_id(),
        "startup allocation differs from bound runtime"
    );
    prefix
        .check_binding()
        .context("startup allocation is unavailable")?;
    preflight_request(&binding, limits)?;
    verify_bootstrap(runtime, prefix)?;
    let snapshots = super::runtime_source::startup::capture_startup_snapshots(
        runtime.source_binding(),
        &binding,
        limits,
    )?;
    let request = encode_request(
        runtime,
        &binding,
        &snapshots,
        limits,
        "cfg_recognition_intercepted",
    )?;
    let handle =
        crate::python_runtime::PythonRuntimeHandle::from_existing_executable(runtime.executable())?;
    let raw = crate::python_runtime::probe_startup_intercept(
        &handle,
        request,
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("startup fixed observation failed")?;
    let transcript = decode_transcript(&raw, limits)?;
    validate_recognition(runtime, &binding, &transcript, limits)?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "startup limit: deadline expired"
    );
    snapshots.recheck()?;
    verify_bootstrap(runtime, prefix)?;
    Ok(CfgRecognitionEvidence {
        allocation: runtime.allocation_id().into(),
        final_prefix: runtime.final_prefix().into(),
        executable: runtime.executable().into(),
        source_binding: binding,
        transcript,
        snapshots,
    })
}

pub(super) fn validate_limits(limits: RuntimeStartupLimits) -> Result<()> {
    ensure!(
        limits.max_source_bytes != 0
            && limits.max_cache_bytes != 0
            && limits.max_request_bytes != 0
            && limits.max_request_bytes <= 32 * 1024 * 1024
            && limits.max_stdout_bytes != 0
            && limits.max_stderr_bytes != 0
            && limits.max_paths != 0
            && limits.max_path_bytes != 0
            && limits.max_code_nodes != 0
            && limits.max_code_depth != 0
            && limits.max_code_bytes != 0
            && limits.max_literal_bytes != 0,
        "startup limit values are invalid"
    );
    Ok(())
}

fn preflight_request(binding: &StartupAdapterBinding, limits: RuntimeStartupLimits) -> Result<()> {
    let cache = match &binding.cache {
        Some(StartupCacheBinding::Pinned(pin)) => Some(pin),
        _ => None,
    };
    ensure!(
        binding.source.size <= limits.max_source_bytes
            && cache.is_none_or(|pin| pin.size <= limits.max_cache_bytes),
        "startup limit: snapshot bytes exceeded"
    );
    let cache_path = match &binding.cache {
        Some(StartupCacheBinding::Pinned(pin)) => Some(&pin.path),
        Some(StartupCacheBinding::Missing { path }) => Some(path),
        None => None,
    };
    for path in std::iter::once(&binding.source.path).chain(cache_path) {
        ensure!(
            path.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
            "startup limit: input path exceeded"
        );
    }
    let bytes = binding
        .source
        .size
        .checked_add(cache.map_or(0, |pin| pin.size))
        .and_then(|bytes| bytes.checked_add(4))
        .and_then(|bytes| bytes.checked_mul(4))
        .map(|bytes| bytes / 3)
        .context("startup limit: request size overflow")?;
    ensure!(
        bytes <= limits.max_request_bytes as u64,
        "startup limit: encoded request exceeded"
    );
    Ok(())
}

fn verify_bootstrap(runtime: &BootstrappedRuntime, prefix: &UnpublishedPrefix) -> Result<()> {
    verify_runtime_inventory(runtime, prefix, &[])
}

fn verify_runtime_inventory(
    runtime: &BootstrappedRuntime,
    prefix: &UnpublishedPrefix,
    extra_dirs: &[PathBuf],
) -> Result<()> {
    let files = runtime
        .files()
        .iter()
        .map(|file| PrefixExpectedFile {
            relative: file.relative_path().into(),
            sha256: *file.sha256(),
            size: file.size(),
            mode: file.mode(),
        })
        .collect::<Vec<_>>();
    let mut directories = runtime
        .reserved()
        .iter()
        .filter(|entry| {
            entry.kind == WheelReservedKind::Directory && entry.path != runtime.final_prefix()
        })
        .map(|entry| {
            Ok(PrefixExpectedDirectory {
                relative: entry.path.strip_prefix(runtime.final_prefix())?.into(),
                mode: 0o755,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    for relative in extra_dirs {
        if !directories.iter().any(|entry| entry.relative == *relative) {
            directories.push(PrefixExpectedDirectory {
                relative: relative.clone(),
                mode: 0o755,
            });
        }
    }
    verify_prefix_inventory(
        prefix,
        &files,
        &directories,
        PrefixInventoryLimits::default(),
    )
    .context("startup bootstrap inventory changed")?;
    Ok(())
}

#[derive(Serialize)]
struct SourceRequest<'a> {
    path: &'a Path,
    sha256: String,
    size: u64,
    mode: u32,
    mtime: i64,
    bytes: &'a str,
}
#[derive(Serialize)]
struct CacheRequest<'a> {
    state: &'static str,
    path: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<&'a str>,
}
#[derive(Serialize)]
struct CodeLimits {
    max_nodes: usize,
    max_depth: usize,
    max_code_bytes: usize,
    max_literal_bytes: usize,
    max_source_bytes: u64,
    max_cache_bytes: u64,
    max_path_bytes: usize,
}
#[derive(Serialize)]
struct StartupRequest<'a> {
    schema: u32,
    mode: &'static str,
    final_prefix: &'a Path,
    source: SourceRequest<'a>,
    cache: Option<CacheRequest<'a>>,
    loader: StartupLoaderKind,
    cfg_sha256: String,
    max_paths: usize,
    code_limits: CodeLimits,
}
struct RequestWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for RequestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        ensure_request_capacity(self.bytes.len(), bytes.len(), self.maximum)?;
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("startup request allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn ensure_request_capacity(current: usize, count: usize, maximum: usize) -> io::Result<()> {
    if current
        .checked_add(count)
        .is_none_or(|total| total > maximum)
    {
        Err(io::Error::other("startup input limit exceeded"))
    } else {
        Ok(())
    }
}
fn encode_request(
    runtime: &BootstrappedRuntime,
    binding: &StartupAdapterBinding,
    snapshots: &super::runtime_source::startup::StartupSnapshots,
    limits: RuntimeStartupLimits,
    mode: &'static str,
) -> Result<Vec<u8>> {
    let source_text = BASE64_STANDARD.encode(&snapshots.source);
    let cache_text = snapshots
        .cache
        .as_ref()
        .map(|bytes| BASE64_STANDARD.encode(bytes));
    let cache = match &binding.cache {
        Some(StartupCacheBinding::Missing { path }) => Some(CacheRequest {
            state: "missing",
            path,
            sha256: None,
            size: None,
            bytes: None,
        }),
        Some(StartupCacheBinding::Pinned(pin)) => Some(CacheRequest {
            state: "pinned",
            path: &pin.path,
            sha256: Some(hex::encode(pin.sha256)),
            size: Some(pin.size),
            bytes: cache_text.as_deref(),
        }),
        None => None,
    };
    let cfg = runtime
        .files()
        .iter()
        .find(|file| file.relative_path() == Path::new("pyvenv.cfg"))
        .context("startup bootstrap inventory has no cfg")?;
    let request = StartupRequest {
        schema: 1,
        mode,
        final_prefix: runtime.final_prefix(),
        source: SourceRequest {
            path: &binding.source.path,
            sha256: hex::encode(binding.source.sha256),
            size: binding.source.size,
            mode: binding.source.mode,
            mtime: snapshots.source_mtime,
            bytes: &source_text,
        },
        cache,
        loader: binding.loader,
        cfg_sha256: hex::encode(cfg.sha256()),
        max_paths: limits.max_paths,
        code_limits: CodeLimits {
            max_nodes: limits.max_code_nodes,
            max_depth: limits.max_code_depth,
            max_code_bytes: limits.max_code_bytes,
            max_literal_bytes: limits.max_literal_bytes,
            max_source_bytes: limits.max_source_bytes,
            max_cache_bytes: limits.max_cache_bytes,
            max_path_bytes: limits.max_path_bytes,
        },
    };
    let mut writer = RequestWriter {
        bytes: Vec::new(),
        maximum: limits.max_request_bytes,
    };
    serde_json::to_writer(&mut writer, &request)
        .context("startup limit: request encoding failed")?;
    Ok(writer.bytes)
}

fn decode_transcript(raw: &[u8], limits: RuntimeStartupLimits) -> Result<StartupTranscript> {
    decode_mode(raw, limits, "cfg_recognition_intercepted")
}

fn decode_mode(
    raw: &[u8],
    limits: RuntimeStartupLimits,
    mode: &'static str,
) -> Result<StartupTranscript> {
    ensure!(
        raw.len() <= limits.max_stdout_bytes,
        "startup limit: output bytes exceeded"
    );
    json_budget::check(raw, limits)?;
    let transcript: StartupTranscript =
        serde_json::from_slice(raw).context("startup protocol JSON or fields are invalid")?;
    ensure!(
        transcript.schema == 1 && transcript.observation_mode == mode,
        "startup protocol schema or mode disagrees"
    );
    for path in [
        &transcript.executable,
        &transcript.prefix,
        &transcript.exec_prefix,
        &transcript.base_prefix,
        &transcript.base_exec_prefix,
        &transcript.cfg_path,
        &transcript.home,
    ]
    .into_iter()
    .chain(transcript.cache_path.iter())
    .chain(transcript.requested_sites.iter())
    .chain(transcript.sys_path.iter())
    .chain(transcript.default_paths.values())
    .chain(transcript.final_paths.values())
    {
        ensure!(
            path.is_absolute()
                && !path.as_os_str().as_encoded_bytes().contains(&0)
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "startup protocol path is invalid"
        );
    }
    ensure!(
        transcript.default_paths.len() <= 8
            && transcript.default_paths.keys().all(|key| matches!(
                key.as_str(),
                "stdlib"
                    | "platstdlib"
                    | "purelib"
                    | "platlib"
                    | "include"
                    | "platinclude"
                    | "scripts"
                    | "data"
            ))
            && transcript.final_paths.len() == 5
            && transcript.final_paths.keys().all(|key| matches!(
                key.as_str(),
                "purelib" | "platlib" | "headers" | "scripts" | "data"
            )),
        "startup protocol scheme fields are invalid"
    );
    Ok(transcript)
}

fn validate_recognition(
    runtime: &BootstrappedRuntime,
    binding: &StartupAdapterBinding,
    facts: &StartupTranscript,
    _limits: RuntimeStartupLimits,
) -> Result<()> {
    let expected = runtime.source_binding().layout();
    let expected_cache = match &binding.cache {
        Some(StartupCacheBinding::Missing { path }) => Some(path),
        Some(StartupCacheBinding::Pinned(pin)) => Some(&pin.path),
        None => None,
    };
    let expected_hash = match &binding.cache {
        Some(StartupCacheBinding::Pinned(pin)) => Some(hex::encode(pin.sha256)),
        _ => None,
    };
    ensure!(
        facts.executable == runtime.executable()
            && facts.base_prefix
                == *expected
                    .base_prefix
                    .as_ref()
                    .context("cfg recognition base prefix absent")?
            && facts.base_exec_prefix
                == *expected
                    .base_exec_prefix
                    .as_ref()
                    .context("cfg recognition base exec prefix absent")?
            && facts.loader == binding.loader
            && facts.source_sha256 == hex::encode(binding.source.sha256)
            && facts.cache_sha256 == expected_hash
            && facts.cache_path.as_ref() == expected_cache
            && match binding.loader {
                StartupLoaderKind::Frozen =>
                    facts.site_origin == "frozen" && facts.code_selection == "frozen",
                StartupLoaderKind::Source =>
                    Path::new(&facts.site_origin) == binding.source.path
                        && matches!(facts.code_selection.as_str(), "source" | "cache"),
            },
        "startup observation binding disagrees"
    );
    ensure!(
        facts.prefix == runtime.final_prefix()
            && facts.exec_prefix == runtime.final_prefix()
            && facts.cfg_path == runtime.final_prefix().join("pyvenv.cfg")
            && facts.home
                == runtime
                    .source_binding()
                    .observed_runtime()
                    .executable
                    .parent()
                    .context("cfg recognition source executable parent absent")?
            && !facts.user_site_enabled
            && facts.no_site
            && facts.ignore_environment
            && facts.dont_write_bytecode,
        "cfg recognition did not establish an isolated final prefix"
    );
    const HOOKS: [&str; 6] = [
        "addsitepackages",
        "addsitedir",
        "addusersitepackages",
        "execsitecustomize",
        "execusercustomize",
        "enablerlcompleter",
    ];
    let actual = facts
        .suppressed_hooks
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        facts.suppressed_hooks.len() == HOOKS.len()
            && actual == HOOKS.into_iter().collect()
            && !facts.requested_sites.is_empty()
            && facts
                .requested_sites
                .iter()
                .all(|path| path.starts_with(runtime.final_prefix())
                    && path != runtime.final_prefix()
                    && ["purelib", "platlib"]
                        .iter()
                        .any(|key| facts.final_paths.get(*key) == Some(path))),
        "startup hook boundary did not exclude base/user paths"
    );
    ensure!(
        facts.final_paths == expected.target_paths
            && facts
                .sys_path
                .iter()
                .all(|path| expected.sys_path.contains(path)),
        "startup observation binding scheme or search paths disagree"
    );
    Ok(())
}

pub(crate) async fn verify_empty_environment_layout(
    runtime: BootstrappedRuntime,
    prefix: &mut UnpublishedPrefix,
    evidence: CfgRecognitionEvidence,
    limits: RuntimeStartupLimits,
    timeout: Duration,
) -> Result<EmptyEnvironmentLayout> {
    validate_limits(limits)?;
    ensure!(
        !timeout.is_zero(),
        "startup limit: normal timeout must be positive"
    );
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .context("startup limit: normal deadline overflow")?;
    verify_empty_environment_layout_at_deadline(runtime, prefix, evidence, limits, deadline).await
}

pub(super) async fn verify_empty_environment_layout_at_deadline(
    runtime: BootstrappedRuntime,
    prefix: &mut UnpublishedPrefix,
    evidence: CfgRecognitionEvidence,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
) -> Result<EmptyEnvironmentLayout> {
    validate_limits(limits)?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "normal startup deadline expired"
    );
    ensure!(
        runtime.final_prefix() == prefix.final_path()
            && runtime.allocation_id() == prefix.allocation_id()
            && evidence.final_prefix == runtime.final_prefix()
            && evidence.allocation == runtime.allocation_id()
            && evidence.executable == runtime.executable(),
        "normal startup allocation differs from recognition"
    );
    prefix
        .check_binding()
        .context("normal startup allocation is unavailable")?;
    let result = normal_attempt(runtime, prefix, evidence, limits, deadline).await;
    if result.is_err() {
        prefix.mark_failed();
    }
    result
}

async fn normal_attempt(
    runtime: BootstrappedRuntime,
    prefix: &mut UnpublishedPrefix,
    evidence: CfgRecognitionEvidence,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
) -> Result<EmptyEnvironmentLayout> {
    normal_attempt_with_finish_hook(runtime, prefix, evidence, limits, deadline, || {}).await
}

async fn normal_attempt_with_finish_hook(
    runtime: BootstrappedRuntime,
    prefix: &mut UnpublishedPrefix,
    evidence: CfgRecognitionEvidence,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
    before_return: impl FnOnce(),
) -> Result<EmptyEnvironmentLayout> {
    evidence.snapshots.recheck()?;
    let fresh = recognize_at_deadline(
        &runtime,
        prefix,
        evidence.source_binding.clone(),
        limits,
        deadline,
    )
    .await
    .context("normal startup site is not empty or recognition changed")?;
    ensure!(
        fresh.source_binding == evidence.source_binding
            && fresh.snapshots.source == evidence.snapshots.source
            && fresh.snapshots.cache == evidence.snapshots.cache
            && fresh.transcript.requested_sites == evidence.transcript.requested_sites,
        "normal startup recognition inputs changed"
    );
    let mut directories = std::collections::BTreeSet::new();
    for site in fresh.requested_sites() {
        let mut path = site
            .strip_prefix(runtime.final_prefix())
            .context("normal startup site escaped prefix")?;
        while !path.as_os_str().is_empty() {
            directories.insert(path.to_path_buf());
            path = path
                .parent()
                .context("normal startup site parent invalid")?;
        }
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    for directory in &directories {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "normal startup deadline expired"
        );
        prefix
            .create_dir_all(directory)
            .context("normal startup site is not empty or owned")?;
    }
    verify_runtime_inventory(&runtime, prefix, &directories)
        .context("normal startup site is not empty or runtime changed")?;
    fresh.snapshots.recheck()?;
    let request = encode_request(
        &runtime,
        &fresh.source_binding,
        &fresh.snapshots,
        limits,
        "normal_empty_environment",
    )?;
    let handle =
        crate::python_runtime::PythonRuntimeHandle::from_existing_executable(runtime.executable())?;
    let raw = crate::python_runtime::probe_empty_environment(
        &handle,
        request,
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("normal startup observation failed")?;
    let normal = decode_mode(&raw, limits, "normal_empty_environment")
        .context("normal startup observation protocol failed")?;
    validate_normal(&runtime, &fresh, &normal).context("normal startup observation disagrees")?;
    fresh.snapshots.recheck()?;
    verify_runtime_inventory(&runtime, prefix, &directories)?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "normal startup deadline expired"
    );
    let actual_scheme = WheelInstallScheme::new(
        runtime.final_prefix().into(),
        [
            (super::super::python_wheel::WheelScheme::Purelib, "purelib"),
            (super::super::python_wheel::WheelScheme::Platlib, "platlib"),
            (super::super::python_wheel::WheelScheme::Headers, "headers"),
            (super::super::python_wheel::WheelScheme::Scripts, "scripts"),
            (super::super::python_wheel::WheelScheme::Data, "data"),
        ]
        .into_iter()
        .map(|(kind, key)| (kind, normal.final_paths[key].clone()))
        .collect(),
    )?;
    let site_disabled = observe_empty_site_disabled(&runtime, limits, deadline).await?;
    fresh.snapshots.recheck()?;
    verify_runtime_inventory(&runtime, prefix, &directories)?;
    let mut reserved = runtime.reserved().to_vec();
    for relative in &directories {
        let path = runtime.final_prefix().join(relative);
        if !reserved.iter().any(|entry| entry.path == path) {
            reserved.push(WheelReservedDestination {
                path,
                kind: WheelReservedKind::Directory,
            });
        }
    }
    before_return();
    ensure!(
        tokio::time::Instant::now() < deadline,
        "normal startup deadline expired"
    );
    Ok(EmptyEnvironmentLayout {
        runtime,
        recognition: fresh,
        normal,
        actual_scheme,
        site_disabled,
        normal_directories: directories,
        reserved,
    })
}

async fn observe_empty_site_disabled(
    runtime: &BootstrappedRuntime,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
) -> Result<super::runtime_layout::RuntimeLayoutObservation> {
    let handle =
        crate::python_runtime::PythonRuntimeHandle::from_existing_executable(runtime.executable())?;
    let observed = crate::python_runtime::probe_at_deadline(
        &handle,
        deadline,
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
    )
    .await
    .context("normal startup site-disabled runtime observation failed")?;
    ensure!(
        observed == *runtime.observed_site_disabled(),
        "normal startup site-disabled runtime changed"
    );
    let raw = crate::python_runtime::probe_layout(
        &handle,
        runtime.final_prefix(),
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("normal startup site-disabled layout observation failed")?;
    json_budget::check(&raw, limits)?;
    super::runtime_layout::decode_runtime_layout(
        &raw,
        &observed,
        runtime.final_prefix(),
        super::runtime_layout::RuntimeLayoutLimits {
            max_stdout_bytes: limits.max_stdout_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
            max_paths: limits.max_paths,
            max_path_bytes: limits.max_path_bytes,
            ..Default::default()
        },
    )
}

fn validate_normal(
    runtime: &BootstrappedRuntime,
    evidence: &CfgRecognitionEvidence,
    facts: &StartupTranscript,
) -> Result<()> {
    let prior = &evidence.transcript;
    ensure!(
        !facts.no_site
            && facts.ignore_environment
            && facts.dont_write_bytecode
            && !facts.user_site_enabled
            && facts.suppressed_hooks.is_empty()
            && facts.executable == prior.executable
            && facts.prefix == runtime.final_prefix()
            && facts.exec_prefix == runtime.final_prefix()
            && facts.base_prefix == prior.base_prefix
            && facts.base_exec_prefix == prior.base_exec_prefix
            && facts.loader == prior.loader
            && facts.site_origin == prior.site_origin
            && facts.cache_path == prior.cache_path
            && facts.source_sha256 == prior.source_sha256
            && facts.cache_sha256 == prior.cache_sha256
            && facts.cfg_path == prior.cfg_path
            && facts.home == prior.home,
        "normal startup prefix,source or flags changed"
    );
    ensure!(
        matches!(facts.code_selection.as_str(), "source" | "cache" | "frozen")
            && facts.code_selection == prior.code_selection,
        "normal startup code selection changed"
    );
    let sites = facts
        .requested_sites
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let expected = prior
        .requested_sites
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        sites == expected
            && !sites.is_empty()
            && ["purelib", "platlib"]
                .into_iter()
                .all(|key| sites.contains(&facts.final_paths[key]))
            && facts
                .final_paths
                .values()
                .all(|path| path.starts_with(runtime.final_prefix())),
        "normal startup site or scheme changed"
    );
    let actual_paths = facts
        .sys_path
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let expected_paths = prior
        .sys_path
        .iter()
        .chain(facts.requested_sites.iter())
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        actual_paths == expected_paths,
        "normal startup search paths changed"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
