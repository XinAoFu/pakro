use super::*;
use crate::packages::python_environment::resolved_build::{ActualRuntime, actual_clt_inputs};
use crate::packages::python_resolution::lock_model::{
    ContentPin, LockGraphInput, LockLimit, LockMarkerSemantics, NodeInput, PinEvidence,
    RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput, RootInput, RuntimePin,
};
use crate::packages::python_resolution::source::registry_depot::RegistryBuildFixture;
use pakro_index::python_markers::PythonMarkerScope;
use pakro_store::Digest;
use std::cell::Cell;
use std::fs;
use std::time::Duration;

fn request() -> Instant {
    Instant::now() + Duration::from_secs(240)
}
fn key(prepared: &PreparedResolvedEnvironment<'_>, group: &str) -> GroupKey {
    GroupKey::new(
        "",
        group,
        *prepared.target().fingerprint(),
        Default::default(),
    )
    .unwrap()
}
async fn build<'lock>(
    actual: ActualRuntime,
    graph: ClosedRegistryGraph<'lock>,
    deadline: Instant,
) -> (tempfile::TempDir, PreparedResolvedEnvironment<'lock>) {
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = actual;
    let prepared = prepare_resolved_environment(
        graph,
        runtime,
        destination,
        startup,
        Default::default(),
        deadline,
    )
    .await
    .expect("actual Closed/Observed/D1-D4/seal prerequisite");
    (_owner, prepared)
}
fn old_pin() -> ContentPin {
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "old-registry",
            canonical_base_fingerprint: [91; 32],
            project: "old-only",
            version: "1.0",
            filename: "old_only-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        Default::default(),
    )
    .unwrap();
    // Portable old-lock input only; no byte/Source capability is constructed.
    ContentPin::new(
        &file,
        Digest::Sha256([92; 32]),
        7,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn actual_runtime_and_original_raw_root_and_m1_indices_are_projected() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let content = *actual.runtime.content_tree_sha256();
    let fixture = RegistryBuildFixture::with_dependencies(
        ">=3",
        &[
            "dep == 1.0 ; python_version >= '3'",
            "dep == 1.0 ; python_version >= '3'",
        ],
        d.into_std(),
    );
    let root = fixture.lock();
    let raws = [
        "inactive ; python_version < '2'",
        " Demo_Pkg == 1.0 ; python_version >= '3' ",
        "demo-pkg==1.0",
    ];
    let graph = fixture.closed(&root, actual.runtime.target(), &raws).await;
    assert_eq!(graph.checked().roots.len(), 2);
    assert_eq!(graph.checked().edges.len(), 2);
    let (_owner, prepared) = build(actual, graph, d).await;
    let k = key(&prepared, "main");
    let model = prepared
        .merged_lock_model(&k, Default::default(), d)
        .unwrap();
    let group = model.graph(&k).unwrap();
    assert_eq!(group.runtime().content_tree(), &content);
    assert_eq!(group.runtime().recipe(), 1);
    assert_eq!(group.runtime().target(), prepared.target().fingerprint());
    assert_eq!(group.root_declarations(), raws);
    assert_eq!(
        group
            .roots()
            .iter()
            .map(|r| r.declaration_index())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(group.edges().len(), 2);
    for edge in group.edges() {
        assert_eq!(edge.declaration(), "dep == 1.0 ; python_version >= '3'");
    }
    for selected in &prepared.plan().selected {
        assert!(
            model
                .pins()
                .iter()
                .any(|pin| Some(pin) == prepared.source_pin(selected.key))
        );
    }
    assert!(root.model().is_none());
    fixture.assert_unchanged(None);
}

#[tokio::test]
async fn actual_old_other_groups_shared_pins_and_replaced_only_pin_gc_are_preserved() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let fixture = RegistryBuildFixture::with_dependencies(">=3", &["dep==1.0"], d.into_std());
    let seed = fixture.lock_existing(&actual.runtime);
    let original = seed.model().unwrap().clone();
    drop(seed);
    let k = GroupKey::new(
        "",
        "replace",
        *actual.runtime.target().fingerprint(),
        Default::default(),
    )
    .unwrap();
    let pin = old_pin();
    let old_input = LockGraphInput {
        key: &k,
        semantics: LockMarkerSemantics::Current2026,
        runtime: RuntimePin::new([77; 32], 1, *k.target()).unwrap(),
        root_declarations: &["old-only==1.0"],
        roots: &[RootInput {
            declaration_index: 0,
            node: "old-only",
        }],
        nodes: &[NodeInput {
            name: "old-only",
            version: "1.0",
            source_pin: pin.id(),
        }],
        edges: &[],
    };
    let old = original
        .replace_group(&[pin.clone()], &old_input, Default::default())
        .unwrap();
    fs::write(
        fixture.lock_path(),
        format!(
            "lockfile_version=4\n{}",
            old.encode_projection(Default::default()).unwrap()
        ),
    )
    .unwrap();
    let root = fixture.lock();
    let image = fixture.image();
    let graph = fixture
        .closed(&root, actual.runtime.target(), &["demo-pkg==1.0"])
        .await;
    let (_owner, prepared) = build(actual, graph, d).await;
    let merged = prepared
        .merged_lock_model(&k, Default::default(), d)
        .unwrap();
    assert_eq!(merged.graphs().len(), 2);
    assert_eq!(
        merged.graph(original.graphs()[0].key()),
        Some(&original.graphs()[0])
    );
    assert_eq!(merged.pins(), original.pins());
    assert!(!merged.pins().iter().any(|p| p.id() == pin.id()));
    assert_eq!(
        merged.graph(&k).unwrap().runtime().content_tree(),
        prepared.runtime_content_tree_sha256()
    );
    fixture.assert_unchanged(image);
}

