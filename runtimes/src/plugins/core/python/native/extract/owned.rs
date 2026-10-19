//! Streaming native extraction through a live, unpublished language owner.
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use eyre::{Context, ContextCompat, Result, bail, ensure};
use tokio_util::sync::CancellationToken;

use super::super::pipeline::RuntimeInstallPlan;
use super::super::{RuntimeDistribution, entrypoint::runtime_entrypoint};
use crate::artifact_snapshot::VerifiedArtifact;
use crate::libraries::language::identity::LanguageKey;
use crate::libraries::language::repository::LanguageBuildOwner;
use crate::packages::python_environment::prefix::verify::{
    PrefixInventoryLimits, TreeInventory, TreeInventoryKind,
};
use crate::python_runtime::{self, PythonRuntimeDescriptor, PythonRuntimeHandle};

const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SYSCONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// Only the complete extraction consumer can construct this evidence.
#[derive(Debug)]
pub(crate) struct NativeExtractionReceipt {
    key: LanguageKey,
    artifact: pakro_store::Digest,
    inventory: TreeInventory,
    descriptor: PythonRuntimeDescriptor,
}
impl NativeExtractionReceipt {
    pub(crate) fn descriptor(&self) -> &PythonRuntimeDescriptor {
        &self.descriptor
    }
    pub(crate) fn inventory(&self) -> &TreeInventory {
        &self.inventory
    }
    pub(crate) fn validate_owner(&self, owner: &LanguageBuildOwner) -> Result<()> {
        owner.checkpoint()?;
        ensure!(
            self.key == owner.spec().key()
                && self.artifact == owner.spec().plan().source().digest(),
            "native extraction receipt belongs to another installation"
        );
        ensure!(
            owner.capture_inventory()? == self.inventory,
            "native extraction receipt inventory changed"
        );
        Ok(())
    }
}

struct ControlledRead<R> {
    input: R,
    deadline: Instant,
    cancellation: CancellationToken,
    remaining: u64,
    #[cfg(test)]
    hook: Option<TestReadHook>,
}
impl<R: Read> Read for ControlledRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::other("runtime extraction cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "runtime extraction timed out",
            ));
        }
        let limit = buffer
            .len()
            .min(self.remaining.saturating_add(1).min(usize::MAX as u64) as usize);
        let read = self.input.read(&mut buffer[..limit])?;
        #[cfg(test)]
        if read > 0
            && let Some(hook) = &self.hook
        {
            hook(read)?;
        }
        self.remaining = self
            .remaining
            .checked_sub(read as u64)
            .ok_or_else(|| io::Error::other("runtime archive exceeds decoded byte budget"))?;
        Ok(read)
    }
}

fn relative_member(path: &Path) -> Result<Option<PathBuf>> {
    let bytes = path.as_os_str().as_encoded_bytes();
    ensure!(
        !bytes.is_empty() && !path.is_absolute() && !bytes.contains(&0) && !bytes.contains(&b'\\'),
        "runtime archive path is unsafe"
    );
    // Check raw components before Path normalizes interior `.` or separators.
    ensure!(
        !bytes
            .split(|byte| *byte == b'/')
            .any(|part| matches!(part, b"." | b"..")),
        "runtime archive path traversal is forbidden"
    );
    let mut components = path.components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(_))),
        "runtime archive path has no bundle root"
    );
    let relative: PathBuf = components.collect();
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "runtime archive path traversal is forbidden"
    );
    Ok(Some(relative))
}

