use super::*;
use crate::libraries::language::identity::{
    IdentityLimits, LanguageIdentityInput, LanguageInput, TargetIdentityInput,
};
use crate::libraries::package::identity::{PackageIdentityInput, PackageRole, SourceIdentity};
use crate::packages::python_environment::model::InputKind;
use pakro_store::Digest;

fn target(abi: &str) -> TargetIdentity {
    TargetIdentity::new(
        TargetIdentityInput {
            os: "linux",
            arch: "x86_64",
            libc: "glibc-2.17",
            abi,
            compatibility: [7; 32],
        },
        IdentityLimits::default(),
    )
    .unwrap()
}
fn language(target: &TargetIdentity) -> LanguageKey {
    LanguageKey::new(
        LanguageIdentityInput {
            implementation: "cpython",
            distribution: "python-build-standalone",
            exact_version: "3.11.9",
            target,
            input: LanguageInput::Artifact(ArtifactId::new(Digest::Sha256([1; 32]))),
            recipe: 1,
        },
        IdentityLimits::default(),
    )
    .unwrap()
}
fn package(target: &TargetIdentity) -> PackageInstanceId {
    let source = SourceIdentity::new("pypi", [2; 32], IdentityLimits::default()).unwrap();
    PackageInstanceId::python(
        PackageIdentityInput {
            name: "demo",
            exact_version: "1.0.0",
            source: &source,
            role: PackageRole::Runtime,
            target,
            dependency_context: [3; 32],
        },
        IdentityLimits::default(),
    )
    .unwrap()
}
fn key(project: u128, target: &TargetIdentity) -> SelectionKey {
    SelectionKey::new(
        Uuid::from_u128(project),
        "main",
        target.fingerprint(),
        ModelLimits::default(),
    )
    .unwrap()
}
fn register(
    model: &mut ProjectSelections,
    target: &TargetIdentity,
    allocation: &str,
    packages: &[PackageInstanceId],
    graph: u8,
    receipt: u8,
) -> SharedContentReference {
    model
        .register_content(SharedContentInput {
            allocation,
            language: language(target),
            target,
            packages,
            graph: [graph; 32],
            build_requests: &[],
            artifacts: &[],
            build_outputs: &[],
            layout_recipe: 1,
            receipt: [receipt; 32],
        })
        .unwrap()
}

#[test]
fn r04_two_projects_reference_one_content_record() {
    let target = target("cp311");
    let package = package(&target);
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let content = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000001",
        &[package],
        4,
        5,
    );
    let duplicate = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000002",
        &[package],
        4,
        5,
    );
    assert_eq!(content, duplicate);
    let a = key(1, &target);
    let b = key(2, &target);
    let a_stamp = model
        .apply_selection(
            &a,
            &SelectionStamp::default(),
            Some(&content),
            Uuid::from_u128(101),
        )
        .unwrap();
    let b_stamp = model
        .apply_selection(
            &b,
            &SelectionStamp::default(),
            Some(&content),
            Uuid::from_u128(102),
        )
        .unwrap();
    assert_eq!(a_stamp.revision(), 1);
    assert_eq!(b_stamp.revision(), 1);
    assert_ne!(a_stamp.transaction(), b_stamp.transaction());
    assert_eq!(a_stamp.generation(), b_stamp.generation());
    model
        .apply_selection(&a, &a_stamp, None, Uuid::from_u128(103))
        .unwrap();
    assert_eq!(model.snapshot(&b).unwrap().stamp(), &b_stamp);
    assert!(model.forget_retired(content.allocation()).is_err());
    let wire: serde_json::Value =
        serde_json::from_slice(&model.encode(CatalogCodecLimits::default()).unwrap()).unwrap();
    assert_eq!(wire["generations"].as_array().unwrap().len(), 1);
    assert_eq!(wire["selections"].as_array().unwrap().len(), 2);
    assert!(wire["generations"][0].get("owner").is_none());
}

#[test]
fn r04_same_version_different_abi_stays_distinct() {
    let a = target("cp311");
    let b = target("cp311d");
    assert_ne!(language(&a), language(&b));
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let content_a = register(
        &mut model,
        &a,
        "g-00000000000000000000000000000001",
        &[],
        4,
        5,
    );
    let content_b = register(
        &mut model,
        &b,
        "g-00000000000000000000000000000002",
        &[],
        4,
        5,
    );
    assert_ne!(content_a.layout(), content_b.layout());
    let a_key = key(1, &a);
    assert!(
        model
            .apply_selection(
                &a_key,
                &SelectionStamp::default(),
                Some(&content_b),
                Uuid::from_u128(101)
            )
            .is_err()
    );
    assert_eq!(model.snapshot(&a_key).unwrap().stamp().revision(), 0);
}

