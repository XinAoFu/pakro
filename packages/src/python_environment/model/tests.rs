use super::*;

fn key(project: u128, group: &str) -> SelectionKey {
    SelectionKey::new(
        Uuid::from_u128(project),
        group,
        [3; 32],
        ModelLimits::default(),
    )
    .unwrap()
}
fn allocation(number: u128) -> String {
    format!("g-{number:032x}")
}
fn reference(kind: InputKind, number: u8) -> InputReference {
    InputReference::new(kind, Digest::Sha256([number; 32]))
}
fn record(owner: &SelectionKey, number: u128, inputs: Vec<InputReference>) -> GenerationRecord {
    GenerationRecord::new(
        &allocation(number),
        *owner.target(),
        [4; 32],
        [5; 32],
        inputs,
        ModelLimits::default(),
    )
    .unwrap()
}
fn model() -> CatalogModel {
    CatalogModel::new(ModelLimits::default()).unwrap()
}

#[test]
fn applied_transaction_lookup_preserves_a_commit_after_later_selection_changes() {
    let mut model = model();
    let owner = key(1, "main");
    register(&mut model, &owner, 1);
    let expected = model.snapshot(&owner);
    select(&mut model, &owner, 1, 100);
    let selected = model.snapshot(&owner);
    model
        .apply_selection(&owner, &selected, None, Uuid::from_u128(101))
        .unwrap();
    let (actual_key, actual_expected, generation) = model
        .applied_transaction(Uuid::from_u128(100))
        .expect("committed nonce history was lost");
    assert_eq!(actual_key, &owner);
    assert_eq!(actual_expected, &expected);
    assert_eq!(generation, Some(allocation(1).as_str()));
    let (_, actual_expected, generation) = model.applied_transaction(Uuid::from_u128(101)).unwrap();
    assert_eq!(actual_expected, &selected);
    assert_eq!(generation, None);
    assert!(model.applied_transaction(Uuid::from_u128(999)).is_none());
    assert_eq!(model.snapshot(&owner).revision(), 2);
}
fn register(model: &mut CatalogModel, key: &SelectionKey, number: u128) {
    model
        .register_generation(record(
            key,
            number,
            vec![reference(InputKind::Artifact, number as u8)],
        ))
        .unwrap();
}
fn select(model: &mut CatalogModel, key: &SelectionKey, number: u128, nonce: u128) {
    let expected = model.snapshot(key);
    assert_eq!(
        model
            .apply_selection(
                key,
                &expected,
                Some(&allocation(number)),
                Uuid::from_u128(nonce)
            )
            .unwrap(),
        ApplyResult::Applied
    );
}

#[test]
fn selection_identity_preserves_unicode_group_and_distinguishes_all_dimensions() {
    let first = key(1, "开发 main");
    assert_eq!(first.project(), Uuid::from_u128(1));
    assert_eq!(first.group(), "开发 main");
    assert_eq!(first.target(), &[3; 32]);
    assert_ne!(first, key(2, "开发 main"));
    assert_ne!(first, key(1, "开发 MAIN"));
    assert_ne!(
        first,
        SelectionKey::new(
            Uuid::from_u128(1),
            "开发 main",
            [9; 32],
            ModelLimits::default()
        )
        .unwrap()
    );
}

#[test]
fn invalid_selection_values_and_utf8_byte_budget_are_rejected() {
    assert!(SelectionKey::new(Uuid::nil(), "main", [0; 32], ModelLimits::default()).is_err());
    for group in ["", "bad\nname", "bad\0name", "bad\u{7f}name"] {
        assert!(
            SelectionKey::new(Uuid::from_u128(1), group, [0; 32], ModelLimits::default()).is_err(),
            "{group:?}"
        );
    }
    let limits = ModelLimits {
        max_group_bytes: 3,
        ..Default::default()
    };
    assert!(SelectionKey::new(Uuid::from_u128(1), "中", [0; 32], limits).is_ok());
    assert!(SelectionKey::new(Uuid::from_u128(1), "中文", [0; 32], limits).is_err());
}

#[test]
fn input_references_keep_kind_and_both_strong_digest_algorithms() {
    let sha256 = reference(InputKind::Runtime, 1);
    let sha512 = InputReference::new(InputKind::BuildInput, Digest::Sha512([1; 64]));
    assert_eq!(sha256.kind(), InputKind::Runtime);
    assert_eq!(sha512.kind(), InputKind::BuildInput);
    assert_eq!(sha256.digest(), format!("sha256-{}", "01".repeat(32)));
    assert_eq!(sha512.digest(), format!("sha512-{}", "01".repeat(64)));
    assert_ne!(sha256, reference(InputKind::Artifact, 1));
}

