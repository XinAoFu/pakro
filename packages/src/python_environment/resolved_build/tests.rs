use super::*;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::prefix::{EnvironmentStore, PrefixLimits};
use super::super::runtime_layout::{RuntimeLayoutLimits, decode_runtime_layout};
use super::super::runtime_snapshot::{OwnedRuntimeSnapshot, snapshot_runtime};
use super::super::runtime_startup::{StartupCacheBinding, StartupFilePin, StartupLoaderKind};
use super::super::runtime_target::observe_registry_runtime_target;
use crate::packages::python_resolution::source::registry_depot::RegistryBuildFixture;
use crate::python_runtime::{PythonRuntimeHandle, probe_at_deadline, probe_layout};
use pakro_index::python_markers::{MarkerSemantics, PythonMarkerScope};

const CLT_ROOT: &str =
    "/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9";
const CLT_EXE: &str = "bin/python3.9";
// Test-only output from actual production snapshot/observation factories.
// Callers retain _owner while consuming the genuine build input capabilities.
pub(crate) struct ActualRuntime {
    pub(crate) _owner: tempfile::TempDir,
    pub(crate) runtime: ObservedRegistryRuntimeTarget,
    pub(crate) destination: UnpublishedPrefix,
    pub(crate) startup: StartupAdapterBinding,
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(240)
}