#[tokio::test]
async fn caller_target_data_cannot_replace_the_actual_prepared_runtime() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let fixture = RegistryBuildFixture::with_dependencies(">=3", &["dep==1.0"], d.into_std());
    let root = fixture.lock();
    let graph = fixture
        .closed(&root, actual.runtime.target(), &["demo-pkg==1.0"])
        .await;
    let (_owner, prepared) = build(actual, graph, d).await;
    let wrong = GroupKey::new("", "main", [255; 32], Default::default()).unwrap();
    assert_ne!(wrong.target(), prepared.target().fingerprint());
    assert_eq!(
        prepared
            .merged_lock_model(&wrong, Default::default(), d)
            .unwrap_err(),
        ResolvedProjectionError::TargetMismatch
    );
    fixture.assert_unchanged(None);
}

#[tokio::test]
async fn zero_and_tighter_output_limits_fail_without_lock_or_environment_writes() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let fixture = RegistryBuildFixture::with_dependencies(">=3", &["dep==1.0"], d.into_std());
    let root = fixture.lock();
    let graph = fixture
        .closed(&root, actual.runtime.target(), &["demo-pkg==1.0"])
        .await;
    let (_owner, prepared) = build(actual, graph, d).await;
    let k = key(&prepared, "main");
    let before = prepared.verification().inventory_sha256();
    let mut zero = LockModelLimits::default();
    zero.max_groups = 0;
    assert_eq!(
        prepared.merged_lock_model(&k, zero, d).unwrap_err(),
        ResolvedProjectionError::InvalidLimits
    );
    let limits = LockModelLimits {
        max_nodes_per_group: 1,
        ..Default::default()
    };
    assert_eq!(
        prepared.merged_lock_model(&k, limits, d).unwrap_err(),
        ResolvedProjectionError::Model(LockModelError::Limit(LockLimit::Nodes))
    );
    assert_eq!(prepared.verification().inventory_sha256(), before);
    prepared.recheck_current().unwrap();
    fixture.assert_unchanged(None);
}

#[tokio::test]
async fn a_late_original_image_change_rejects_projection_and_preserves_external_bytes() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let fixture = RegistryBuildFixture::with_dependencies(">=3", &["dep==1.0"], d.into_std());
    let root = fixture.lock();
    let graph = fixture
        .closed(&root, actual.runtime.target(), &["demo-pkg==1.0"])
        .await;
    let (_owner, prepared) = build(actual, graph, d).await;
    let k = key(&prepared, "main");
    let path = fixture.lock_path();
    let prefix = prepared.final_prefix().to_owned();
    let reached = Cell::new(false);
    let external = b"lockfile_version=4\n# late projection writer\n";
    let error = project_with_finish(&prepared, &k, Default::default(), d, || {
        reached.set(true);
        fs::write(&path, external).unwrap();
    })
    .unwrap_err();
    assert_eq!(error, ResolvedProjectionError::Revoked);
    assert!(reached.get());
    assert_eq!(fs::read(&path).unwrap(), external);
    drop(prepared);
    assert!(!prefix.exists());
    assert_eq!(fs::read(path).unwrap(), external);
}

#[tokio::test]
async fn copied_runtime_bytes_are_rechecked_before_runtime_pin_projection() {
    let d = request();
    let actual = actual_clt_inputs(PythonMarkerScope::Environment, d).await;
    let entrypoint = actual.runtime.entrypoint().to_owned();
    let fixture = RegistryBuildFixture::with_dependencies(">=3", &["dep==1.0"], d.into_std());
    let root = fixture.lock();
    let graph = fixture
        .closed(&root, actual.runtime.target(), &["demo-pkg==1.0"])
        .await;
    let (_owner, prepared) = build(actual, graph, d).await;
    let k = key(&prepared, "main");
    let original = fs::read(&entrypoint).unwrap();
    fs::write(&entrypoint, b"changed private copy only").unwrap();
    let error = prepared
        .merged_lock_model(&k, Default::default(), d)
        .unwrap_err();
    fs::write(&entrypoint, &original).unwrap();
    assert_eq!(error, ResolvedProjectionError::Runtime);
    prepared.recheck_current().unwrap();
    fixture.assert_unchanged(None);
}