#[test]
fn generation_records_keep_facts_and_canonicalize_references() {
    let owner = key(1, "main");
    let a = reference(InputKind::Runtime, 1);
    let b = reference(InputKind::Artifact, 2);
    let actual = record(&owner, 1, vec![b.clone(), a.clone(), b.clone()]);
    assert_eq!(actual.allocation(), allocation(1));
    assert_eq!(actual.target(), owner.target());
    assert_eq!(actual.graph(), &[4; 32]);
    assert_eq!(actual.receipt(), &[5; 32]);
    assert_eq!(actual.inputs(), &[a, b]);
}

#[test]
fn generation_identifier_and_per_record_budget_are_checked() {
    for id in [
        "",
        "../outside",
        "g-1",
        "g-FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        "g-0000000000000000000000000000000g",
    ] {
        assert!(
            GenerationRecord::new(
                id,
                *key(1, "main").target(),
                [0; 32],
                [0; 32],
                vec![],
                ModelLimits::default()
            )
            .is_err(),
            "{id:?}"
        );
    }
    let limits = ModelLimits {
        max_refs_per_generation: 1,
        ..Default::default()
    };
    assert!(
        GenerationRecord::new(
            &allocation(1),
            *key(1, "main").target(),
            [0; 32],
            [0; 32],
            vec![
                reference(InputKind::Artifact, 1),
                reference(InputKind::Artifact, 2)
            ],
            limits
        )
        .is_err()
    );
}

#[test]
fn identical_registration_is_idempotent_but_conflicting_facts_cannot_replace_it() {
    let mut state = model();
    let value = record(&key(1, "main"), 1, vec![]);
    state.register_generation(value.clone()).unwrap();
    state.register_generation(value.clone()).unwrap();
    assert_eq!(state.generation(value.allocation()), Some(&value));
    let mut conflicting = value.clone();
    conflicting.receipt = [6; 32];
    assert!(state.register_generation(conflicting).is_err());
    assert_eq!(state.generation(value.allocation()), Some(&value));
}

#[test]
fn selection_replacement_has_a_per_key_revision_and_transaction() {
    let owner = key(1, "main");
    let mut state = model();
    register(&mut state, &owner, 1);
    register(&mut state, &owner, 2);
    assert_eq!(state.snapshot(&owner).revision(), 0);
    select(&mut state, &owner, 1, 101);
    let first = state.snapshot(&owner);
    assert_eq!(first.generation(), Some(allocation(1).as_str()));
    assert_eq!(first.revision(), 1);
    assert_eq!(first.transaction(), Uuid::from_u128(101));
    select(&mut state, &owner, 2, 102);
    assert_eq!(
        state.snapshot(&owner).generation(),
        Some(allocation(2).as_str())
    );
    assert_eq!(state.snapshot(&owner).revision(), 2);
    assert_eq!(state.retired_generations(), vec![allocation(1)]);
}

#[test]
fn unrelated_selections_merge_without_a_global_snapshot_overwrite() {
    let a = key(1, "main");
    let b = key(2, "main");
    let mut state = model();
    register(&mut state, &a, 1);
    register(&mut state, &b, 2);
    let old_a = state.snapshot(&a);
    let old_b = state.snapshot(&b);
    state
        .apply_selection(&a, &old_a, Some(&allocation(1)), Uuid::from_u128(101))
        .unwrap();
    state
        .apply_selection(&b, &old_b, Some(&allocation(2)), Uuid::from_u128(102))
        .unwrap();
    assert_eq!(
        state.snapshot(&a).generation(),
        Some(allocation(1).as_str())
    );
    assert_eq!(
        state.snapshot(&b).generation(),
        Some(allocation(2).as_str())
    );
}

#[test]
fn stale_selection_and_aba_updates_are_rejected_without_state_changes() {
    let owner = key(1, "main");
    let mut state = model();
    for number in [1, 2, 3] {
        register(&mut state, &owner, number);
    }
    select(&mut state, &owner, 1, 101);
    let stale = state.snapshot(&owner);
    select(&mut state, &owner, 2, 102);
    select(&mut state, &owner, 1, 103);
    let current = state.snapshot(&owner);
    assert!(
        state
            .apply_selection(&owner, &stale, Some(&allocation(3)), Uuid::from_u128(104))
            .is_err()
    );
    assert_eq!(state.snapshot(&owner), current);
}