async fn startup_binding(
    snapshot: &OwnedRuntimeSnapshot,
    destination: &UnpublishedPrefix,
    cwd: &Path,
    deadline: Instant,
) -> StartupAdapterBinding {
    let handle = PythonRuntimeHandle::from_existing_executable(snapshot.entrypoint()).unwrap();
    let selected = probe_at_deadline(&handle, deadline, 1024 * 1024, 64 * 1024)
        .await
        .unwrap();
    let raw = probe_layout(
        &handle,
        destination.final_path(),
        1024 * 1024,
        64 * 1024,
        deadline,
    )
    .await
    .unwrap();
    let layout = decode_runtime_layout(
        &raw,
        &selected,
        destination.final_path(),
        RuntimeLayoutLimits::default(),
    )
    .unwrap();
    let site = layout.base_paths["stdlib"].join("site.py");
    let mut child = tokio::process::Command::new(snapshot.entrypoint());
    child.args(["-E","-s","-S","-B","-c","import importlib.util,json,sys; s=importlib.util.find_spec('site'); print(json.dumps({'origin':s.origin,'cache':importlib.util.cache_from_source(sys.argv[1])}))"])
        .arg(&site).env_clear().env("LC_ALL","C").current_dir(cwd).kill_on_drop(true);
    let output = tokio::time::timeout_at(deadline, child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success() && output.stderr.is_empty() && output.stdout.len() < 8192,
        "{output:?}"
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pin = |path: &Path| {
        let relative = path.strip_prefix(snapshot.root()).unwrap();
        let actual = snapshot
            .files()
            .iter()
            .find(|file| file.relative == relative)
            .unwrap();
        StartupFilePin {
            path: path.to_owned(),
            sha256: actual.sha256,
            size: actual.size,
            mode: actual.mode,
        }
    };
    let (loader, cache) = if actual["origin"] == "frozen" {
        (StartupLoaderKind::Frozen, None)
    } else {
        assert_eq!(
            Path::new(actual["origin"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            site.canonicalize().unwrap()
        );
        let path = PathBuf::from(actual["cache"].as_str().unwrap());
        let cache = if fs::symlink_metadata(&path).is_ok() {
            StartupCacheBinding::Pinned(pin(&path))
        } else {
            StartupCacheBinding::Missing { path }
        };
        (StartupLoaderKind::Source, Some(cache))
    };
    StartupAdapterBinding {
        source: pin(&site),
        cache,
        loader,
    }
}
async fn actual_runtime(source: &Path, executable: &Path) -> ActualRuntime {
    actual_runtime_in_scope(
        source,
        executable,
        PythonMarkerScope::Environment,
        deadline(),
    )
    .await
}
async fn actual_runtime_in_scope(
    source: &Path,
    executable: &Path,
    scope: PythonMarkerScope<'_>,
    deadline: Instant,
) -> ActualRuntime {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let snapshots = EnvironmentStore::open(
        &root.join("snapshots"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let environments = EnvironmentStore::open(
        &root.join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap();
    let snapshot = snapshot_runtime(
        source,
        executable,
        snapshots.allocate().unwrap(),
        Default::default(),
    )
    .expect("complete actual runtime snapshot prelude");
    let destination = environments.allocate().unwrap();
    let startup = startup_binding(&snapshot, &destination, &root, deadline).await;
    let runtime = observe_registry_runtime_target(
        snapshot,
        scope,
        MarkerSemantics::Current2026,
        Default::default(),
        deadline,
    )
    .await
    .expect("actual Observed runtime capability prelude");
    runtime.recheck(Default::default(), deadline).unwrap();
    ActualRuntime {
        _owner: owner,
        runtime,
        destination,
        startup,
    }
}
pub(crate) async fn actual_clt_inputs(
    scope: PythonMarkerScope<'_>,
    deadline: Instant,
) -> ActualRuntime {
    actual_runtime_in_scope(Path::new(CLT_ROOT), Path::new(CLT_EXE), scope, deadline).await
}
async fn clt() -> ActualRuntime {
    actual_runtime(Path::new(CLT_ROOT), Path::new(CLT_EXE)).await
}

fn assert_receipt(result: &PreparedResolvedEnvironment<'_>) {
    assert_eq!(result.sources().len(), 2);
    assert_eq!(result.plan().selected.len(), 2);
    assert_eq!(result.checked().edges.len(), 1);
    for selected in &result.plan().selected {
        let pin = result
            .source_pin(selected.key)
            .expect("retained actual source pin");
        let rows = result
            .sources()
            .iter()
            .filter(|source| source.owner().name() == pin.file().project())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename(), pin.file().filename());
        assert_eq!(rows[0].digest(), pin.digest());
        assert_eq!(rows[0].size(), pin.size());
    }
    assert!(
        result
            .sources()
            .iter()
            .any(|source| matches!(source.digest(), pakro_store::Digest::Sha512(_)))
    );
    assert!(
        result
            .sources()
            .iter()
            .any(|source| matches!(source.digest(), pakro_store::Digest::Sha256(_)))
    );
}

#[tokio::test]
async fn actual_closed_fresh_sources_and_runtime_enter_the_complete_local_build() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let content = *runtime.content_tree_sha256();
    let inventory = *runtime.inventory_sha256();
    // RP is actual archive metadata, not a fabricated resolver fact.
    let fixture = RegistryBuildFixture::new(">=3.9,<3.10");
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture
        .closed(
            &root,
            runtime.target(),
            &["demo-pkg==1.0", "inactive; python_version<'2'"],
        )
        .await;
    let requests = fixture.requests();
    let result = prepare_resolved_environment(
        graph,
        runtime,
        destination,
        startup,
        Default::default(),
        deadline(),
    )
    .await
    .unwrap();
    assert_receipt(&result);
    assert_eq!(result.runtime_content_tree_sha256(), &content);
    assert_eq!(result.runtime_inventory_sha256(), &inventory);
    assert_eq!(fixture.requests(), requests);
    fixture.assert_unchanged(image);
}

#[tokio::test]
async fn a_different_full_target_cannot_authorize_the_actual_runtime() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let mut environment = runtime.target().environment();
    environment.python_full_version = "3.9.999";
    environment.implementation_version = "3.9.999";
    let target = FrozenRegistryTarget::new(
        environment,
        runtime.target().supported(),
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap();
    assert!(!target.matches(runtime.target()));
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture.closed(&root, &target, &["demo-pkg==1.0"]).await;
    let path = destination.final_path().to_owned();
    assert_eq!(
        prepare_resolved_environment(
            graph,
            runtime,
            destination,
            startup,
            Default::default(),
            deadline()
        )
        .await
        .unwrap_err(),
        ResolvedBuildError::TargetMismatch
    );
    assert!(!path.exists());
    fixture.assert_unchanged(image);
}

#[tokio::test]
async fn a_different_root_scope_cannot_be_substituted_for_the_observed_target_scope() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let target = FrozenRegistryTarget::new(
        runtime.target().environment(),
        runtime.target().supported(),
        PythonMarkerScope::Dependency {
            requested_extras: &["other-scope"],
        },
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap();
    assert!(!target.matches(runtime.target()));
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture.closed(&root, &target, &["demo-pkg==1.0"]).await;
    assert_eq!(
        prepare_resolved_environment(
            graph,
            runtime,
            destination,
            startup,
            Default::default(),
            deadline()
        )
        .await
        .unwrap_err(),
        ResolvedBuildError::TargetMismatch
    );
    fixture.assert_unchanged(image);
}

#[tokio::test]
async fn actual_existing_pins_and_source_receipts_survive_cache_removal_without_reinspection() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock_existing(&runtime);
    let image = fixture.image();
    let graph = fixture
        .closed(&root, runtime.target(), &["demo-pkg==1.0"])
        .await;
    assert!(root.model().is_some());
    let requests = fixture.requests();
    fixture.remove_cache();
    let result = prepare_resolved_environment(
        graph,
        runtime,
        destination,
        startup,
        Default::default(),
        deadline(),
    )
    .await
    .unwrap();
    assert_receipt(&result);
    assert_eq!(fixture.requests(), requests);
    fixture.assert_unchanged(image);
}

#[tokio::test]
async fn an_actual_project_image_changed_after_build_is_not_accepted_and_the_prefix_is_cleaned() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock();
    let graph = fixture
        .closed(&root, runtime.target(), &["demo-pkg==1.0"])
        .await;
    let path = fixture.lock_path();
    let prefix = destination.final_path().to_owned();
    let reached = Cell::new(false);
    let external = b"lockfile_version=4\n# actual late external writer\n";
    let error = prepare_with_finish(
        graph,
        runtime,
        destination,
        startup,
        Default::default(),
        deadline(),
        || {
            reached.set(true);
            fs::write(&path, external).unwrap();
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error, ResolvedBuildError::Revoked);
    assert!(reached.get());
    assert_eq!(fs::read(path).unwrap(), external);
    assert!(!prefix.exists());
}

#[tokio::test]
async fn zero_build_budget_fails_closed_and_cleans_only_the_unpublished_destination() {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = clt().await;
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture
        .closed(&root, runtime.target(), &["demo-pkg==1.0"])
        .await;
    let prefix = destination.final_path().to_owned();
    let limits = ResolvedBuildLimits {
        build: EnvironmentBuildLimits {
            max_archives: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        prepare_resolved_environment(graph, runtime, destination, startup, limits, deadline())
            .await
            .unwrap_err(),
        ResolvedBuildError::InvalidLimits
    );
    assert!(!prefix.exists());
    fixture.assert_unchanged(image);
}

async fn live(source: &Path, executable: &Path) {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = actual_runtime(source, executable).await;
    let fixture = RegistryBuildFixture::new(">=3");
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture
        .closed(&root, runtime.target(), &["demo-pkg==1.0"])
        .await;
    let result = prepare_resolved_environment(
        graph,
        runtime,
        destination,
        startup,
        Default::default(),
        deadline(),
    )
    .await
    .unwrap();
    assert_receipt(&result);
    let file = result
        .verification()
        .files()
        .iter()
        .find(|file| file.relative_path().ends_with("demo_pkg/__init__.py"))
        .unwrap();
    assert_eq!(
        fs::read(result.final_prefix().join(file.relative_path())).unwrap(),
        b"VALUE=42\n"
    );
    fixture.assert_unchanged(image);
}
#[tokio::test]
#[ignore = "explicit complete CLT ClosedGraph/Observed through controlled local build"]
async fn actual_clt_closed_graph_resolved_build() {
    live(Path::new(CLT_ROOT), Path::new(CLT_EXE)).await;
}
#[tokio::test]
#[ignore = "explicit complete owned311 ClosedGraph/Observed through controlled local build"]
async fn actual_owned311_closed_graph_resolved_build() {
    live(
        Path::new("/private/tmp/pakro-d3-owned-cpython311-2b8nnsx3"),
        Path::new("bin/python3.11"),
    )
    .await;
}
#[tokio::test]
#[ignore = "explicit complete PyPy ClosedGraph/Observed through controlled local build"]
async fn actual_pypy_closed_graph_resolved_build() {
    live(
        Path::new("/private/tmp/pakro-python-live/data/installs/python/pypy3.12-8.0.1"),
        Path::new("bin/pypy3.12"),
    )
    .await;
}
