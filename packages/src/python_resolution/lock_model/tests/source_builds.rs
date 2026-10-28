use super::*;
use crate::packages::python_environment::model::{
    SourceBuildInput, SourceBuildObservation, SourceBuildRecord,
};

fn receipt(fixture: &Fixture) -> (PinId, SourceBuildRecord) {
    let pin = &fixture.pins[1];
    let input = &fixture.pins[0];
    let input = SourceBuildInput {
        name: input.file().project().into(),
        version: input.file().version().into(),
        source_pin: hex::encode(input.id().as_bytes()),
        registry: input.file().registry_id().into(),
        base: *input.file().canonical_base_fingerprint(),
        source_filename: input.file().filename().into(),
        source_digest: input.digest().to_string(),
        source_size: input.size(),
        source_is_sdist: false,
        advertised: input.evidence() == PinEvidence::Advertised,
        output_filename: input.file().filename().into(),
        output_digest: input.digest().to_string(),
        output_size: input.size(),
    };
    (
        pin.id(),
        SourceBuildRecord {
            source_digest: pin.digest().to_string(),
            request_id: [7; 32],
            recipe: 1,
            backend: "setuptools.build_meta".into(),
            inputs: vec![input.clone()],
            output_filename: "dep-2.0-py3-none-any.whl".into(),
            output_digest: Digest::Sha256([8; 32]).to_string(),
            output_size: 2048,
            metadata_sha256: [9; 32],
            observation: Some(SourceBuildObservation {
                initial_requirements: vec!["demo-pkg==1.0.0".into()],
                initial_inputs: vec![input],
                dynamic_requirements: vec![],
            }),
            native: None,
        },
    )
}

#[test]
fn p2_source_lock_roundtrip_preserves_original_input_and_actual_output() {
    let fixture = Fixture::new();
    let receipt = receipt(&fixture);
    let model = PythonLockModel::from_parts_with_builds(
        &fixture.pins,
        &[fixture.input()],
        std::slice::from_ref(&receipt),
        Default::default(),
    )
    .unwrap();
    let encoded = model.encode_projection(Default::default()).unwrap();
    assert!(encoded.contains("schema = 3"));
    let decoded = PythonLockModel::decode_projection(&encoded, Default::default()).unwrap();
    assert_eq!(decoded.source_build(receipt.0), Some(&receipt.1));
    assert_eq!(decoded, model);
    assert_ne!(
        receipt.1.request_id, [8; 32],
        "input request is distinct from actual output digest"
    );
}

#[test]
fn p2_source_lock_rejects_wrong_source_output_and_missing_observation() {
    let fixture = Fixture::new();
    for case in 0..3 {
        let mut receipt = receipt(&fixture);
        match case {
            0 => receipt.1.source_digest = Digest::Sha256([0; 32]).to_string(),
            1 => receipt.1.output_filename = "another-2.0-py3-none-any.whl".into(),
            _ => receipt.1.observation = None,
        }
        assert!(
            PythonLockModel::from_parts_with_builds(
                &fixture.pins,
                &[fixture.input()],
                &[receipt],
                Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn p2_source_lock_applies_build_collection_and_text_budgets_before_decode() {
    let fixture = Fixture::new();
    let receipt = receipt(&fixture);
    let model = PythonLockModel::from_parts_with_builds(
        &fixture.pins,
        &[fixture.input()],
        &[receipt],
        Default::default(),
    )
    .unwrap();
    let encoded = model.encode_projection(Default::default()).unwrap();
    let limits = LockModelLimits {
        max_total_references: 6,
        ..Default::default()
    };
    assert!(PythonLockModel::decode_projection(&encoded, limits).is_err());
    let malicious = encoded.replace(
        "[[python.builds]]",
        "[[python.builds]]\nunsupported = 'private-secret-marker'",
    );
    let error = PythonLockModel::decode_projection(&malicious, Default::default()).unwrap_err();
    assert!(!error.to_string().contains("private-secret-marker"));
}

#[test]
fn p2_declaration_commitment_roundtrips_without_rewriting_wheel_identity() {
    let mut fixture = Fixture::new();
    fixture.pins.truncate(1);
    fixture.nodes.truncate(1);
    fixture.edges.clear();
    let model = fixture
        .model(Default::default())
        .unwrap()
        .with_declaration_fingerprint([42; 32]);
    let encoded = model.encode_projection(Default::default()).unwrap();
    assert!(encoded.contains("schema = 3"));
    assert!(encoded.contains("builds = []"));
    let decoded = PythonLockModel::decode_projection(&encoded, Default::default()).unwrap();
    assert_eq!(decoded.declaration_fingerprint(), Some(&[42; 32]));
    assert_eq!(decoded.pins(), model.pins());
    assert_eq!(decoded.graphs(), model.graphs());
    assert!(
        PythonLockModel::decode_projection(
            &encoded.replace("schema = 3", "schema = 2"),
            Default::default()
        )
        .is_err()
    );
}

#[test]
fn p6_native_build_lock_roundtrip_requires_schema4_and_closed_native_json() {
    use crate::packages::python_environment::model::NativeToolchainRecord;
    let fixture = Fixture::new();
    let mut receipt = receipt(&fixture);
    receipt.1.recipe = 2;
    receipt.1.native = Some(NativeToolchainRecord::test_record());
    let model = PythonLockModel::from_parts_with_builds(
        &fixture.pins,
        &[fixture.input()],
        std::slice::from_ref(&receipt),
        Default::default(),
    ).unwrap();
    let encoded = model.encode_projection(Default::default()).unwrap();
    assert!(encoded.contains("schema = 4\n") && encoded.contains("native = \""));
    let decoded = PythonLockModel::decode_projection(&encoded, Default::default()).unwrap();
    assert_eq!(decoded, model);
    assert_eq!(decoded.encode_projection(Default::default()).unwrap(), encoded);
    for schema in [2, 3] {
        assert_eq!(
            PythonLockModel::decode_projection(&encoded.replacen("schema = 4", &format!("schema = {schema}"), 1), Default::default()),
            Err(LockModelError::UnsupportedSchema),
        );
    }
    let raw = receipt.1.native.as_ref().unwrap().canonical_json().unwrap();
    let unknown = raw.replacen("\"schema\":1", "\"schema\":1,\"unknown\":true", 1);
    let malicious = encoded.replace(&serde_json::to_string(&raw).unwrap(), &serde_json::to_string(&unknown).unwrap());
    assert_ne!(malicious, encoded);
    assert!(PythonLockModel::decode_projection(&malicious, Default::default()).is_err());
    let limits = LockModelLimits { max_declaration_bytes: raw.len() - 1, ..Default::default() };
    assert!(PythonLockModel::decode_projection(&encoded, limits).is_err());
    receipt.1.recipe = 1;
    assert!(receipt.1.validate().is_err());
    receipt.1.recipe = 2;
    receipt.1.native = None;
    assert!(receipt.1.validate().is_err());
}