#[test]
fn r04_empty_packages_keep_declared_language() {
    let target = target("cp311");
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let package = package(&target);
    let a = key(1, &target);
    let full = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000001",
        &[package],
        4,
        5,
    );
    let first = model
        .apply_selection(
            &a,
            &SelectionStamp::default(),
            Some(&full),
            Uuid::from_u128(101),
        )
        .unwrap();
    let empty = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000002",
        &[],
        6,
        7,
    );
    let next = model
        .apply_selection(&a, &first, Some(&empty), Uuid::from_u128(102))
        .unwrap();
    assert_eq!(next.revision(), 2);
    assert!(next.generation().is_some());
    let view = model.snapshot(&a).unwrap();
    assert_eq!(view.key(), &a);
    assert!(view.packages().is_empty());
    let language = view.language().unwrap();
    assert_eq!(language.kind(), InputKind::Language);
    assert!(model.protecting_references().contains(language));
    let restored = ProjectSelections::decode(
        &model.encode(CatalogCodecLimits::default()).unwrap(),
        CatalogCodecLimits::default(),
    )
    .unwrap();
    assert_eq!(restored.snapshot(&a).unwrap().language(), Some(language));
    assert!(restored.snapshot(&a).unwrap().packages().is_empty());
}

#[test]
fn r04_replaying_an_older_transaction_returns_its_stamp_without_rewinding_selection() {
    let target = target("cp311");
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let a = key(1, &target);
    let content = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000001",
        &[],
        4,
        5,
    );
    let before = SelectionStamp::default();
    let transaction = Uuid::from_u128(101);
    let first = model
        .apply_selection(&a, &before, Some(&content), transaction)
        .unwrap();
    let cleared = model
        .apply_selection(&a, &first, None, Uuid::from_u128(102))
        .unwrap();
    let replay = model
        .apply_selection(&a, &before, Some(&content), transaction)
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(model.snapshot(&a).unwrap().stamp(), &cleared);
    assert!(
        model
            .apply_selection(&a, &before, Some(&content), Uuid::from_u128(103))
            .is_err()
    );
    assert!(
        model
            .apply_selection(&a, &cleared, None, transaction)
            .is_err()
    );
}

#[test]
fn r04_conflicting_content_preserves_catalog_bytes() {
    let target = target("cp311");
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    register(
        &mut model,
        &target,
        "g-00000000000000000000000000000001",
        &[],
        4,
        5,
    );
    let before = model.encode(CatalogCodecLimits::default()).unwrap();
    assert!(
        model
            .register_content(SharedContentInput {
                allocation: "g-00000000000000000000000000000002",
                language: language(&target),
                target: &target,
                packages: &[],
                graph: [4; 32],
                build_requests: &[],
                artifacts: &[],
                build_outputs: &[],
                layout_recipe: 1,
                receipt: [9; 32]
            })
            .is_err()
    );
    assert_eq!(model.encode(CatalogCodecLimits::default()).unwrap(), before);
}

#[test]
fn r04_typed_content_references_keep_actual_digest_algorithms() {
    let target = target("cp311");
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let digest = Digest::Sha512([6; 64]);
    model
        .register_content(SharedContentInput {
            allocation: "g-00000000000000000000000000000001",
            language: language(&target),
            target: &target,
            packages: &[],
            graph: [4; 32],
            build_requests: &[],
            artifacts: &[ArtifactId::new(digest)],
            build_outputs: &[OutputDigest::new(digest)],
            layout_recipe: 1,
            receipt: [5; 32],
        })
        .unwrap();
    let refs = model.protecting_references();
    assert!(refs.contains(&InputReference::new(InputKind::Artifact, digest)));
    assert!(refs.contains(&InputReference::new(InputKind::BuildOutput, digest)));
    assert_eq!(refs.len(), 3);
}

