use super::*;
use crate::packages::python_resolution::source::registry_depot::ProjectRegistryFixture;
use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};

#[tokio::test]
async fn f3_exact_roots_preserve_real_dependency_edges() {
    let directory = tempfile::tempdir().unwrap();
    let stage = directory.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(30),
    );
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    assert!(
        resolve_registry_roots(
            &context,
            &root,
            &[("demo-pkg", ">=1.0")],
            fixture.context(&endpoints, &policy),
            None,
        )
        .await
        .is_err()
    );
    assert!(
        fixture.requests().is_empty(),
        "nonexact requests must fail before lookup"
    );
    // 1.0 is a complete published package version. Packages do not require
    // Python's three-component language version spelling.
    let closed = resolve_registry_roots(
        &context,
        &root,
        &[("demo-pkg", "1.0")],
        fixture.context(&endpoints, &policy),
        None,
    )
    .await
    .unwrap();
    assert_eq!(closed.roots()[0].version(), "1.0");
    let projection = closed.projection(&[[11; 32], [12; 32]]).unwrap();
    assert_eq!(projection.nodes.len(), 2);
    assert_eq!(projection.edges.len(), 1);
    let edge = &projection.edges[0];
    assert_eq!(projection.nodes[edge.from].name, "demo-pkg");
    assert_eq!(projection.nodes[edge.to].name, "dep");
    assert_eq!(edge.declaration, "dep==1.0");
    assert!(
        projection
            .nodes
            .iter()
            .all(|node| node.source_pin.len() == 64)
    );
    let (archives, evidence) = closed.into_sources().unwrap();
    assert_eq!(archives.len(), 2);
    evidence
        .recheck(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
        .unwrap();
    fixture.no_publish();
}

#[tokio::test]
async fn f3_catalog_rejects_changed_graph_roots_and_edge_declarations() {
    use crate::packages::python_environment::model::{
        CatalogCodecLimits, CatalogModel, GenerationRecord, InputKind, InputReference, ModelLimits,
    };
    use pakro_store::Digest;
    let directory = tempfile::tempdir().unwrap();
    let stage = directory.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(30),
    );
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let closed = resolve_registry_roots(
        &context,
        &root,
        &[("demo-pkg", "1.0")],
        fixture.context(&endpoints, &policy),
        None,
    )
    .await
    .unwrap();
    let graph = closed.projection(&[[11; 32], [12; 32]]).unwrap();
    let fingerprint = closed.fingerprint().unwrap();
    let record = GenerationRecord::new_shared(
        &format!("g-{:032x}", 1),
        [3; 32],
        fingerprint,
        [5; 32],
        [6; 32],
        vec![
            InputReference::new(InputKind::Language, Digest::Sha256([10; 32])),
            InputReference::new(InputKind::Package, Digest::Sha256([11; 32])),
            InputReference::new(InputKind::Package, Digest::Sha256([12; 32])),
        ],
        ModelLimits::default(),
    )
    .unwrap()
    .with_package_graph(graph.clone(), ModelLimits::default())
    .unwrap();
    let mut model = CatalogModel::new(ModelLimits::default()).unwrap();
    model.register_shared_generation(record).unwrap();
    let bytes = model.encode_catalog(CatalogCodecLimits::default()).unwrap();
    CatalogModel::decode_catalog(&bytes, CatalogCodecLimits::default()).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for change_roots in [true, false] {
        let mut wire = original.clone();
        let persisted = &mut wire["generations"][0]["package_graph"];
        if change_roots {
            persisted["roots"] = serde_json::json!([graph.roots[0], graph.edges[0].to]);
        } else {
            persisted["edges"][0]["declaration"] = serde_json::json!("dep>=2.0");
        }
        assert!(
            CatalogModel::decode_catalog(
                &serde_json::to_vec(&wire).unwrap(),
                CatalogCodecLimits::default()
            )
            .is_err(),
            "changed roots={change_roots} or dependency declarations cannot retain the original graph identity"
        );
    }
    fixture.no_publish();
}

#[tokio::test]
async fn fresh_wheel_closure_provides_real_urls_for_schema7_lock() {
    use crate::packages::python_resolution::lock_model::{
        EdgeInput, GroupKey, LockGraphInput, LockMarkerSemantics, NodeInput, PythonLockModel,
        RootInput, RuntimePin,
    };
    let directory = tempfile::tempdir().unwrap();
    let stage = directory.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(30),
    );
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let closed = resolve_registry_roots(
        &context,
        &root,
        &[("demo-pkg", "1.0")],
        fixture.context(&endpoints, &policy),
        None,
    )
    .await
    .unwrap();
    let projection = closed.projection(&[[11; 32], [12; 32]]).unwrap();
    let pins = projection
        .nodes
        .iter()
        .map(|node| node.content_pin().unwrap())
        .collect::<Vec<_>>();
    let nodes = projection
        .nodes
        .iter()
        .zip(&pins)
        .map(|(node, pin)| NodeInput {
            name: &node.name,
            version: &node.version,
            source_pin: pin.id(),
        })
        .collect::<Vec<_>>();
    let edges = projection
        .edges
        .iter()
        .map(|edge| EdgeInput {
            from: &projection.nodes[edge.from].name,
            to: &projection.nodes[edge.to].name,
            declaration: &edge.declaration,
        })
        .collect::<Vec<_>>();
    let key = GroupKey::new("", "main", [4; 32], Default::default()).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo-pkg",
    }];
    let input = LockGraphInput {
        key: &key,
        semantics: LockMarkerSemantics::Current2026,
        runtime: RuntimePin::new([5; 32], 1, [4; 32]).unwrap(),
        root_declarations: &["demo-pkg==1.0"],
        roots: &roots,
        nodes: &nodes,
        edges: &edges,
    };
    let model = PythonLockModel::from_parts(&pins, &[input], Default::default()).unwrap();
    let urls = closed.locators_for_lock(&root, &model).unwrap();
    let model = model.with_locators(urls, Default::default()).unwrap();
    let wire = model.encode_projection(Default::default()).unwrap();
    assert!(wire.contains("schema = 7"));
    for pin in model.pins() {
        assert!(
            model
                .locator(pin.id())
                .unwrap()
                .ends_with(pin.file().filename())
        );
    }
    assert_eq!(
        PythonLockModel::decode_projection(&wire, Default::default()).unwrap(),
        model
    );
}