#[test]
fn clearing_the_whole_selection_keeps_a_cas_tombstone_and_retires_the_generation() {
    let owner = key(1, "main");
    let mut state = model();
    register(&mut state, &owner, 1);
    let never_selected = state.snapshot(&owner);
    select(&mut state, &owner, 1, 101);
    let active = state.snapshot(&owner);
    state
        .apply_selection(&owner, &active, None, Uuid::from_u128(102))
        .unwrap();
    let removed = state.snapshot(&owner);
    assert_eq!(removed.generation(), None);
    assert_eq!(removed.revision(), 2);
    assert_eq!(state.retired_generations(), vec![allocation(1)]);
    assert!(
        state
            .apply_selection(
                &owner,
                &never_selected,
                Some(&allocation(1)),
                Uuid::from_u128(103)
            )
            .is_err()
    );
}

#[test]
fn missing_generation_wrong_target_and_nil_transaction_cannot_publish_metadata() {
    let owner = key(1, "main");
    let other =
        SelectionKey::new(Uuid::from_u128(2), "main", [9; 32], ModelLimits::default()).unwrap();
    let mut state = model();
    register(&mut state, &other, 2);
    let old = state.snapshot(&owner);
    assert!(
        state
            .apply_selection(&owner, &old, Some(&allocation(1)), Uuid::from_u128(101))
            .is_err()
    );
    assert!(
        state
            .apply_selection(&owner, &old, Some(&allocation(2)), Uuid::from_u128(102))
            .is_err()
    );
    assert!(
        state
            .apply_selection(&owner, &old, None, Uuid::nil())
            .is_err()
    );
    assert_eq!(state.snapshot(&owner), old);
}

#[test]
fn exact_transaction_replay_is_idempotent_and_nonce_reuse_is_rejected() {
    let owner = key(1, "main");
    let mut state = model();
    register(&mut state, &owner, 1);
    register(&mut state, &owner, 2);
    let old = state.snapshot(&owner);
    let nonce = Uuid::from_u128(101);
    assert_eq!(
        state
            .apply_selection(&owner, &old, Some(&allocation(1)), nonce)
            .unwrap(),
        ApplyResult::Applied
    );
    let after = state.snapshot(&owner);
    assert_eq!(
        state
            .apply_selection(&owner, &old, Some(&allocation(1)), nonce)
            .unwrap(),
        ApplyResult::AlreadyApplied
    );
    assert_eq!(state.snapshot(&owner), after);
    assert!(
        state
            .apply_selection(&owner, &after, Some(&allocation(2)), nonce)
            .is_err()
    );
    select(&mut state, &owner, 2, 102);
    let current = state.snapshot(&owner);
    assert!(
        state
            .apply_selection(&owner, &current, Some(&allocation(1)), nonce)
            .is_err()
    );
    assert_eq!(state.snapshot(&owner), current);
}

#[test]
fn retired_generations_protect_inputs_until_actual_gc_forgets_their_metadata() {
    let owner = key(1, "main");
    let mut state = model();
    register(&mut state, &owner, 1);
    register(&mut state, &owner, 2);
    select(&mut state, &owner, 1, 101);
    select(&mut state, &owner, 2, 102);
    assert_eq!(
        state.protecting_references(),
        BTreeSet::from([
            reference(InputKind::Artifact, 1),
            reference(InputKind::Artifact, 2)
        ])
    );
    assert!(state.forget_retired(&allocation(2)).is_err());
    state.forget_retired(&allocation(1)).unwrap();
    state.forget_retired(&allocation(1)).unwrap();
    assert!(state.generation(&allocation(1)).is_none());
    assert_eq!(
        state.protecting_references(),
        BTreeSet::from([reference(InputKind::Artifact, 2)])
    );
}

#[test]
fn record_count_total_inputs_and_destination_budgets_fail_before_partial_changes() {
    let owner = key(1, "main");
    let limits = ModelLimits {
        max_generation_records: 1,
        ..Default::default()
    };
    let mut state = CatalogModel::new(limits).unwrap();
    register(&mut state, &owner, 1);
    assert!(
        state
            .register_generation(record(&owner, 2, vec![]))
            .is_err()
    );
    assert!(state.generation(&allocation(1)).is_some());
    assert!(state.generation(&allocation(2)).is_none());
    let limits = ModelLimits {
        max_total_references: 1,
        ..Default::default()
    };
    let mut state = CatalogModel::new(limits).unwrap();
    register(&mut state, &owner, 1);
    assert!(
        state
            .register_generation(record(&owner, 2, vec![reference(InputKind::Artifact, 2)]))
            .is_err()
    );
    assert_eq!(state.protecting_references().len(), 1);
}