#[test]
fn r04_content_binding_rejects_mixed_targets_without_catalog_mutation() {
    let target_a = target("cp311");
    let target_b = target("cp311d");
    let package_b = package(&target_b);
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let before = model.encode(CatalogCodecLimits::default()).unwrap();
    for language_mismatch in [false, true] {
        let language = if language_mismatch {
            language(&target_b)
        } else {
            language(&target_a)
        };
        let packages = if language_mismatch {
            vec![]
        } else {
            vec![package_b]
        };
        assert!(
            model
                .register_content(SharedContentInput {
                    allocation: "g-00000000000000000000000000000001",
                    language,
                    target: &target_a,
                    packages: &packages,
                    graph: [4; 32],
                    build_requests: &[],
                    artifacts: &[],
                    build_outputs: &[],
                    layout_recipe: 1,
                    receipt: [5; 32]
                })
                .is_err()
        );
    }
    assert_eq!(model.encode(CatalogCodecLimits::default()).unwrap(), before);
}

#[test]
fn r04_reference_budget_counts_raw_duplicates_before_canonicalization() {
    let target = target("cp311");
    let package = package(&target);
    let mut model = ProjectSelections::new(ModelLimits {
        max_refs_per_generation: 2,
        ..Default::default()
    })
    .unwrap();
    let before = model.encode(CatalogCodecLimits::default()).unwrap();
    assert!(
        model
            .register_content(SharedContentInput {
                allocation: "g-00000000000000000000000000000001",
                language: language(&target),
                target: &target,
                packages: &[package, package],
                graph: [4; 32],
                build_requests: &[],
                artifacts: &[],
                build_outputs: &[],
                layout_recipe: 1,
                receipt: [5; 32]
            })
            .is_err()
    );
    assert_eq!(model.encode(CatalogCodecLimits::default()).unwrap(), before);
}

#[test]
fn r04_nonce_replay_survives_retired_metadata_cleanup() {
    let target = target("cp311");
    let mut model = ProjectSelections::new(ModelLimits::default()).unwrap();
    let key = key(1, &target);
    let content = register(
        &mut model,
        &target,
        "g-00000000000000000000000000000001",
        &[],
        4,
        5,
    );
    let before = SelectionStamp::default();
    let txn = Uuid::from_u128(101);
    let first = model
        .apply_selection(&key, &before, Some(&content), txn)
        .unwrap();
    let cleared = model
        .apply_selection(&key, &first, None, Uuid::from_u128(102))
        .unwrap();
    model.forget_retired(content.allocation()).unwrap();
    assert_eq!(
        model
            .apply_selection(&key, &before, Some(&content), txn)
            .unwrap(),
        first
    );
    assert_eq!(model.snapshot(&key).unwrap().stamp(), &cleared);
    assert!(model.protecting_references().is_empty());
}

#[test]
fn r04_foreign_content_description_cannot_select_different_facts() {
    let target = target("cp311");
    let mut a = ProjectSelections::new(ModelLimits::default()).unwrap();
    let mut b = ProjectSelections::new(ModelLimits::default()).unwrap();
    let allocation = "g-00000000000000000000000000000001";
    let content = register(&mut a, &target, allocation, &[], 4, 5);
    register(&mut b, &target, allocation, &[], 6, 7);
    let key = key(1, &target);
    assert!(
        b.apply_selection(
            &key,
            &SelectionStamp::default(),
            Some(&content),
            Uuid::from_u128(101)
        )
        .is_err()
    );
    assert_eq!(b.snapshot(&key).unwrap().stamp().revision(), 0);
}

#[test]
fn r04_generic_selection_is_not_upgraded_to_shared_content_by_decoding() {
    let target = target("cp311");
    let key = key(1, &target);
    let allocation = "g-00000000000000000000000000000001";
    let mut original = CatalogModel::new(ModelLimits::default()).unwrap();
    original
        .register_generation(
            GenerationRecord::new(
                allocation,
                target.fingerprint(),
                [4; 32],
                [5; 32],
                vec![InputReference::new(
                    InputKind::Runtime,
                    Digest::Sha256([6; 32]),
                )],
                ModelLimits::default(),
            )
            .unwrap(),
        )
        .unwrap();
    original
        .apply_selection(
            &key,
            &SelectionStamp::default(),
            Some(allocation),
            Uuid::from_u128(101),
        )
        .unwrap();
    let before = original
        .encode_catalog(CatalogCodecLimits::default())
        .unwrap();
    let restored = ProjectSelections::decode(&before, CatalogCodecLimits::default()).unwrap();
    let error = restored.snapshot(&key).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("generic record has no shared content identity")
    );
    assert_eq!(
        restored.encode(CatalogCodecLimits::default()).unwrap(),
        before
    );
}
