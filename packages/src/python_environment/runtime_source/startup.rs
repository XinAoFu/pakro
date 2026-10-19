//! Source/candidate-cache snapshots opened inside the existing source boundary.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};

use super::super::runtime_startup::{
    RuntimeStartupLimits, StartupAdapterBinding, StartupCacheBinding, StartupFilePin,
    StartupLoaderKind,
};
use super::BoundRuntimeSources;

#[derive(Debug)]
pub(in super::super) struct StartupSnapshots {
    pub(in super::super) source: Vec<u8>,
    pub(in super::super) cache: Option<Vec<u8>>,
    pub(in super::super) source_mtime: i64,
    checks: Vec<InputCheck>,
    missing: Option<PathBuf>,
}

#[derive(Debug)]
struct InputCheck {
    pin: StartupFilePin,
    canonical: PathBuf,
    device: u64,
    inode: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    _held: File,
}

impl StartupSnapshots {
    pub(in super::super) fn recheck(&self) -> Result<()> {
        for check in &self.checks {
            ensure!(
                check.pin.path.canonicalize()? == check.canonical,
                "startup input changed during observation"
            );
            let mut current = super::open_source_file(&check.canonical)
                .context("startup input changed during observation")?;
            let metadata = current.metadata()?;
            ensure!(
                metadata.dev() == check.device
                    && metadata.ino() == check.inode
                    && metadata.mtime() == check.mtime
                    && metadata.mtime_nsec() == check.mtime_nsec
                    && metadata.ctime() == check.ctime
                    && metadata.ctime_nsec() == check.ctime_nsec,
                "startup input changed during observation"
            );
            validate_metadata(&metadata, &check.pin)?;
            ensure!(
                super::hash_input(&mut current, check.pin.size)? == check.pin.sha256,
                "startup input changed during observation"
            );
        }
        if let Some(path) = &self.missing {
            ensure_missing(path)?;
        }
        Ok(())
    }
}

pub(in super::super) fn capture_startup_snapshots(
    runtime: &BoundRuntimeSources,
    binding: &StartupAdapterBinding,
    limits: RuntimeStartupLimits,
) -> Result<StartupSnapshots> {
    super::super::runtime_startup::validate_limits(limits)?;
    let source = &binding.source;
    ensure!(
        source.path.is_absolute()
            && source.path.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
        "startup input binding path is invalid"
    );
    let canonical = source
        .path
        .canonicalize()
        .context("startup input binding source cannot be resolved")?;
    let roots = ["stdlib", "platstdlib"]
        .into_iter()
        .filter_map(|key| runtime.layout.base_paths.get(key))
        .map(|root| root.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        roots
            .iter()
            .any(|root| canonical.parent() == Some(root.as_path()))
            && source.path.file_name() == Some(std::ffi::OsStr::new("site.py")),
        "startup input binding source differs from bound stdlib"
    );
    ensure!(
        source.size <= limits.max_source_bytes,
        "startup limit: source bytes exceeded"
    );
    let (bytes, check, mtime) = capture(source, limits.max_source_bytes)?;
    let mut output = StartupSnapshots {
        source: bytes,
        cache: None,
        source_mtime: mtime,
        checks: vec![check],
        missing: None,
    };
    match (&binding.loader, &binding.cache) {
        (StartupLoaderKind::Frozen, None) => {}
        (StartupLoaderKind::Source, Some(cache)) => {
            let path = match cache {
                StartupCacheBinding::Missing { path } => path,
                StartupCacheBinding::Pinned(pin) => &pin.path,
            };
            let expected_parent = source
                .path
                .parent()
                .context("startup input binding source parent missing")?
                .join("__pycache__");
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("startup input binding cache name invalid")?;
            ensure!(
                path.parent() == Some(expected_parent.as_path())
                    && name.starts_with("site.")
                    && name.ends_with(".pyc")
                    && name.len() <= 256
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'.' | b'_' | b'-'))
                    && path.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
                "startup input binding cache differs from source layout"
            );
            match expected_parent.canonicalize() {
                Ok(parent) => ensure!(
                    roots.iter().any(|root| parent.starts_with(root)),
                    "startup input binding cache parent escapes stdlib"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("startup input binding cache parent unavailable");
                }
            }
            match cache {
                StartupCacheBinding::Missing { path } => {
                    ensure_missing(path)?;
                    output.missing = Some(path.clone());
                }
                StartupCacheBinding::Pinned(pin) => {
                    ensure!(
                        pin.size <= limits.max_cache_bytes,
                        "startup limit: cache bytes exceeded"
                    );
                    let canonical = pin
                        .path
                        .canonicalize()
                        .context("startup input binding cache cannot be resolved")?;
                    ensure!(
                        roots.iter().any(|root| canonical.starts_with(root)),
                        "startup input binding cache escapes stdlib"
                    );
                    let (bytes, check, _) = capture(pin, limits.max_cache_bytes)?;
                    output.cache = Some(bytes);
                    output.checks.push(check);
                }
            }
        }
        _ => eyre::bail!("startup input binding loader/cache disagree"),
    }
    Ok(output)
}

fn validate_metadata(metadata: &std::fs::Metadata, pin: &StartupFilePin) -> Result<()> {
    ensure!(
        metadata.is_file()
            && (metadata.uid() == 0 || metadata.uid() == nix::unistd::geteuid().as_raw())
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o7000 == 0
            && metadata.mode() & 0o777 == pin.mode
            && metadata.len() == pin.size
            && pin.mode <= 0o777,
        "startup input binding size,mode,owner or type disagrees"
    );
    Ok(())
}

fn capture(pin: &StartupFilePin, maximum: u64) -> Result<(Vec<u8>, InputCheck, i64)> {
    ensure!(pin.size <= maximum, "startup limit: input bytes exceeded");
    let canonical = pin
        .path
        .canonicalize()
        .context("startup input binding path cannot be resolved")?;
    let mut file = super::open_source_file(&canonical)
        .context("startup input binding source cannot be opened")?;
    let before = file.metadata()?;
    validate_metadata(&before, pin)?;
    let capacity = usize::try_from(pin.size).context("startup limit: size is not representable")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .context("startup limit: snapshot allocation failed")?;
    file.by_ref()
        .take(pin.size.saturating_add(1))
        .read_to_end(&mut bytes)
        .context("startup input digest read failed")?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    ensure!(
        bytes.len() as u64 == pin.size && digest == pin.sha256,
        "startup input digest differs from pin"
    );
    let after = file.metadata()?;
    validate_metadata(&after, pin)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "startup input changed during capture"
    );
    let check = InputCheck {
        pin: pin.clone(),
        canonical,
        device: before.dev(),
        inode: before.ino(),
        mtime: before.mtime(),
        mtime_nsec: before.mtime_nsec(),
        ctime: before.ctime(),
        ctime_nsec: before.ctime_nsec(),
        _held: file,
    };
    Ok((bytes, check, before.mtime()))
}

fn ensure_missing(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => eyre::bail!("startup input binding cache expected missing"),
    }
    if let Some(parent) = path.parent() {
        match parent.canonicalize() {
            Ok(parent) => {
                super::open_source_directory(&parent)
                    .context("startup input binding cache parent is unsafe")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("startup input binding cache parent is unsafe");
            }
        }
    }
    Ok(())
}
