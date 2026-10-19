//! A local build from real owned inputs, without graph/publication authority.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use eyre::{Context, ContextCompat, Result, ensure};
use pakro_index::{PythonMarkerContext, PythonMarkerContextBuilder};
use tokio::time::Instant;

use super::prefix::UnpublishedPrefix;
use super::prefix::verify::{PrefixExpectedFile, PrefixInventoryLimits, verify_prefix_inventory};
use super::runtime_bootstrap::RuntimeBootstrapLimits;
use super::runtime_layout::RuntimeLayoutLimits;
use super::runtime_layout::{RuntimeLayoutObservation, RuntimeLoaderObservation};
use super::runtime_snapshot::OwnedRuntimeSnapshot;
use super::runtime_source::{
    RuntimeInputExpectation, RuntimeInputPurpose, RuntimeSourceBinding, RuntimeSourceTrust,
};
use super::runtime_startup::installed::EnvironmentVerificationReceipt;
use super::runtime_startup::{
    RuntimeStartupLimits, StartupAdapterBinding, StartupCacheBinding, StartupFilePin,
};
use super::seal::LocallySealedEnvironment;
use crate::packages::python_wheel::WheelLimits;
use crate::packages::python_wheel_install::{
    PreparedWheelSource, WheelInstallLimits, WheelInstallTarget, materialize_wheel_graph,
    prepare_wheel_graph,
};
use crate::packages::python_wheel_plan::WheelPlanLimits;
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::python_runtime::{PythonRuntimeDescriptor, PythonRuntimeHandle};

#[derive(Debug, Clone, Copy)]
pub(crate) struct EnvironmentBuildLimits {
    pub max_archives: usize,
    pub max_total_archive_bytes: u64,
    pub max_total_members: usize,
    pub max_total_uncompressed_bytes: u64,
    pub wheel: WheelLimits,
    pub plan: WheelPlanLimits,
    pub install: WheelInstallLimits,
    pub layout: RuntimeLayoutLimits,
    pub bootstrap: RuntimeBootstrapLimits,
    pub startup: RuntimeStartupLimits,
    pub inventory: PrefixInventoryLimits,
}
impl Default for EnvironmentBuildLimits {
    fn default() -> Self {
        Self {
            max_archives: 16_384,
            max_total_archive_bytes: 512 * 1024 * 1024 * 1024,
            max_total_members: 1_048_576,
            max_total_uncompressed_bytes: 512 * 1024 * 1024 * 1024,
            wheel: WheelLimits::default(),
            plan: WheelPlanLimits::default(),
            install: WheelInstallLimits::default(),
            layout: RuntimeLayoutLimits::default(),
            bootstrap: RuntimeBootstrapLimits::default(),
            startup: RuntimeStartupLimits::default(),
            inventory: PrefixInventoryLimits::default(),
        }
    }
}

impl EnvironmentBuildLimits {
    /// The same positive preflight used by the local build kernel.
    pub(crate) fn validate(self) -> Result<()> {
        let limits = self;
        ensure!(
            limits.max_archives != 0
                && limits.max_total_archive_bytes != 0
                && limits.max_total_members != 0
                && limits.max_total_uncompressed_bytes != 0,
            "local build batch limits must be nonzero"
        );
        crate::packages::python_wheel::validate_limits(limits.wheel)?;
        super::runtime_layout::validate_limits(limits.layout)?;
        super::runtime_startup::validate_limits(limits.startup)?;
        let p = limits.plan;
        let b = limits.bootstrap;
        let i = limits.install;
        let s = i.scripts;
        ensure!(
            [
                p.max_wheels,
                p.max_sources,
                p.max_files,
                p.max_directories,
                p.max_path_bytes,
                b.max_files,
                b.max_directories,
                b.max_path_bytes,
                b.max_generated_bytes,
                i.max_record_rows,
                i.max_record_bytes,
                s.max_entrypoints_bytes,
                s.max_entrypoints,
                s.max_entrypoint_value_bytes,
                s.max_wrapper_bytes,
                s.max_first_line_bytes
            ]
            .into_iter()
            .all(|value| value != 0)
                && [
                    p.max_total_bytes,
                    b.max_total_bytes,
                    i.max_generated_bytes,
                    s.max_input_bytes,
                    s.max_output_bytes
                ]
                .into_iter()
                .all(|value| value != 0),
            "local build nested limits must be nonzero"
        );
        Ok(())
    }
}