fn extract_stream(
    plan: &RuntimeInstallPlan,
    archive: &mut VerifiedArtifact,
    owner: &mut LanguageBuildOwner,
) -> Result<()> {
    ensure!(
        archive.size() <= MAX_ARCHIVE_BYTES,
        "runtime artifact exceeds archive byte budget"
    );
    archive.rewind()?;
    let decoder: Box<dyn Read + '_> = if plan.source().filename().ends_with(".tar.gz") {
        Box::new(flate2::read::GzDecoder::new(archive))
    } else if plan.source().filename().ends_with(".tar.bz2") {
        Box::new(bzip2::read::BzDecoder::new(archive))
    } else {
        bail!("unsupported native runtime archive format");
    };
    let mut decoded = ControlledRead {
        input: decoder,
        deadline: owner.deadline(),
        cancellation: owner.cancellation_token(),
        remaining: MAX_DECODED_BYTES,
        #[cfg(test)]
        hook: test_read_hooks()
            .lock()
            .unwrap()
            .get(owner.final_path())
            .cloned(),
    };
    {
        let mut archive = jdx_tar::Archive::new(&mut decoded);
        let mut entries = 0usize;
        for entry in archive.entries()? {
            owner.checkpoint()?;
            entries = entries
                .checked_add(1)
                .filter(|n| *n <= 1_048_576)
                .context("runtime archive entry budget exceeded")?;
            let mut entry = entry?;
            let Some(relative) = relative_member(&entry.path()?)? else {
                ensure!(
                    entry.entry_type() == jdx_tar::EntryType::Directory,
                    "runtime archive root is not a directory"
                );
                continue;
            };
            let mode = entry.header().mode();
            ensure!(
                mode & !0o777 == 0,
                "runtime archive contains privileged permission bits"
            );
            // Official archives may preserve the build machine's group-write
            // bits. The fixed extraction recipe only removes write authority;
            // privileged bits were rejected above, never silently stripped.
            let mode = mode & !0o022;
            owner.ensure_parents(&relative)?;
            match entry.entry_type() {
                jdx_tar::EntryType::Directory => owner.create_dir(&relative, mode)?,
                jdx_tar::EntryType::File => {
                    ensure!(
                        entry.size() <= PrefixInventoryLimits::default().max_file_bytes,
                        "runtime archive file exceeds byte budget"
                    );
                    let expected = entry.size();
                    let mut output = owner.create_file_exclusive(&relative, mode)?;
                    let actual = io::copy(&mut entry, &mut output)?;
                    ensure!(actual == expected, "runtime archive file is truncated");
                    output.finish()?;
                }
                jdx_tar::EntryType::Symlink => {
                    let target = entry
                        .header()
                        .link_name()
                        .context("runtime archive symlink has no target")?;
                    owner.create_symlink(&relative, &target)?;
                }
                _ => bail!("runtime archive contains an unsupported entry type"),
            }
        }
    }
    // Verify the compression trailer even if tar stopped at its end marker.
    io::copy(&mut decoded, &mut io::sink())?;
    owner.checkpoint()?;
    Ok(())
}

fn patch_owned_sysconfig(
    owner: &mut LanguageBuildOwner,
    descriptor: &PythonRuntimeDescriptor,
) -> Result<()> {
    let major = descriptor.language_version.major;
    let minor = descriptor.language_version.minor;
    let suffix = if descriptor.gil_disabled == Some(true) {
        "t"
    } else {
        ""
    };
    let inventory = owner.capture_inventory()?;
    let candidates = [
        format!("lib/python{major}.{minor}{suffix}"),
        format!("lib/python{major}.{minor}"),
        format!("lib/python{major}.{minor}t"),
    ];
    let directory = candidates
        .iter()
        .find(|candidate| {
            inventory.entries().iter().any(|entry| {
                entry.relative == Path::new(candidate)
                    && matches!(entry.kind, TreeInventoryKind::Directory)
            })
        })
        .context("Python sysconfig library directory is missing")?;
    let matching: Vec<_> = inventory
        .entries()
        .iter()
        .filter(|entry| {
            entry.relative.parent() == Some(Path::new(directory))
                && entry
                    .relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("_sysconfigdata_") && name.ends_with(".py")
                    })
        })
        .collect();
    ensure!(
        matching.len() == 1,
        "Python sysconfig file is missing or ambiguous"
    );
    ensure!(
        matches!(matching[0].kind, TreeInventoryKind::File { .. }),
        "Python sysconfig must be a regular owned file"
    );
    let path = matching[0].relative.clone();
    let original = owner.read_file(&path, MAX_SYSCONFIG_BYTES)?;
    let text = std::str::from_utf8(&original).context("Python sysconfig is not UTF-8")?;
    let patched = crate::sysconfig::patch_sysconfig_contents(text, owner.final_path())?;
    ensure!(
        patched.len() as u64 <= MAX_SYSCONFIG_BYTES,
        "patched Python sysconfig exceeds byte budget"
    );
    owner.replace_file(&path, &original, patched.as_bytes())?;
    Ok(())
}

