//! Runtime observations derived only from an owned, rechecked private snapshot.
//! The frozen target remains data; this owner grants no installation authority.

use std::path::Path;

use pakro_index::python_markers::{MarkerSemantics, PythonMarkerEnvironment, PythonMarkerScope};
use thiserror::Error;
use tokio::time::Instant;

use super::prefix::verify::PrefixInventoryLimits;
use super::runtime_snapshot::OwnedRuntimeSnapshot;
use crate::packages::python_resolution::source::locked_graph::{
    FrozenRegistryTarget, FrozenTargetLimits,
};
use crate::python_runtime::{PythonRuntimeDescriptor, PythonRuntimeHandle, probe_at_deadline};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeTargetLimits {
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
    pub inventory: PrefixInventoryLimits,
    pub target: FrozenTargetLimits,
}
impl Default for RuntimeTargetLimits {
    fn default() -> Self {
        Self {
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            inventory: PrefixInventoryLimits::default(),
            target: FrozenTargetLimits::default(),
        }
    }
}

pub(crate) struct ObservedRegistryRuntimeTarget {
    snapshot: OwnedRuntimeSnapshot,
    descriptor: PythonRuntimeDescriptor,
    target: FrozenRegistryTarget,
}
impl ObservedRegistryRuntimeTarget {
    pub(crate) fn root(&self) -> &Path {
        self.snapshot.root()
    }
    pub(crate) fn entrypoint(&self) -> &Path {
        self.snapshot.entrypoint()
    }
    pub(crate) fn content_tree_sha256(&self) -> &[u8; 32] {
        self.snapshot.content_tree_sha256()
    }
    pub(crate) fn inventory_sha256(&self) -> &[u8; 32] {
        self.snapshot.inventory_sha256()
    }
    pub(crate) fn descriptor(&self) -> &PythonRuntimeDescriptor {
        &self.descriptor
    }
    pub(crate) fn target(&self) -> &FrozenRegistryTarget {
        &self.target
    }
    /// Recheck this snapshot instance. Later build probes must also match the
    /// frozen target; OS/library observations are not a permanent attestation.
    pub(crate) fn recheck(
        &self,
        inventory: PrefixInventoryLimits,
        deadline: Instant,
    ) -> Result<(), RuntimeTargetError> {
        check_deadline(deadline)?;
        self.snapshot
            .recheck(inventory)
            .map_err(|_| RuntimeTargetError::Inventory)?;
        check_deadline(deadline)
    }
}
impl ObservedRegistryRuntimeTarget {
    /// A fixed consumer of the private snapshot; no general extraction exists.
    pub(super) async fn complete_resolved_build<'lock>(
        self,
        permit: super::resolved_build::RuntimeBuildPermit<'lock>,
    ) -> Result<
        super::resolved_build::PreparedResolvedEnvironment<'lock>,
        super::resolved_build::ResolvedBuildError,
    > {
        permit.complete(self.snapshot, self.target).await
    }
}
impl std::fmt::Debug for ObservedRegistryRuntimeTarget {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("ObservedRegistryRuntimeTarget")
            .field("supported_tags", &self.target.supported().ordered().len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum RuntimeTargetError {
    #[error("runtime target limits must be positive")]
    InvalidLimits,
    #[error("runtime target deadline expired")]
    Deadline,
    #[error("runtime snapshot inventory could not be verified")]
    Inventory,
    #[error("selected runtime probe failed")]
    Probe,
    #[error("selected runtime has incomplete native target observations")]
    IncompleteTarget,
    #[error("observed registry target could not be frozen")]
    FrozenTarget,
}

pub(crate) async fn observe_registry_runtime_target(
    snapshot: OwnedRuntimeSnapshot,
    root_scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    limits: RuntimeTargetLimits,
    deadline: Instant,
) -> Result<ObservedRegistryRuntimeTarget, RuntimeTargetError> {
    if limits.max_stdout_bytes == 0 || limits.max_stderr_bytes == 0 {
        return Err(RuntimeTargetError::InvalidLimits);
    }
    limits
        .target
        .validate()
        .map_err(|_| RuntimeTargetError::InvalidLimits)?;
    check_deadline(deadline)?;
    snapshot
        .recheck(limits.inventory)
        .map_err(|_| RuntimeTargetError::Inventory)?;
    check_deadline(deadline)?;
    let selected = PythonRuntimeHandle::from_existing_executable(snapshot.entrypoint())
        .map_err(|_| RuntimeTargetError::Probe)?;
    let probed = probe_at_deadline(
        &selected,
        deadline,
        limits.max_stdout_bytes,
        limits.max_stderr_bytes,
    )
    .await;
    check_deadline(deadline)?;
    let descriptor = probed.map_err(|_| RuntimeTargetError::Probe)?;
    if descriptor.executable != snapshot.entrypoint() {
        return Err(RuntimeTargetError::Probe);
    }
    snapshot
        .recheck(limits.inventory)
        .map_err(|_| RuntimeTargetError::Inventory)?;
    check_deadline(deadline)?;
    let target = freeze_observed_target(&descriptor, root_scope, semantics, limits.target)?;
    check_deadline(deadline)?;
    Ok(ObservedRegistryRuntimeTarget {
        snapshot,
        descriptor,
        target,
    })
}

// This projects data, never an ObservedRegistryRuntimeTarget. Only the factory
// above combines the actual probe with ownership of its rechecked snapshot.
pub(crate) fn freeze_observed_target(
    descriptor: &PythonRuntimeDescriptor,
    root_scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    limits: FrozenTargetLimits,
) -> Result<FrozenRegistryTarget, RuntimeTargetError> {
    let projected = crate::packages::python_target::wheel_target(descriptor)
        .map_err(|_| RuntimeTargetError::IncompleteTarget)?;
    if projected.observed_platform.is_none()
        || projected.capability_reason.is_some()
        || !projected
            .supported
            .ordered()
            .iter()
            .any(|tag| tag.platform() != "any")
    {
        return Err(RuntimeTargetError::IncompleteTarget);
    }
    let markers = &descriptor.markers;
    FrozenRegistryTarget::new(
        PythonMarkerEnvironment {
            os_name: &markers.os_name,
            sys_platform: &markers.sys_platform,
            platform_release: &markers.platform_release,
            platform_system: &markers.platform_system,
            platform_version: &markers.platform_version,
            platform_machine: &markers.platform_machine,
            platform_python_implementation: &markers.platform_python_implementation,
            python_version: &markers.python_version,
            python_full_version: &markers.python_full_version,
            implementation_name: &markers.implementation_name,
            implementation_version: &markers.implementation_version,
        },
        &projected.supported,
        root_scope,
        semantics,
        limits,
    )
    .map_err(|_| RuntimeTargetError::FrozenTarget)
}

fn check_deadline(deadline: Instant) -> Result<(), RuntimeTargetError> {
    if Instant::now() >= deadline {
        return Err(RuntimeTargetError::Deadline);
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
    use crate::packages::python_environment::runtime_snapshot::{
        RuntimeSnapshotLimits, snapshot_runtime,
    };

    struct Fixture {
        _owner: tempfile::TempDir,
        source: PathBuf,
        witness: PathBuf,
        store: EnvironmentStore,
    }
    impl Fixture {
        fn new() -> Self {
            let owner = tempfile::tempdir().unwrap();
            let root = owner.path().canonicalize().unwrap();
            let source = root.join("source");
            fs::create_dir_all(source.join("bin")).unwrap();
            let witness = root.join("actual-probe-argv");
            let quote = format!("'{}'", witness.to_str().unwrap().replace('\'', "'\\''"));
            // A deliberately failing real executable, never a positive Python
            // descriptor fixture or a fabricated runtime capability.
            fs::write(
                source.join("bin/python"),
                format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {quote}\nexit 9\n"),
            )
            .unwrap();
            fs::set_permissions(source.join("bin/python"), fs::Permissions::from_mode(0o755))
                .unwrap();
            let store = EnvironmentStore::open(
                &root.join("snapshots"),
                nix::unistd::geteuid().as_raw(),
                PrefixLimits::default(),
            )
            .unwrap();
            Self {
                _owner: owner,
                source,
                witness,
                store,
            }
        }
        fn snapshot(&self) -> OwnedRuntimeSnapshot {
            snapshot_runtime(
                &self.source,
                Path::new("bin/python"),
                self.store.allocate().unwrap(),
                RuntimeSnapshotLimits::default(),
            )
            .unwrap()
        }
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    async fn observe(
        snapshot: OwnedRuntimeSnapshot,
        limits: RuntimeTargetLimits,
        deadline: Instant,
    ) -> Result<ObservedRegistryRuntimeTarget, RuntimeTargetError> {
        observe_registry_runtime_target(
            snapshot,
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            limits,
            deadline,
        )
        .await
    }

    #[tokio::test]
    async fn zero_output_and_nested_target_limits_refuse_before_executing_a_program() {
        let fixture = Fixture::new();
        for limits in [
            RuntimeTargetLimits {
                max_stdout_bytes: 0,
                ..Default::default()
            },
            RuntimeTargetLimits {
                max_stderr_bytes: 0,
                ..Default::default()
            },
            RuntimeTargetLimits {
                target: FrozenTargetLimits {
                    max_supported_tags: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            RuntimeTargetLimits {
                target: FrozenTargetLimits {
                    markers: pakro_index::python_markers::PythonMarkerLimits {
                        max_nodes: 0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            assert_eq!(
                observe(fixture.snapshot(), limits, deadline())
                    .await
                    .unwrap_err(),
                RuntimeTargetError::InvalidLimits
            );
            assert!(!fixture.witness.exists());
        }
    }
    #[tokio::test]
    async fn an_expired_shared_deadline_refuses_before_launching_the_program() {
        let fixture = Fixture::new();
        assert_eq!(
            observe(fixture.snapshot(), Default::default(), Instant::now())
                .await
                .unwrap_err(),
            RuntimeTargetError::Deadline
        );
        assert!(!fixture.witness.exists());
    }
    #[tokio::test]
    async fn actual_snapshot_bytes_changed_before_observation_cannot_be_probed() {
        let fixture = Fixture::new();
        let original = fs::read(fixture.source.join("bin/python")).unwrap();
        let snapshot = fixture.snapshot();
        fs::write(snapshot.entrypoint(), b"changed private snapshot").unwrap();
        assert_eq!(
            observe(snapshot, Default::default(), deadline())
                .await
                .unwrap_err(),
            RuntimeTargetError::Inventory
        );
        assert!(!fixture.witness.exists());
        assert_eq!(
            fs::read(fixture.source.join("bin/python")).unwrap(),
            original
        );
    }
    #[tokio::test]
    async fn a_real_failed_probe_keeps_the_existing_isolation_and_no_bytecode_flags() {
        let fixture = Fixture::new();
        assert_eq!(
            observe(fixture.snapshot(), Default::default(), deadline())
                .await
                .unwrap_err(),
            RuntimeTargetError::Probe
        );
        let argv = fs::read_to_string(&fixture.witness).unwrap();
        assert_eq!(
            argv.lines().take(5).collect::<Vec<_>>(),
            ["-E", "-s", "-S", "-B", "-c"]
        );
    }
    #[tokio::test]
    async fn inventory_budgets_use_the_actual_v1_preflight_before_any_probe() {
        let fixture = Fixture::new();
        for inventory in [
            PrefixInventoryLimits {
                max_files: 0,
                ..Default::default()
            },
            PrefixInventoryLimits {
                max_file_bytes: 1,
                ..Default::default()
            },
        ] {
            assert_eq!(
                observe(
                    fixture.snapshot(),
                    RuntimeTargetLimits {
                        inventory,
                        ..Default::default()
                    },
                    deadline()
                )
                .await
                .unwrap_err(),
                RuntimeTargetError::Inventory
            );
            assert!(!fixture.witness.exists());
        }
    }

    async fn live(source: &Path, entrypoint: &Path) {
        let owner = tempfile::tempdir().unwrap();
        let original = fs::read(source.join(entrypoint)).unwrap();
        let store = EnvironmentStore::open(
            &owner.path().canonicalize().unwrap().join("snapshots"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let snapshot = snapshot_runtime(
            source,
            entrypoint,
            store.allocate().unwrap(),
            Default::default(),
        )
        .unwrap();
        let root = snapshot.root().to_owned();
        let executable = snapshot.entrypoint().to_owned();
        let content = *snapshot.content_tree_sha256();
        let inventory = *snapshot.inventory_sha256();
        let observed = observe_registry_runtime_target(
            snapshot,
            PythonMarkerScope::Dependency {
                requested_extras: &["Safe_Scope", "safe-scope"],
            },
            MarkerSemantics::Current2026,
            Default::default(),
            Instant::now() + Duration::from_secs(300),
        )
        .await
        .unwrap();
        assert_eq!(observed.root(), root);
        assert_eq!(observed.entrypoint(), executable);
        assert_eq!(observed.content_tree_sha256(), &content);
        assert_eq!(observed.inventory_sha256(), &inventory);
        assert_eq!(
            observed.descriptor().probe_flags,
            ["-E", "-s", "-S", "-B", "-c"]
        );
        assert_eq!(observed.descriptor().executable, executable);
        assert!(
            observed
                .target()
                .supported()
                .ordered()
                .iter()
                .any(|tag| tag.platform() != "any")
        );
        let actual = observed.target().environment();
        let markers = &observed.descriptor().markers;
        assert_eq!(
            [
                actual.os_name,
                actual.sys_platform,
                actual.platform_release,
                actual.platform_system,
                actual.platform_version,
                actual.platform_machine,
                actual.platform_python_implementation,
                actual.python_version,
                actual.python_full_version,
                actual.implementation_name,
                actual.implementation_version,
            ],
            [
                markers.os_name.as_str(),
                markers.sys_platform.as_str(),
                markers.platform_release.as_str(),
                markers.platform_system.as_str(),
                markers.platform_version.as_str(),
                markers.platform_machine.as_str(),
                markers.platform_python_implementation.as_str(),
                markers.python_version.as_str(),
                markers.python_full_version.as_str(),
                markers.implementation_name.as_str(),
                markers.implementation_version.as_str(),
            ]
        );
        observed.target().with_root_scope(|scope| {
            let PythonMarkerScope::Dependency { requested_extras } = scope else {
                panic!("root scope changed")
            };
            assert_eq!(requested_extras, ["safe-scope"]);
        });
        // A private data-only negative projection of actual observed facts;
        // it cannot construct an Observed owner from an arbitrary descriptor.
        let mut unknown = observed.descriptor().clone();
        unknown.compatibility = None;
        assert_eq!(
            freeze_observed_target(
                &unknown,
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                Default::default(),
            )
            .unwrap_err(),
            RuntimeTargetError::IncompleteTarget
        );
        observed.recheck(Default::default(), deadline()).unwrap();
        fs::write(
            observed.entrypoint(),
            b"changed owned copy after real observation",
        )
        .unwrap();
        assert_eq!(
            observed
                .recheck(Default::default(), deadline())
                .unwrap_err(),
            RuntimeTargetError::Inventory
        );
        assert_eq!(fs::read(source.join(entrypoint)).unwrap(), original);
    }
    #[tokio::test]
    #[ignore = "explicit complete CLT snapshot and actual observed target factory"]
    async fn actual_clt_snapshot_observed_registry_target() {
        live(Path::new("/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9"),Path::new("bin/python3.9")).await;
    }
    #[tokio::test]
    #[ignore = "explicit complete owned311 snapshot and actual observed target factory"]
    async fn actual_owned311_snapshot_observed_registry_target() {
        live(
            Path::new("/private/tmp/pakro-d3-owned-cpython311-2b8nnsx3"),
            Path::new("bin/python3.11"),
        )
        .await;
    }
    #[tokio::test]
    #[ignore = "explicit complete PyPy snapshot and actual observed target factory"]
    async fn actual_pypy_snapshot_observed_registry_target() {
        live(
            Path::new("/private/tmp/pakro-python-live/data/installs/python/pypy3.12-8.0.1"),
            Path::new("bin/pypy3.12"),
        )
        .await;
    }
}
