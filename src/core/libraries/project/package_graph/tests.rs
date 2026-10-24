use super::*;
use crate::packages::python_environment::model::{
    PackageGraphNode, PackageGraphRecord, PackageSourcePin,
};
use crate::packages::python_resolution::lock_model::*;
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use std::time::{Duration, Instant};

fn pin(digest: u8) -> ContentPin {
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "corp",
            canonical_base_fingerprint: [7; 32],
            project: "demo",
            version: "1.0",
            filename: "demo-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        Default::default(),
    )
    .unwrap();
    ContentPin::new(
        &file,
        Digest::Sha256([digest; 32]),
        100,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap()
}
fn document(pin: &ContentPin) -> String {
    let key = GroupKey::new("pakro.toml", "default", [3; 32], Default::default()).unwrap();
    let runtime = RuntimePin::new([8; 32], 1, [3; 32]).unwrap();
    let model = PythonLockModel::from_parts(
        &[pin.clone()],
        &[LockGraphInput {
            key: &key,
            semantics: LockMarkerSemantics::Current2026,
            runtime,
            root_declarations: &["demo==1.0"],
            roots: &[RootInput {
                declaration_index: 0,
                node: "demo",
            }],
            nodes: &[NodeInput {
                name: "demo",
                version: "1.0",
                source_pin: pin.id(),
            }],
            edges: &[],
        }],
        Default::default(),
    )
    .unwrap();
    format!(
        "lockfile_version = 4\n{}",
        model.encode_projection(Default::default()).unwrap()
    )
}
fn observe(
    project: &std::path::Path,
) -> crate::libraries::project::snapshot::ObservedProjectInputs {
    let mut discovery = crate::config::FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    crate::libraries::project::snapshot::observe_project(
        project,
        crate::libraries::project::snapshot::ReadPolicy::new(
            discovery,
            Instant::now() + Duration::from_secs(30),
        ),
    )
    .unwrap()
}
#[test]
fn f3_existing_graph_checks_source_and_observed_known_pins() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let source = pin(11);
    let graph = CommittedGraph {
        fingerprint: [9; 32],
        graph: PackageGraphRecord {
            roots: vec![0],
            nodes: vec![PackageGraphNode {
                name: "demo".into(),
                version: "1.0".into(),
                instance: [4; 32],
                source_pin: hex::encode(source.id().as_bytes()),
                artifact: source.digest().to_string(),
                pin: PackageSourcePin {
                    registry: "corp".into(),
                    base: [7; 32],
                    filename: "demo-1.0-py3-none-any.whl".into(),
                    size: 100,
                    advertised: true,
                    build: None,
                },
            }],
            edges: vec![],
        },
        layout: [5; 32],
        language: [6; 32],
        allocation: format!("g-{:032x}", 1),
    };
    let expected = document(&source);
    let path = project.join("pakro.lock");
    std::fs::write(&path, &expected).unwrap();
    let observed = observe(&project);
    graph
        .validate_observed_pins(&observed, &[("corp", [7; 32])])
        .unwrap();
    let mismatch = graph
        .validate_observed_pins(&observed, &[("pypi", [7; 32])])
        .unwrap_err();
    assert!(mismatch.downcast_ref::<GraphPinMismatch>().is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(
        std::fs::read_dir(&project).unwrap().count(),
        2,
        "readonly validation must not create sidecar locks"
    );
    drop(observed);
    std::fs::write(&path, document(&pin(12))).unwrap();
    let observed = observe(&project);
    assert!(
        graph
            .validate_observed_pins(&observed, &[("corp", [7; 32])])
            .is_err(),
        "a known digest conflict cannot be replaced by an existing local layout"
    );
    let bound = BoundPythonLock::acquire(
        &path,
        Default::default(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let mismatch = graph
        .validate_pins(&bound, &[("corp", [7; 32])])
        .unwrap_err();
    assert!(mismatch.downcast_ref::<GraphPinMismatch>().is_some());
    drop(bound);
    drop(observed);
    std::fs::write(&path, "lockfile_version=4\n[python]\nschema='damaged'\n").unwrap();
    let observed = observe(&project);
    let damaged = graph
        .validate_observed_pins(&observed, &[("corp", [7; 32])])
        .unwrap_err();
    let damaged_other_scope = graph
        .validate_observed_pins(&observed, &[("pypi", [9; 32])])
        .unwrap_err();
    assert!(
        damaged_other_scope
            .downcast_ref::<GraphPinMismatch>()
            .is_none()
    );
    assert!(
        damaged.downcast_ref::<GraphPinMismatch>().is_none(),
        "damaged lock must never be filtered as a different cached candidate"
    );
}