pub(crate) struct PreparedEnvironmentBuild {
    // Keep the complete base alive until after environment cleanup on Drop.
    environment: LocallySealedEnvironment,
    snapshot: OwnedRuntimeSnapshot,
}
impl PreparedEnvironmentBuild {
    pub(crate) fn allocation_id(&self) -> &str {
        self.environment.allocation_id()
    }
    pub(crate) fn final_prefix(&self) -> &Path {
        self.environment.final_prefix()
    }
    pub(crate) fn executable(&self) -> &Path {
        &self
            .environment
            .verification()
            .observed_runtime()
            .executable
    }
    pub(crate) fn runtime_root(&self) -> &Path {
        self.snapshot.root()
    }
    pub(crate) fn runtime_content_tree_sha256(&self) -> &[u8; 32] {
        self.snapshot.content_tree_sha256()
    }
    pub(crate) fn runtime_inventory_sha256(&self) -> &[u8; 32] {
        self.snapshot.inventory_sha256()
    }
    pub(crate) fn verification(&self) -> &EnvironmentVerificationReceipt {
        self.environment.verification()
    }
    pub(crate) fn sources(&self) -> &[PreparedWheelSource] {
        self.environment.verification().sources()
    }
    pub(crate) fn recheck(&self, inventory: PrefixInventoryLimits) -> Result<()> {
        self.environment.recheck(inventory)?;
        self.snapshot.recheck(inventory)
    }
}
impl std::fmt::Debug for PreparedEnvironmentBuild {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("PreparedEnvironmentBuild")
            .field("allocation", &self.allocation_id())
            .field("sources", &self.sources().len())
            .finish_non_exhaustive()
    }
}

pub(crate) async fn prepare_environment_build(
    snapshot: OwnedRuntimeSnapshot,
    archives: Vec<InspectedWheelArchive>,
    mut destination: UnpublishedPrefix,
    startup: StartupAdapterBinding,
    limits: EnvironmentBuildLimits,
    deadline: Instant,
) -> Result<PreparedEnvironmentBuild> {
    check_deadline(deadline)?;
    preflight_batch(&archives, limits)?;
    verify_prefix_inventory(&destination, &[], &[], limits.inventory)
        .context("local build destination must be empty")?;
    snapshot.recheck(limits.inventory)?;
    validate_startup_membership(&snapshot, &startup)?;
    check_deadline(deadline)?;
    let selected = PythonRuntimeHandle::from_existing_executable(snapshot.entrypoint())?;
    let descriptor = crate::python_runtime::probe_at_deadline(
        &selected,
        deadline,
        limits.layout.max_stdout_bytes as u64,
        limits.layout.max_stderr_bytes as u64,
    )
    .await
    .context("local build selected runtime probe failed")?;
    validate_archive_target(&archives, &descriptor)?;
    let raw = crate::python_runtime::probe_layout(
        &selected,
        destination.final_path(),
        limits.layout.max_stdout_bytes as u64,
        limits.layout.max_stderr_bytes as u64,
        deadline,
    )
    .await
    .context("local build selected layout probe failed")?;
    let layout = super::runtime_layout::decode_runtime_layout(
        &raw,
        &descriptor,
        destination.final_path(),
        limits.layout,
    )?;
    let inputs = runtime_inputs(&snapshot, &descriptor, &layout, limits.layout)?;
    let roots = [snapshot.root().to_path_buf()];
    let sources = super::runtime_source::capture_runtime_sources_at_deadline(
        &selected,
        &descriptor,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &roots,
            inputs: &inputs,
        },
        &destination,
        limits.layout,
        deadline,
    )
    .await?;
    // D1 repeats the probe. Reclassify every reported image from those facts;
    // declaring a subset of loader payloads would leave that second run unchecked.
    let repeated = runtime_inputs(&snapshot, &descriptor, sources.layout(), limits.layout)?;
    ensure!(
        same_inputs(&inputs, &repeated),
        "local build loaded-image inputs changed"
    );
    super::runtime_source::startup::capture_startup_snapshots(&sources, &startup, limits.startup)
        .context("local build startup binding is invalid")?;
    check_deadline(deadline)?;
    let plan = super::runtime_bootstrap::prepare_runtime_bootstrap(
        sources,
        &destination,
        limits.bootstrap,
    )?;
    let runtime =
        super::runtime_bootstrap::bootstrap_runtime_at_deadline(plan, &mut destination, deadline)
            .await?;
    let recognition = super::runtime_startup::recognize_at_deadline(
        &runtime,
        &destination,
        startup,
        limits.startup,
        deadline,
    )
    .await?;
    let layout = super::runtime_startup::verify_empty_environment_layout_at_deadline(
        runtime,
        &mut destination,
        recognition,
        limits.startup,
        deadline,
    )
    .await?;
    check_deadline(deadline)?;
    let scratch = tempfile::tempdir().context("local build private scratch unavailable")?;
    let graph = prepare_wheel_graph(
        archives,
        &WheelInstallTarget {
            scheme: layout.actual_scheme(),
            interpreter: layout.executable(),
            reserved: layout.reserved(),
        },
        limits.wheel,
        limits.plan,
        limits.install,
        scratch.path(),
    )?;
    check_deadline(deadline)?;
    let wheels = materialize_wheel_graph(graph, &mut destination)?;
    check_deadline(deadline)?;
    let verified = super::runtime_startup::installed::verify_materialized_environment_at_deadline(
        &layout,
        &wheels,
        &destination,
        limits.startup,
        deadline,
    )
    .await?;
    snapshot.recheck(limits.inventory)?;
    check_deadline(deadline)?;
    let environment = super::seal::seal_verified_environment(
        layout,
        wheels,
        destination,
        verified,
        limits.inventory,
    )?;
    snapshot.recheck(limits.inventory)?;
    check_deadline(deadline)?;
    Ok(PreparedEnvironmentBuild {
        environment,
        snapshot,
    })
}

fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(Instant::now() < deadline, "local build deadline expired");
    Ok(())
}

fn preflight_batch(
    archives: &[InspectedWheelArchive],
    limits: EnvironmentBuildLimits,
) -> Result<()> {
    limits.validate()?;
    let p = limits.plan;
    ensure!(
        archives.len() <= limits.max_archives && archives.len() <= p.max_wheels,
        "local build archive count limit exceeded"
    );
    let (mut bytes, mut members, mut unpacked) = (0u64, 0u64, 0u64);
    for archive in archives {
        archive.check_limits(limits.wheel)?;
        bytes = bytes
            .checked_add(archive.size())
            .filter(|n| *n <= limits.max_total_archive_bytes)
            .context("local build archive bytes limit exceeded")?;
        members = members
            .checked_add(archive.actual_member_count())
            .filter(|n| *n <= limits.max_total_members as u64)
            .context("local build member count limit exceeded")?;
        unpacked = unpacked
            .checked_add(archive.actual_uncompressed_bytes())
            .filter(|n| *n <= limits.max_total_uncompressed_bytes)
            .context("local build uncompressed bytes limit exceeded")?;
    }
    Ok(())
}

fn validate_archive_target(
    archives: &[InspectedWheelArchive],
    descriptor: &PythonRuntimeDescriptor,
) -> Result<()> {
    let target = crate::packages::python_target::wheel_target(descriptor)?;
    let m = &descriptor.markers;
    let context = PythonMarkerContext::new(PythonMarkerContextBuilder {
        implementation_name: &m.implementation_name,
        implementation_version: &m.implementation_version,
        os_name: &m.os_name,
        platform_machine: &m.platform_machine,
        platform_python_implementation: &m.platform_python_implementation,
        platform_release: &m.platform_release,
        platform_system: &m.platform_system,
        platform_version: &m.platform_version,
        python_full_version: &m.python_full_version,
        python_version: &m.python_version,
        sys_platform: &m.sys_platform,
    })?;
    for archive in archives {
        ensure!(
            archive
                .manifest()
                .tags()
                .iter()
                .any(|tag| target.supported.ordered().contains(tag)),
            "local build wheel tags do not match the selected runtime"
        );
        for requirement in archive.manifest().core_metadata().values("requires-python") {
            let matches = pakro_index::python_requires_matches_for_context(requirement, &context)
                .map_err(|_| eyre::eyre!("local build Requires-Python is invalid"))?;
            ensure!(
                matches,
                "local build Requires-Python does not match the selected runtime"
            );
        }
    }
    Ok(())
}

