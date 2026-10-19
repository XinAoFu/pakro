//! Local installed-prefix facts; this is never a seal or publication authority.
//!
//! Verify complete installed bytes before and after site-disabled observation.

use eyre::{Context, ContextCompat, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::prefix::UnpublishedPrefix;
use super::{EmptyEnvironmentLayout, RuntimeStartupLimits};
use crate::packages::python_wheel_install::{
    MaterializedWheelDirectory, MaterializedWheelFile, PreparedWheelSource,
    WheelMaterializationReceipt,
};
use crate::python_runtime::PythonRuntimeDescriptor;

pub(crate) struct EnvironmentVerificationReceipt {
    allocation: String,
    final_prefix: PathBuf,
    sources: Vec<PreparedWheelSource>,
    files: Vec<MaterializedWheelFile>,
    directories: Vec<MaterializedWheelDirectory>,
    inventory_sha256: [u8; 32],
    observed: PythonRuntimeDescriptor,
    complete_files: Vec<super::PrefixExpectedFile>,
    complete_directories: Vec<super::PrefixExpectedDirectory>,
}
impl EnvironmentVerificationReceipt {
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn sources(&self) -> &[PreparedWheelSource] {
        &self.sources
    }
    pub(crate) fn files(&self) -> &[MaterializedWheelFile] {
        &self.files
    }
    pub(crate) fn directories(&self) -> &[MaterializedWheelDirectory] {
        &self.directories
    }
    pub(crate) fn inventory_sha256(&self) -> &[u8; 32] {
        &self.inventory_sha256
    }
    pub(crate) fn observed_runtime(&self) -> &PythonRuntimeDescriptor {
        &self.observed
    }
    pub(crate) fn complete_files(&self) -> &[super::PrefixExpectedFile] {
        &self.complete_files
    }
    pub(crate) fn complete_directories(&self) -> &[super::PrefixExpectedDirectory] {
        &self.complete_directories
    }
}
impl std::fmt::Debug for EnvironmentVerificationReceipt {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("EnvironmentVerificationReceipt")
            .field("allocation", &self.allocation)
            .field("source_count", &self.sources.len())
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}

pub(crate) async fn verify_materialized_environment(
    layout: &EmptyEnvironmentLayout,
    wheels: &WheelMaterializationReceipt,
    prefix: &UnpublishedPrefix,
    limits: RuntimeStartupLimits,
    timeout: Duration,
) -> Result<EnvironmentVerificationReceipt> {
    super::validate_limits(limits).context("installed limit values are invalid")?;
    ensure!(
        !timeout.is_zero(),
        "installed limit: timeout must be positive"
    );
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .context("installed limit: deadline overflow")?;
    verify_materialized_environment_at_deadline(layout, wheels, prefix, limits, deadline).await
}

pub(in super::super) async fn verify_materialized_environment_at_deadline(
    layout: &EmptyEnvironmentLayout,
    wheels: &WheelMaterializationReceipt,
    prefix: &UnpublishedPrefix,
    limits: RuntimeStartupLimits,
    deadline: tokio::time::Instant,
) -> Result<EnvironmentVerificationReceipt> {
    super::validate_limits(limits).context("installed limit values are invalid")?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "installed startup deadline expired"
    );
    ensure!(
        layout.allocation_id() == prefix.allocation_id()
            && wheels.allocation_id() == prefix.allocation_id()
            && layout.final_prefix() == prefix.final_path()
            && wheels.final_prefix() == prefix.final_path(),
        "installed allocation differs from the runtime or wheel receipt"
    );
    let inventory_limits = super::PrefixInventoryLimits {
        max_path_bytes: limits.max_path_bytes,
        ..Default::default()
    };
    preflight(layout, wheels, limits, inventory_limits)?;
    let (files, directories) = expected_inventory(layout, wheels)?;
    super::verify_prefix_inventory(prefix, &files, &directories, inventory_limits)
        .context("installed inventory disagrees with the complete runtime/wheel receipts")?;
    protect_startup(layout, wheels)?;
    layout
        .recognition
        .snapshots
        .recheck()
        .context("installed startup source changed")?;
    let handle =
        crate::python_runtime::PythonRuntimeHandle::from_existing_executable(layout.executable())?;
    let observed = crate::python_runtime::probe_at_deadline(
        &handle,
        deadline,
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
    )
    .await
    .context("installed startup fixed runtime probe failed")?;
    ensure!(
        observed == *layout.runtime.observed_site_disabled(),
        "installed startup runtime identity or ABI changed"
    );
    let raw = crate::python_runtime::probe_layout(
        &handle,
        prefix.final_path(),
        limits.max_stdout_bytes as u64,
        limits.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("installed startup fixed layout probe failed")?;
    super::json_budget::check(&raw, limits).context("installed startup layout budget exceeded")?;
    let actual = super::super::runtime_layout::decode_runtime_layout(
        &raw,
        &observed,
        prefix.final_path(),
        super::super::runtime_layout::RuntimeLayoutLimits {
            max_stdout_bytes: limits.max_stdout_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
            max_paths: limits.max_paths,
            max_path_bytes: limits.max_path_bytes,
            ..Default::default()
        },
    )?;
    // The private D3 baseline was collected before any package bytes existed.
    ensure!(
        actual == layout.site_disabled,
        "installed startup layout changed"
    );
    layout
        .recognition
        .snapshots
        .recheck()
        .context("installed startup source changed")?;
    let inventory = super::verify_prefix_inventory(prefix, &files, &directories, inventory_limits)
        .context("installed inventory changed during observation")?;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "installed startup deadline expired"
    );
    Ok(EnvironmentVerificationReceipt {
        allocation: prefix.allocation_id().into(),
        final_prefix: prefix.final_path().into(),
        sources: wheels.sources().to_vec(),
        files: wheels.files().to_vec(),
        directories: wheels.directories().to_vec(),
        inventory_sha256: *inventory.inventory_sha256(),
        observed,
        complete_files: files,
        complete_directories: directories,
    })
}

