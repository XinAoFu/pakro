use super::*;

fn built_node_wire() -> serde_json::Value {
    use crate::packages::python_resolution::lock_model::{
        ContentPin, PinEvidence, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    };
    let source = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [7; 32],
            project: "demo",
            version: "1.0",
            filename: "demo-1.0.tar.gz",
            kind: RegistryArtifactKind::Sdist,
        },
        Default::default(),
    )
    .unwrap();
    let source_digest = Digest::Sha256([71; 32]);
    let pin = ContentPin::new(
        &source,
        source_digest,
        100,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap();
    let input = package_node("setuptools", "80.9.0", 51, 61);
    serde_json::json!({
        "name": "demo", "version": "1.0", "instance": vec![21u8;32],
        "source_pin": hex::encode(pin.id().as_bytes()),
        "artifact": Digest::Sha256([31;32]).to_string(),
        "pin": {
            "registry":"pypi", "base":vec![7u8;32], "filename":"demo-1.0.tar.gz",
            "size":100, "advertised":true,
            "build": {
                "source_digest":source_digest.to_string(), "request_id":vec![8u8;32],
                "recipe":1, "backend":"setuptools.build_meta:__legacy__",
                "inputs":[{
                    "name":input.name, "version":input.version, "source_pin":input.source_pin,
                    "registry":input.pin.registry, "base":input.pin.base,
                    "source_filename":input.pin.filename,
                    "source_digest":input.artifact, "source_size":input.pin.size,
                    "source_is_sdist":false, "advertised":true,
                    "output_filename":"setuptools-80.9.0-py3-none-any.whl",
                    "output_digest":Digest::Sha256([61;32]).to_string(), "output_size":100
                }],
                "output_filename":"demo-1.0-py3-none-any.whl",
                "output_digest":Digest::Sha256([31;32]).to_string(), "output_size":200,
                "metadata_sha256":vec![9u8;32]
            }
        }
    })
}
fn built_graph() -> PackageGraphRecord {
    let node = serde_json::from_value(built_node_wire()).unwrap();
    PackageGraphRecord {
        roots: vec![0],
        nodes: vec![node],
        edges: vec![],
    }
}

#[test]
fn s2_source_build_keeps_the_original_sdist_pin_and_actual_wheel_output() {
    use crate::packages::python_resolution::lock_model::RegistryArtifactKind;
    let graph = built_graph();
    graph.validate(ModelLimits::default()).unwrap();
    let node = &graph.nodes[0];
    let pin = node.content_pin().unwrap();
    assert_eq!(pin.file().kind(), RegistryArtifactKind::Sdist);
    assert_eq!(pin.file().filename(), "demo-1.0.tar.gz");
    assert_eq!(pin.digest(), Digest::Sha256([71; 32]));
    assert_eq!(node.artifact, Digest::Sha256([31; 32]).to_string());
    assert_ne!(pin.digest().to_string(), node.artifact);
    let record = GenerationRecord::new_shared(
        &allocation(701),
        [3; 32],
        graph.fingerprint().unwrap(),
        [5; 32],
        [6; 32],
        vec![
            reference(InputKind::Language, 20),
            reference(InputKind::Package, 21),
        ],
        ModelLimits::default(),
    )
    .unwrap()
    .with_package_graph(graph.clone(), ModelLimits::default())
    .unwrap();
    let mut state = model();
    state.register_shared_generation(record).unwrap();
    let bytes = state.encode_catalog(CatalogCodecLimits::default()).unwrap();
    let restored = CatalogModel::decode_catalog(&bytes, CatalogCodecLimits::default()).unwrap();
    assert_eq!(
        restored
            .generation(&allocation(701))
            .unwrap()
            .package_graph(),
        Some(&graph)
    );
}

#[test]
fn s2_source_build_rejects_output_and_exact_build_input_pin_mismatches() {
    let valid = built_graph();
    valid.validate(ModelLimits::default()).unwrap();
    for field in [
        "output_digest",
        "output_filename",
        "source_digest",
        "input_pin",
    ] {
        let mut wire = built_node_wire();
        let build = &mut wire["pin"]["build"];
        match field {
            "output_digest" => {
                build[field] = serde_json::json!(Digest::Sha256([99; 32]).to_string())
            }
            "output_filename" => build[field] = serde_json::json!("other-1.0-py3-none-any.whl"),
            "source_digest" => {
                build[field] = serde_json::json!(Digest::Sha256([99; 32]).to_string())
            }
            _ => build["inputs"][0]["source_pin"] = serde_json::json!("00".repeat(32)),
        }
        let node: PackageGraphNode = serde_json::from_value(wire).unwrap();
        assert!(
            PackageGraphRecord {
                roots: vec![0],
                nodes: vec![node],
                edges: vec![]
            }
            .validate(ModelLimits::default())
            .is_err(),
            "{field}"
        );
    }
}

#[test]
fn s2_source_build_fingerprint_tracks_outputs_but_not_advertised_evidence() {
    let first = built_graph();
    first.validate(ModelLimits::default()).unwrap();
    let mut wire = built_node_wire();
    wire["pin"]["advertised"] = serde_json::json!(false);
    wire["pin"]["build"]["inputs"][0]["advertised"] = serde_json::json!(false);
    let second = PackageGraphRecord {
        roots: vec![0],
        nodes: vec![serde_json::from_value(wire.clone()).unwrap()],
        edges: vec![],
    };
    assert_eq!(first.fingerprint().unwrap(), second.fingerprint().unwrap());
    wire["artifact"] = serde_json::json!(Digest::Sha256([32; 32]).to_string());
    wire["pin"]["build"]["output_digest"] = wire["artifact"].clone();
    let different = PackageGraphRecord {
        roots: vec![0],
        nodes: vec![serde_json::from_value(wire).unwrap()],
        edges: vec![],
    };
    different.validate(ModelLimits::default()).unwrap();
    assert_ne!(
        first.fingerprint().unwrap(),
        different.fingerprint().unwrap()
    );
}

#[test]
fn s2_wheel_only_source_pin_keeps_its_old_serialized_shape() {
    let node = package_node("demo", "1.0", 21, 31);
    let wire = serde_json::to_value(&node.pin).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 5);
    assert!(wire.get("build").is_none());
    node.content_pin().unwrap();
}

#[test]
fn s2_source_build_normalizes_the_generated_wheel_distribution_name() {
    use crate::packages::python_resolution::lock_model::{
        ContentPin, PinEvidence, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    };
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [7; 32],
            project: "demo-pkg",
            version: "1.0",
            filename: "demo_pkg-1.0.tar.gz",
            kind: RegistryArtifactKind::Sdist,
        },
        Default::default(),
    )
    .unwrap();
    let pin = ContentPin::new(
        &file,
        Digest::Sha256([71; 32]),
        100,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap();
    let mut wire = built_node_wire();
    wire["name"] = serde_json::json!("demo-pkg");
    wire["source_pin"] = serde_json::json!(hex::encode(pin.id().as_bytes()));
    wire["pin"]["filename"] = serde_json::json!("demo_pkg-1.0.tar.gz");
    wire["pin"]["build"]["output_filename"] = serde_json::json!("demo_pkg-1.0-py3-none-any.whl");
    let node: PackageGraphNode = serde_json::from_value(wire).unwrap();
    node.content_pin().unwrap();
}