fn snapshot_file<'a>(
    snapshot: &'a OwnedRuntimeSnapshot,
    path: &Path,
) -> Result<&'a PrefixExpectedFile> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|p| matches!(p, Component::ParentDir | Component::CurDir)),
        "local build snapshot path is not normalized"
    );
    let relative = path
        .strip_prefix(snapshot.root())
        .context("local build input is outside its snapshot")?;
    snapshot
        .files()
        .iter()
        .find(|file| file.relative == relative)
        .context("local build input is absent from its snapshot manifest")
}

fn validate_startup_membership(
    snapshot: &OwnedRuntimeSnapshot,
    binding: &StartupAdapterBinding,
) -> Result<()> {
    let pin_matches = |pin: &StartupFilePin| -> Result<()> {
        let file = snapshot_file(snapshot, &pin.path)?;
        ensure!(
            file.sha256 == pin.sha256 && file.size == pin.size && file.mode == pin.mode,
            "local build startup pin differs from its snapshot manifest"
        );
        Ok(())
    };
    pin_matches(&binding.source)?;
    match &binding.cache {
        Some(StartupCacheBinding::Pinned(pin)) => pin_matches(pin)?,
        Some(StartupCacheBinding::Missing { path }) => {
            ensure!(
                path.starts_with(snapshot.root())
                    && !path
                        .components()
                        .any(|p| matches!(p, Component::ParentDir | Component::CurDir)),
                "local build missing cache is outside its snapshot"
            );
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => eyre::bail!("local build missing cache is present or unavailable"),
            }
        }
        None => {}
    }
    Ok(())
}

fn runtime_inputs(
    snapshot: &OwnedRuntimeSnapshot,
    descriptor: &PythonRuntimeDescriptor,
    layout: &RuntimeLayoutObservation,
    limits: RuntimeLayoutLimits,
) -> Result<Vec<RuntimeInputExpectation>> {
    let mut inputs = BTreeMap::new();
    let mut add = |path: &Path, purpose| -> Result<()> {
        let canonical = path
            .canonicalize()
            .context("local build runtime input cannot be resolved")?;
        ensure!(
            canonical == path,
            "local build runtime input is not canonical"
        );
        let file = snapshot_file(snapshot, path)?;
        inputs.insert(
            path.to_path_buf(),
            RuntimeInputExpectation {
                path: path.to_path_buf(),
                purpose,
                sha256: file.sha256,
                size: file.size,
                mode: file.mode,
            },
        );
        ensure!(
            inputs.len() <= limits.max_input_files,
            "local build input count limit exceeded"
        );
        Ok(())
    };
    ensure!(
        descriptor.executable == snapshot.entrypoint(),
        "local build executable differs from snapshot entrypoint"
    );
    add(snapshot.entrypoint(), RuntimeInputPurpose::Executable)?;
    let images = match &layout.loader {
        RuntimeLoaderObservation::Observed { images } => images,
        RuntimeLoaderObservation::Unknown { .. } => {
            eyre::bail!("local build loaded-image observation is unknown")
        }
    };
    for image in images {
        if image == snapshot.entrypoint() {
            continue;
        }
        if image.starts_with(snapshot.root()) {
            add(image, RuntimeInputPurpose::LoaderPayload)?;
        } else {
            // macOS shared-cache image names need not exist as leaf files. The
            // fixed producer reports realpath-normalized names; do not reopen
            // nonexistent system cache leaves as if they were private inputs.
            let roots: &[&str] = match descriptor.markers.sys_platform.as_str() {
                "darwin" => &["/System/Library", "/usr/lib"],
                "linux" => &["/lib", "/lib64", "/usr/lib", "/usr/lib64"],
                _ => &[],
            };
            ensure!(
                roots.iter().any(|root| image.starts_with(root)),
                "local build loaded image is outside the snapshot and system library roots"
            );
        }
    }
    Ok(inputs.into_values().collect())
}

fn same_inputs(left: &[RuntimeInputExpectation], right: &[RuntimeInputExpectation]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            a.path == b.path
                && a.purpose == b.purpose
                && a.sha256 == b.sha256
                && a.size == b.size
                && a.mode == b.mode
        })
}

#[cfg(test)]
mod tests;