fn preflight(
    layout: &EmptyEnvironmentLayout,
    wheels: &WheelMaterializationReceipt,
    limits: RuntimeStartupLimits,
    inventory: super::PrefixInventoryLimits,
) -> Result<()> {
    let file_count = layout
        .runtime
        .files()
        .len()
        .checked_add(wheels.files().len())
        .context("installed limit: file count overflow")?;
    let directory_count = layout
        .runtime
        .reserved()
        .len()
        .checked_add(layout.normal_directories.len())
        .and_then(|count| count.checked_add(wheels.directories().len()))
        .context("installed limit: directory count overflow")?;
    ensure!(
        file_count <= inventory.max_files
            && directory_count <= inventory.max_directories
            && wheels.sources().len() <= limits.max_paths,
        "installed limit: receipt count exceeded"
    );
    let mut path_bytes = 0usize;
    let mut charge = |bytes: &[u8]| -> Result<()> {
        ensure!(
            bytes.len() <= limits.max_path_bytes,
            "installed limit: path or source text exceeded"
        );
        path_bytes = path_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= inventory.max_total_path_bytes)
            .context("installed limit: cumulative path bytes exceeded")?;
        Ok(())
    };
    for source in wheels.sources() {
        charge(source.filename().as_bytes())?;
        charge(source.owner().name().as_bytes())?;
        charge(source.owner().version().as_bytes())?;
    }
    let mut total_bytes = 0u64;
    for (path, size) in layout
        .runtime
        .files()
        .iter()
        .map(|file| (file.relative_path(), file.size()))
        .chain(
            wheels
                .files()
                .iter()
                .map(|file| (file.relative_path(), file.size())),
        )
    {
        charge(path.as_os_str().as_encoded_bytes())?;
        ensure!(
            size <= inventory.max_file_bytes,
            "installed limit: file bytes exceeded"
        );
        total_bytes = total_bytes
            .checked_add(size)
            .filter(|total| *total <= inventory.max_total_bytes)
            .context("installed limit: cumulative file bytes exceeded")?;
    }
    for path in layout
        .runtime
        .reserved()
        .iter()
        .map(|entry| entry.path.as_path())
        .chain(layout.normal_directories.iter().map(PathBuf::as_path))
        .chain(
            wheels
                .directories()
                .iter()
                .map(MaterializedWheelDirectory::relative_path),
        )
    {
        charge(path.as_os_str().as_encoded_bytes())?;
    }
    // Preflight the whole owner cross-product before building indexes or cloning.
    let mut owner_references = 0usize;
    for owners in wheels
        .files()
        .iter()
        .map(MaterializedWheelFile::owners)
        .chain(
            wheels
                .directories()
                .iter()
                .map(MaterializedWheelDirectory::owners),
        )
    {
        owner_references = owner_references
            .checked_add(owners.len())
            .filter(|total| *total <= inventory.max_files)
            .context("installed limit: owner references exceeded")?;
        ensure!(
            owners.windows(2).all(|pair| pair[0] < pair[1]),
            "installed inventory owner aliases disagree"
        );
    }
    let owners: BTreeSet<_> = wheels
        .sources()
        .iter()
        .map(PreparedWheelSource::owner)
        .collect();
    ensure!(
        owners.len() == wheels.sources().len(),
        "installed inventory repeats a source owner"
    );
    for file in wheels.files() {
        ensure!(
            !file.owners().is_empty(),
            "installed inventory file has no source owner"
        );
    }
    ensure!(
        wheels
            .files()
            .iter()
            .flat_map(|file| file.owners())
            .chain(
                wheels
                    .directories()
                    .iter()
                    .flat_map(|directory| directory.owners())
            )
            .all(|owner| owners.contains(owner)),
        "installed inventory has an unknown source owner"
    );
    Ok(())
}

