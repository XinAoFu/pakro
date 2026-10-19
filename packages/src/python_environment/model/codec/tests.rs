use super::*;
use serde_json::{Value, json};

fn key_json() -> Value {
    json!({"project": Uuid::from_u128(1).to_string(), "group": "开发 main", "target": "03".repeat(32)})
}
fn allocation() -> String {
    format!("g-{:032x}", 1)
}
fn transaction() -> String {
    Uuid::from_u128(101).to_string()
}
fn golden() -> Value {
    json!({
        "schema": 3,
        "generations": [{
            "allocation": allocation(), "target": "03".repeat(32), "shared_layout": null,
            "graph": "04".repeat(32), "receipt": "05".repeat(32),
            "inputs": [
                {"kind": "runtime", "digest": format!("sha512-{}", "02".repeat(64))},
                {"kind": "artifact", "digest": format!("sha256-{}", "01".repeat(32))}
            ]
        }],
        "selections": [{"key": key_json(), "stamp": {"revision":1, "generation":allocation(), "language":null, "transaction":transaction()}}],
        "transactions": [{"id": transaction(), "key": key_json(),
            "expected": {"revision":0,"generation":null,"language":null,"transaction":null}, "generation":allocation(), "language":null}],
        "projects": []
    })
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn owner() -> SelectionKey {
    SelectionKey::new(
        Uuid::from_u128(1),
        "开发 main",
        [3; 32],
        ModelLimits::default(),
    )
    .unwrap()
}
fn state() -> CatalogModel {
    let mut state = CatalogModel::new(ModelLimits::default()).unwrap();
    state
        .register_generation(
            GenerationRecord::new(
                &allocation(),
                *owner().target(),
                [4; 32],
                [5; 32],
                vec![
                    InputReference::new(InputKind::Artifact, Digest::Sha256([1; 32])),
                    InputReference::new(InputKind::Runtime, Digest::Sha512([2; 64])),
                ],
                ModelLimits::default(),
            )
            .unwrap(),
        )
        .unwrap();
    state
        .apply_selection(
            &owner(),
            &SelectionStamp::default(),
            Some(&allocation()),
            Uuid::from_u128(101),
        )
        .unwrap();
    state
}

#[test]
fn independent_catalog_golden_decodes_to_complete_active_facts() {
    let state =
        CatalogModel::decode_catalog(&bytes(&golden()), CatalogCodecLimits::default()).unwrap();
    let stamp = state.snapshot(&owner());
    assert_eq!(stamp.revision(), 1);
    assert_eq!(stamp.generation(), Some(allocation().as_str()));
    assert_eq!(stamp.transaction(), Uuid::from_u128(101));
    let record = state.generation(&allocation()).unwrap();
    assert_eq!(record.target(), owner().target());
    assert_eq!(record.graph(), &[4; 32]);
    assert_eq!(record.receipt(), &[5; 32]);
    assert_eq!(record.inputs().len(), 2);
    assert_eq!(state.protecting_references().len(), 2);
    assert_eq!(state.transactions.len(), 1);
}

#[test]
fn catalog_encoder_matches_independent_wire_facts_and_is_deterministic() {
    let state = state();
    let first = state.encode_catalog(CatalogCodecLimits::default()).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), golden());
    assert_eq!(
        state.encode_catalog(CatalogCodecLimits::default()).unwrap(),
        first
    );
    let restored = CatalogModel::decode_catalog(&first, CatalogCodecLimits::default()).unwrap();
    assert_eq!(
        restored
            .encode_catalog(CatalogCodecLimits::default())
            .unwrap(),
        first
    );
}