#[test]
fn selection_slot_and_transaction_history_budgets_are_atomic() {
    let a = key(1, "main");
    let b = key(2, "main");
    for transaction_limit in [false, true] {
        let mut limits = ModelLimits::default();
        if transaction_limit {
            limits.max_transactions = 1;
        } else {
            limits.max_selection_slots = 1;
        }
        let mut state = CatalogModel::new(limits).unwrap();
        register(&mut state, &a, 1);
        register(&mut state, &b, 2);
        select(&mut state, &a, 1, 101);
        let old_a = state.snapshot(&a);
        let old_b = state.snapshot(&b);
        assert!(
            state
                .apply_selection(&b, &old_b, Some(&allocation(2)), Uuid::from_u128(102))
                .is_err()
        );
        assert_eq!(state.snapshot(&a), old_a);
        assert_eq!(state.snapshot(&b), old_b);
        assert_eq!(
            state
                .apply_selection(
                    &a,
                    &SelectionStamp::default(),
                    Some(&allocation(1)),
                    Uuid::from_u128(101)
                )
                .unwrap(),
            ApplyResult::AlreadyApplied
        );
    }
}

#[test]
fn revision_overflow_fails_without_changing_the_last_valid_selection() {
    let owner = key(1, "main");
    let mut state = model();
    register(&mut state, &owner, 1);
    let before = SelectionStamp {
        revision: u64::MAX,
        generation: None,
        language: None,
        transaction: Uuid::from_u128(100),
    };
    state.selections.insert(owner.clone(), before.clone());
    assert!(
        state
            .apply_selection(&owner, &before, Some(&allocation(1)), Uuid::from_u128(101))
            .is_err()
    );
    assert_eq!(state.snapshot(&owner), before);
}

#[test]
fn all_zero_limits_are_rejected_before_model_creation() {
    for index in 0..6 {
        let mut limits = ModelLimits::default();
        match index {
            0 => limits.max_generation_records = 0,
            1 => limits.max_selection_slots = 0,
            2 => limits.max_refs_per_generation = 0,
            3 => limits.max_total_references = 0,
            4 => limits.max_group_bytes = 0,
            _ => limits.max_transactions = 0,
        }
        assert!(CatalogModel::new(limits).is_err(), "limit {index}");
    }
}

fn shared_record(number: u128, layout: u8) -> GenerationRecord {
    GenerationRecord::new_shared(
        &allocation(number),
        [3; 32],
        [4; 32],
        [5; 32],
        [layout; 32],
        vec![
            reference(InputKind::Language, 1),
            reference(InputKind::Package, 2),
        ],
        ModelLimits::default(),
    )
    .unwrap()
}

#[test]
fn r04_shared_registration_returns_one_canonical_allocation_and_rejects_conflicts() {
    let mut state = model();
    assert_eq!(
        state
            .register_shared_generation(shared_record(1, 6))
            .unwrap(),
        allocation(1)
    );
    assert_eq!(
        state
            .register_shared_generation(shared_record(2, 6))
            .unwrap(),
        allocation(1)
    );
    assert!(state.generation(&allocation(2)).is_none());
    assert_eq!(state.generations.len(), 1);
    for dimension in 0..4 {
        let mut conflicting = shared_record(3, 6);
        match dimension {
            0 => conflicting.target = [9; 32],
            1 => conflicting.graph = [9; 32],
            2 => conflicting.receipt = [9; 32],
            _ => conflicting
                .inputs
                .push(reference(InputKind::BuildOutput, 9)),
        }
        assert!(state.register_shared_generation(conflicting).is_err());
    }
    assert_eq!(state.generations.len(), 1);
    assert!(
        state
            .register_shared_generation(record(&key(1, "main"), 9, vec![]))
            .is_err()
    );
}

#[test]
fn r04_generic_registration_cannot_bypass_shared_identity_uniqueness() {
    let mut state = model();
    state.register_generation(shared_record(1, 6)).unwrap();
    assert!(state.register_generation(shared_record(2, 6)).is_err());
    state.register_generation(shared_record(1, 6)).unwrap();
    assert_eq!(state.generations.len(), 1);
}

#[test]
fn r04_shared_content_retirement_waits_for_every_selection() {
    let mut state = model();
    let a = key(1, "main");
    let b = key(2, "development");
    state
        .register_shared_generation(shared_record(1, 6))
        .unwrap();
    select(&mut state, &a, 1, 101);
    select(&mut state, &b, 1, 102);
    assert!(state.retired_generations().is_empty());
    let old_a = state.snapshot(&a);
    let old_b = state.snapshot(&b);
    state
        .apply_selection(&a, &old_a, None, Uuid::from_u128(103))
        .unwrap();
    assert_eq!(state.snapshot(&b), old_b);
    assert!(state.retired_generations().is_empty());
    assert!(state.forget_retired(&allocation(1)).is_err());
    state
        .apply_selection(&b, &old_b, None, Uuid::from_u128(104))
        .unwrap();
    assert_eq!(state.retired_generations(), vec![allocation(1)]);
    assert_eq!(state.protecting_references().len(), 2);
    state.forget_retired(&allocation(1)).unwrap();
    assert!(state.protecting_references().is_empty());
    assert!(state.generation_for_shared_layout(&[6; 32]).is_none());
    assert_eq!(
        state
            .register_shared_generation(shared_record(2, 6))
            .unwrap(),
        allocation(2)
    );
}