fn expected_inventory(
    layout: &EmptyEnvironmentLayout,
    wheels: &WheelMaterializationReceipt,
) -> Result<(
    Vec<super::PrefixExpectedFile>,
    Vec<super::PrefixExpectedDirectory>,
)> {
    let files = layout
        .runtime
        .files()
        .iter()
        .map(|file| super::PrefixExpectedFile {
            relative: file.relative_path().into(),
            sha256: *file.sha256(),
            size: file.size(),
            mode: file.mode(),
        })
        .chain(wheels.files().iter().map(|file| super::PrefixExpectedFile {
            relative: file.relative_path().into(),
            sha256: *file.sha256(),
            size: file.size(),
            mode: file.mode(),
        }))
        .collect();
    let mut directories = BTreeMap::<PathBuf, u32>::new();
    for entry in layout
        .runtime
        .reserved()
        .iter()
        .filter(|entry| entry.kind == super::WheelReservedKind::Directory)
    {
        let relative = entry
            .path
            .strip_prefix(layout.final_prefix())
            .context("installed inventory runtime directory escaped final prefix")?;
        if !relative.as_os_str().is_empty() {
            directories.insert(relative.into(), 0o755);
        }
    }
    for relative in layout
        .normal_directories
        .iter()
        .map(PathBuf::as_path)
        .chain(
            wheels
                .directories()
                .iter()
                .map(MaterializedWheelDirectory::relative_path),
        )
    {
        if !relative.as_os_str().is_empty() {
            directories.insert(relative.into(), 0o755);
        }
    }
    Ok((
        files,
        directories
            .into_iter()
            .map(|(relative, mode)| super::PrefixExpectedDirectory { relative, mode })
            .collect(),
    ))
}

fn protect_startup(
    layout: &EmptyEnvironmentLayout,
    wheels: &WheelMaterializationReceipt,
) -> Result<()> {
    use super::super::runtime_bootstrap::RuntimeBootstrapFileKind;
    let base = layout.runtime.source_binding().layout();
    let mut controls = BTreeSet::new();
    let stdlibs = ["stdlib", "platstdlib"]
        .into_iter()
        .filter_map(|key| base.base_paths.get(key))
        .collect::<BTreeSet<_>>();
    for stdlib in &stdlibs {
        if let Some(relative) = projected_relative(base, stdlib) {
            controls.insert(relative);
        }
    }
    for path in &base.sys_path {
        if !stdlibs.contains(path)
            && let Some(relative) = projected_relative(base, path)
        {
            controls.insert(relative);
        }
    }
    for file in layout.runtime.files().iter().filter(|file| {
        matches!(
            file.kind(),
            RuntimeBootstrapFileKind::Executable | RuntimeBootstrapFileKind::LoaderPayload
        )
    }) {
        let mut pth = file.relative_path().as_os_str().to_os_string();
        pth.push("._pth");
        controls.insert(PathBuf::from(pth));
        controls.insert(
            file.relative_path()
                .parent()
                .context("installed startup protection lacks executable parent")?
                .join("pyvenv.cfg"),
        );
    }
    for file in wheels.files() {
        let path = file.relative_path();
        // Actual scheme roots, not guessed version directories, are safe to
        // populate because this post-install observer never enables site.
        if [
            crate::packages::python_wheel::WheelScheme::Purelib,
            crate::packages::python_wheel::WheelScheme::Platlib,
        ]
        .into_iter()
        .filter_map(|scheme| layout.actual_scheme().root(scheme))
        .any(|site| layout.final_prefix().join(path).starts_with(site))
        {
            continue;
        }
        ensure!(
            !path.ancestors().any(|ancestor| controls.contains(ancestor))
                && controls
                    .range::<Path, _>((std::ops::Bound::Included(path), std::ops::Bound::Unbounded))
                    .next()
                    .is_none_or(|control| !control.starts_with(path)),
            "installed startup protection rejects a new initialization input"
        );
    }
    Ok(())
}

fn projected_relative(
    base: &super::super::runtime_layout::RuntimeLayoutObservation,
    path: &Path,
) -> Option<PathBuf> {
    [
        Some(&base.prefix),
        Some(&base.exec_prefix),
        base.base_prefix.as_ref(),
        base.base_exec_prefix.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|root| {
        path.strip_prefix(root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| (root.components().count(), relative))
    })
    .max_by_key(|(depth, _)| *depth)
    .map(|(_, relative)| relative.to_path_buf())
}