pub(crate) async fn install_into_owner(
    plan: &RuntimeInstallPlan,
    archive: &mut VerifiedArtifact,
    owner: &mut LanguageBuildOwner,
    patch_sysconfig: bool,
) -> Result<NativeExtractionReceipt> {
    owner.checkpoint()?;
    ensure!(
        plan.language_key(patch_sysconfig)? == owner.spec().key(),
        "runtime plan differs from reserved installation"
    );
    owner.check_artifact(archive)?;
    ensure!(
        owner.capture_inventory()?.entries().is_empty(),
        "native extraction requires an empty owned payload"
    );
    extract_stream(plan, archive, owner).context("extracting verified Python runtime archive")?;
    owner.checkpoint()?;
    if std::fs::symlink_metadata(owner.final_path().join("bin/python"))
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        owner.create_symlink(Path::new("bin/python"), Path::new("python3"))?;
    }
    // Standard-library imports can traverse any link in the archive. Validate
    // the complete tree before executing even the fixed observation script.
    owner.capture_inventory()?;
    let executable = runtime_entrypoint(owner.final_path())
        .context("locating contained Python runtime entrypoint")?;
    let handle = PythonRuntimeHandle::from_existing_executable(&executable)?;
    let cancellation = owner.cancellation_token();
    let descriptor = tokio::select! {
        _=cancellation.cancelled()=>bail!("runtime extraction cancelled"),
        result=python_runtime::probe_at_deadline(&handle,tokio::time::Instant::from_std(owner.deadline()),1024*1024,64*1024)=>result.context("probing extracted Python runtime")?,
    };
    owner.checkpoint()?;
    plan.validate_observed(&descriptor)
        .context("validating selected Python runtime identity")?;
    if patch_sysconfig && plan.distribution() == RuntimeDistribution::Pbs {
        patch_owned_sysconfig(owner, &descriptor)
            .context("patching validated PBS Python sysconfig")?;
    }
    let inventory = owner.capture_inventory()?;
    owner.checkpoint()?;
    Ok(NativeExtractionReceipt {
        key: owner.spec().key(),
        artifact: archive.digest(),
        inventory,
        descriptor,
    })
}

#[cfg(test)]
type TestReadHook = std::sync::Arc<dyn Fn(usize) -> io::Result<()> + Send + Sync>;
#[cfg(test)]
fn test_read_hooks() -> &'static std::sync::Mutex<std::collections::BTreeMap<PathBuf, TestReadHook>>
{
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<PathBuf, TestReadHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
#[cfg(test)]
pub(crate) struct TestReadHookGuard(PathBuf);
#[cfg(test)]
impl Drop for TestReadHookGuard {
    fn drop(&mut self) {
        test_read_hooks().lock().unwrap().remove(&self.0);
    }
}
#[cfg(test)]
pub(crate) fn install_test_read_hook(path: PathBuf, hook: TestReadHook) -> TestReadHookGuard {
    assert!(
        test_read_hooks()
            .lock()
            .unwrap()
            .insert(path.clone(), hook)
            .is_none()
    );
    TestReadHookGuard(path)
}