#[test]
fn r04_shared_registration_is_idempotent_at_capacity_and_checks_allocation_conflicts() {
    let mut state = CatalogModel::new(ModelLimits {
        max_generation_records: 1,
        max_total_references: 2,
        ..Default::default()
    })
    .unwrap();
    state
        .register_shared_generation(shared_record(1, 6))
        .unwrap();
    assert_eq!(
        state
            .register_shared_generation(shared_record(2, 6))
            .unwrap(),
        allocation(1)
    );
    assert!(
        state
            .register_shared_generation(shared_record(2, 7))
            .is_err()
    );
    assert!(
        state
            .register_shared_generation(shared_record(1, 7))
            .is_err()
    );
    assert_eq!(state.generations.len(), 1);
    assert_eq!(state.total_references, 2);
}

#[test]
fn r04_historical_stamp_projection_does_not_return_a_later_selection() {
    let mut state = model();
    let key = key(1, "main");
    register(&mut state, &key, 1);
    register(&mut state, &key, 2);
    select(&mut state, &key, 1, 101);
    let committed = state.snapshot(&key);
    select(&mut state, &key, 2, 102);
    assert_eq!(
        state.applied_stamp(Uuid::from_u128(101)),
        Some(committed.clone())
    );
    assert_eq!(
        state
            .apply_selection(
                &key,
                &SelectionStamp::default(),
                Some(&allocation(1)),
                Uuid::from_u128(101)
            )
            .unwrap(),
        ApplyResult::AlreadyApplied
    );
    assert_eq!(state.applied_stamp(Uuid::from_u128(101)), Some(committed));
    assert_eq!(state.snapshot(&key).revision(), 2);
    assert_eq!(
        state.snapshot(&key).generation(),
        Some(allocation(2).as_str())
    );
    assert_eq!(state.applied_stamp(Uuid::from_u128(999)), None);
}

fn shared_with_inputs(inputs: Vec<InputReference>) -> Result<GenerationRecord> {
    GenerationRecord::new_shared(
        &allocation(1),
        [3; 32],
        [4; 32],
        [5; 32],
        [6; 32],
        inputs,
        ModelLimits::default(),
    )
}

#[test]
fn r04_shared_records_require_one_language_and_reject_legacy_identity_kinds() {
    let language = reference(InputKind::Language, 1);
    for inputs in [
        vec![],
        vec![reference(InputKind::Package, 2)],
        vec![language.clone(), reference(InputKind::Language, 2)],
        vec![language.clone(), reference(InputKind::Runtime, 2)],
        vec![language.clone(), reference(InputKind::BuildInput, 2)],
    ] {
        assert!(shared_with_inputs(inputs).is_err());
    }
    assert_eq!(
        shared_with_inputs(vec![language.clone(), language])
            .unwrap()
            .inputs()
            .len(),
        1
    );
}

#[test]
fn r04_typed_key_references_require_sha256_but_artifact_outputs_keep_their_algorithm() {
    for kind in [
        InputKind::Language,
        InputKind::Package,
        InputKind::BuildRequest,
    ] {
        let invalid = InputReference::new(kind, Digest::Sha512([9; 64]));
        assert!(
            GenerationRecord::new(
                &allocation(1),
                [3; 32],
                [4; 32],
                [5; 32],
                vec![invalid.clone()],
                ModelLimits::default()
            )
            .is_err()
        );
        let inputs = if kind == InputKind::Language {
            vec![invalid]
        } else {
            vec![reference(InputKind::Language, 1), invalid]
        };
        assert!(shared_with_inputs(inputs).is_err());
    }
    let record = shared_with_inputs(vec![
        reference(InputKind::Language, 1),
        InputReference::new(InputKind::Artifact, Digest::Sha512([9; 64])),
        InputReference::new(InputKind::BuildOutput, Digest::Sha512([9; 64])),
    ])
    .unwrap();
    assert_eq!(record.inputs().len(), 3);
    assert_ne!(record.inputs()[0].kind(), record.inputs()[2].kind());
}