#[test]
fn invalid_schema_and_json_are_rejected_without_echoing_document_data() {
    let mut cases = vec![
        b"not JSON private-query-token".to_vec(),
        b"[]".to_vec(),
        b"{}".to_vec(),
        br#"{"schema":2,"schema":2,"generations":[],"selections":[],"transactions":[]}"#.to_vec(),
    ];
    let mut future = golden();
    future["schema"] = json!(4);
    cases.push(bytes(&future));
    let mut unknown = golden();
    unknown["private-query-token"] = json!("secret-reason");
    cases.push(bytes(&unknown));
    let mut trailing = bytes(&golden());
    trailing.extend_from_slice(b" {}");
    cases.push(trailing);
    let mut accepted = Vec::new();
    for (index, input) in cases.iter().enumerate() {
        match CatalogModel::decode_catalog(input, CatalogCodecLimits::default()) {
            Ok(_) => accepted.push(index),
            Err(error) => {
                let message = format!("{error:#}");
                assert!(
                    !message.contains("private-query-token") && !message.contains("secret-reason")
                );
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid catalog cases were accepted: {accepted:?}"
    );
}

#[test]
fn invalid_identities_active_targets_and_history_chains_cannot_be_restored() {
    let mut cases = Vec::new();
    let mut value = golden();
    value["generations"][0]["allocation"] = json!("../outside");
    cases.push(value);
    let mut value = golden();
    value["selections"][0]["key"]["project"] = json!(Uuid::nil().to_string());
    cases.push(value);
    let mut value = golden();
    value["generations"][0]["graph"] = json!("not-sha256");
    cases.push(value);
    let mut value = golden();
    value["generations"][0]["inputs"][0]["digest"] = json!("sha512-bad");
    cases.push(value);
    let mut value = golden();
    value["generations"] = json!([]);
    cases.push(value);
    let mut value = golden();
    value["generations"][0]["target"] = json!("09".repeat(32));
    cases.push(value);
    let mut value = golden();
    value["selections"][0]["stamp"]["revision"] = json!(0);
    cases.push(value);
    let mut value = golden();
    value["selections"][0]["stamp"]["transaction"] = json!(Uuid::from_u128(102).to_string());
    cases.push(value);
    let mut value = golden();
    value["transactions"][0]["expected"]["revision"] = json!(2);
    cases.push(value);
    let mut value = golden();
    value["transactions"][0]["expected"]["generation"] = json!(allocation());
    cases.push(value);
    let mut value = golden();
    let duplicate = value["transactions"][0].clone();
    value["transactions"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    cases.push(value);
    let mut accepted = Vec::new();
    for (index, input) in cases.iter().enumerate() {
        if CatalogModel::decode_catalog(&bytes(input), CatalogCodecLimits::default()).is_ok() {
            accepted.push(index);
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid catalog identity/state cases were accepted: {accepted:?}"
    );
}

#[test]
fn document_and_collection_budgets_are_enforced_before_success() {
    let empty = br#"{"schema":2,"generations":[],"selections":[],"transactions":[]}"#;
    let exact = CatalogCodecLimits {
        max_document_bytes: empty.len(),
        ..Default::default()
    };
    assert!(CatalogModel::decode_catalog(empty, exact).is_ok());
    assert!(
        CatalogModel::decode_catalog(
            empty,
            CatalogCodecLimits {
                max_document_bytes: empty.len() - 1,
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        CatalogModel::new(ModelLimits::default())
            .unwrap()
            .encode_catalog(CatalogCodecLimits {
                max_document_bytes: empty.len() - 1,
                ..exact
            })
            .is_err()
    );
    for limit in [0, 1] {
        assert!(
            state()
                .encode_catalog(CatalogCodecLimits {
                    max_document_bytes: limit,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            CatalogModel::decode_catalog(
                &bytes(&golden()),
                CatalogCodecLimits {
                    max_document_bytes: limit,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    let mut limits = CatalogCodecLimits::default();
    limits.model.max_refs_per_generation = 1;
    assert!(CatalogModel::decode_catalog(&bytes(&golden()), limits).is_err());
    let mut too_many = golden();
    let duplicate = too_many["generations"][0].clone();
    too_many["generations"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let mut limits = CatalogCodecLimits::default();
    limits.model.max_generation_records = 1;
    assert!(CatalogModel::decode_catalog(&bytes(&too_many), limits).is_err());
}

#[test]
fn tombstones_and_forgotten_retired_generations_keep_transaction_replay_facts() {
    let mut original = state();
    let old = original.snapshot(&owner());
    original
        .apply_selection(&owner(), &old, None, Uuid::from_u128(102))
        .unwrap();
    original.forget_retired(&allocation()).unwrap();
    let encoded = original
        .encode_catalog(CatalogCodecLimits::default())
        .unwrap();
    let mut restored =
        CatalogModel::decode_catalog(&encoded, CatalogCodecLimits::default()).unwrap();
    assert_eq!(restored.snapshot(&owner()).revision(), 2);
    assert_eq!(restored.snapshot(&owner()).generation(), None);
    assert!(restored.protecting_references().is_empty());
    assert_eq!(
        restored
            .apply_selection(&owner(), &old, None, Uuid::from_u128(102))
            .unwrap(),
        ApplyResult::AlreadyApplied
    );
    assert!(
        restored
            .apply_selection(
                &owner(),
                &SelectionStamp::default(),
                None,
                Uuid::from_u128(103)
            )
            .is_err()
    );
}

fn two_selection_golden() -> Value {
    let mut value = golden();
    let mut second_key = key_json();
    second_key["project"] = json!(Uuid::from_u128(2).to_string());
    second_key["group"] = json!("other");
    let second_allocation = format!("g-{:032x}", 2);
    let mut second_generation = value["generations"][0].clone();
    second_generation["allocation"] = json!(second_allocation);
    value["generations"]
        .as_array_mut()
        .unwrap()
        .push(second_generation);
    value["selections"].as_array_mut().unwrap().push(json!({"key":second_key,
        "stamp":{"revision":1,"generation":second_allocation,"language":null,"transaction":Uuid::from_u128(102).to_string()}}));
    value["transactions"].as_array_mut().unwrap().push(json!({"id":Uuid::from_u128(102).to_string(),
        "key":second_key,"expected":{"revision":0,"generation":null,"language":null,"transaction":null},"generation":second_allocation,"language":null}));
    value
}

#[test]
fn unordered_history_uses_revision_chains_instead_of_nonce_or_array_order() {
    let mut value = golden();
    let initial = Uuid::from_u128(500).to_string();
    let deletion = Uuid::from_u128(3).to_string();
    let final_id = Uuid::from_u128(250).to_string();
    value["transactions"] = json!([
        {"id":deletion,"key":key_json(),"expected":{"revision":1,"generation":allocation(),"transaction":initial},"generation":null},
        {"id":final_id,"key":key_json(),"expected":{"revision":2,"generation":null,"transaction":deletion},"generation":allocation()},
        {"id":initial,"key":key_json(),"expected":{"revision":0,"generation":null,"transaction":null},"generation":allocation()}
    ]);
    value["selections"][0]["stamp"] =
        json!({"revision":3,"generation":allocation(),"transaction":final_id});
    let mut restored =
        CatalogModel::decode_catalog(&bytes(&value), CatalogCodecLimits::default()).unwrap();
    assert_eq!(restored.snapshot(&owner()).revision(), 3);
    assert_eq!(
        restored.snapshot(&owner()).transaction(),
        Uuid::from_u128(250)
    );
    assert_eq!(
        restored
            .apply_selection(
                &owner(),
                &SelectionStamp::default(),
                Some(&allocation()),
                Uuid::from_u128(500)
            )
            .unwrap(),
        ApplyResult::AlreadyApplied
    );
    for variant in 0..3 {
        let mut bad = value.clone();
        match variant {
            0 => bad["transactions"][0]["expected"]["generation"] = Value::Null,
            1 => {
                bad["transactions"][1]["expected"]["transaction"] =
                    json!(Uuid::from_u128(500).to_string())
            }
            _ => {
                bad["transactions"].as_array_mut().unwrap().remove(0);
            }
        }
        assert!(CatalogModel::decode_catalog(&bytes(&bad), CatalogCodecLimits::default()).is_err());
    }
}

#[test]
fn selection_transaction_and_total_reference_budgets_precede_invalid_extra_elements() {
    let value = two_selection_golden();
    let mut accepted = Vec::new();
    for dimension in 0..3 {
        let mut bad = value.clone();
        let mut limits = CatalogCodecLimits::default();
        match dimension {
            0 => {
                limits.model.max_selection_slots = 1;
                bad["selections"][1] = Value::Null;
            }
            1 => {
                limits.model.max_transactions = 1;
                bad["transactions"][1] = Value::Null;
            }
            _ => {
                limits.model.max_total_references = 3;
                bad["generations"][1]["inputs"][1] = Value::Null;
            }
        }
        match CatalogModel::decode_catalog(&bytes(&bad), limits) {
            Ok(_) => accepted.push(dimension),
            Err(error) => assert!(
                error.to_string().contains("limit"),
                "dimension {dimension} did not hit pre-collection budget: {error:#}"
            ),
        }
    }
    assert!(
        accepted.is_empty(),
        "collector ignored budgets before invalid extra values: {accepted:?}"
    );
    let decoded =
        CatalogModel::decode_catalog(&bytes(&value), CatalogCodecLimits::default()).unwrap();
    assert_eq!(decoded.selections.len(), 2);
    assert_eq!(decoded.transactions.len(), 2);
    assert_eq!(decoded.total_references, 4);
}

fn two_selection_state() -> CatalogModel {
    let mut state = state();
    let key =
        SelectionKey::new(Uuid::from_u128(2), "other", [3; 32], ModelLimits::default()).unwrap();
    let allocation = format!("g-{:032x}", 2);
    state
        .register_generation(
            GenerationRecord::new(
                &allocation,
                *key.target(),
                [4; 32],
                [5; 32],
                vec![
                    InputReference::new(InputKind::Runtime, Digest::Sha512([2; 64])),
                    InputReference::new(InputKind::Artifact, Digest::Sha256([1; 32])),
                ],
                ModelLimits::default(),
            )
            .unwrap(),
        )
        .unwrap();
    state
        .apply_selection(
            &key,
            &SelectionStamp::default(),
            Some(&allocation),
            Uuid::from_u128(102),
        )
        .unwrap();
    state
}

#[test]
fn encoder_rechecks_every_tightened_model_limit_without_cloning_the_model() {
    let state = two_selection_state();
    assert_eq!(
        (
            state.generations.len(),
            state.selections.len(),
            state.transactions.len(),
            state.total_references
        ),
        (2, 2, 2, 4)
    );
    let mut accepted = vec![];
    for dimension in 0..6 {
        let mut limits = CatalogCodecLimits::default();
        match dimension {
            0 => limits.model.max_generation_records = 1,
            1 => limits.model.max_selection_slots = 1,
            2 => limits.model.max_refs_per_generation = 1,
            3 => limits.model.max_total_references = 3,
            4 => limits.model.max_group_bytes = "开发 main".len() - 1,
            _ => limits.model.max_transactions = 1,
        }
        match state.encode_catalog(limits) {
            Ok(_) => accepted.push(dimension),
            Err(error) => assert!(error.to_string().contains("limit"), "{error:#}"),
        }
    }
    assert!(
        accepted.is_empty(),
        "encoder ignored tightened model budgets: {accepted:?}"
    );
    let exact = CatalogCodecLimits {
        model: ModelLimits {
            max_generation_records: 2,
            max_selection_slots: 2,
            max_refs_per_generation: 2,
            max_total_references: 4,
            max_group_bytes: "开发 main".len(),
            max_transactions: 2,
        },
        ..Default::default()
    };
    let encoded = state.encode_catalog(exact).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&encoded).unwrap(),
        two_selection_golden()
    );
}

#[test]
fn r04_catalog_v2_roundtrip_keeps_typed_references_and_shared_transactions() {
    let mut state = CatalogModel::new(ModelLimits::default()).unwrap();
    let inputs: Vec<_> = [
        InputKind::Language,
        InputKind::Package,
        InputKind::BuildRequest,
        InputKind::BuildOutput,
    ]
    .into_iter()
    .map(|kind| InputReference::new(kind, Digest::Sha256([8; 32])))
    .collect();
    let generation = GenerationRecord::new_shared(
        &allocation(),
        [3; 32],
        [4; 32],
        [5; 32],
        [6; 32],
        inputs.clone(),
        ModelLimits::default(),
    )
    .unwrap();
    state.register_shared_generation(generation).unwrap();
    let other =
        SelectionKey::new(Uuid::from_u128(2), "other", [3; 32], ModelLimits::default()).unwrap();
    for (key, nonce) in [(owner(), 101), (other.clone(), 102)] {
        state
            .apply_selection(
                &key,
                &SelectionStamp::default(),
                Some(&allocation()),
                Uuid::from_u128(nonce),
            )
            .unwrap();
    }
    let bytes = state.encode_catalog(CatalogCodecLimits::default()).unwrap();
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["schema"], json!(3));
    assert_eq!(wire["generations"].as_array().unwrap().len(), 1);
    assert!(wire["generations"][0].get("owner").is_none());
    assert_eq!(
        wire["generations"][0]["shared_layout"],
        json!("06".repeat(32))
    );
    let restored = CatalogModel::decode_catalog(&bytes, CatalogCodecLimits::default()).unwrap();
    assert_eq!(restored.generation(&allocation()).unwrap().inputs(), inputs);
    assert_eq!(restored.snapshot(&other), state.snapshot(&other));
    assert_eq!(restored.protecting_references().len(), 4);
}

#[test]
fn r04_catalog_rejects_legacy_missing_or_duplicate_shared_identity_facts() {
    let mut legacy = golden();
    legacy["schema"] = json!(1);
    let mut missing = golden();
    missing["generations"][0]
        .as_object_mut()
        .unwrap()
        .remove("shared_layout");
    let mut malformed = golden();
    malformed["generations"][0]["shared_layout"] = json!("not-sha256");
    let mut duplicate = two_selection_golden();
    for generation in duplicate["generations"].as_array_mut().unwrap() {
        generation["shared_layout"] = json!("06".repeat(32));
        generation["inputs"] =
            json!([{"kind":"language","digest":format!("sha256-{}", "01".repeat(32))}]);
    }
    for wire in [legacy, missing, malformed, duplicate] {
        assert!(
            CatalogModel::decode_catalog(&bytes(&wire), CatalogCodecLimits::default()).is_err()
        );
    }
}

#[test]
fn r04_catalog_rejects_shared_records_without_typed_language_and_key_algorithms() {
    let language = json!({"kind":"language","digest":format!("sha256-{}", "01".repeat(32))});
    for inputs in [
        json!([]),
        json!([{"kind":"runtime","digest":format!("sha256-{}", "01".repeat(32))}]),
        json!([{"kind":"language","digest":format!("sha512-{}", "01".repeat(64))}]),
        json!([language.clone(), {"kind":"package","digest":format!("sha512-{}", "02".repeat(64))}]),
        json!([language.clone(), {"kind":"build-request","digest":format!("sha512-{}", "02".repeat(64))}]),
        json!([language.clone(), {"kind":"language","digest":format!("sha256-{}", "02".repeat(32))}]),
        json!([language.clone(), {"kind":"build-input","digest":format!("sha256-{}", "02".repeat(32))}]),
    ] {
        let mut wire = golden();
        wire["generations"][0]["shared_layout"] = json!("06".repeat(32));
        wire["generations"][0]["inputs"] = inputs;
        assert!(
            CatalogModel::decode_catalog(&bytes(&wire), CatalogCodecLimits::default()).is_err()
        );
    }
}
