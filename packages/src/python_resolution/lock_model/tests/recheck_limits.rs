//! Borrowed model bounds are independent from original/encoded wire lengths.
use super::*;

#[test]
fn tighter_model_counts_and_references_are_checked_without_encoding_a_document() {
    let model = Fixture::new().model(LockModelLimits::default()).unwrap();
    let exact = LockModelLimits {
        max_pins: 2,
        max_groups: 1,
        max_nodes_per_group: 2,
        max_total_nodes: 2,
        max_roots_per_group: 1,
        max_edges_per_group: 1,
        max_total_references: 7,
        ..LockModelLimits::default()
    };
    model.check_limits(exact).unwrap();
    for (limits, expected) in [
        (
            LockModelLimits {
                max_pins: 1,
                ..exact
            },
            LockLimit::Pins,
        ),
        (
            LockModelLimits {
                max_nodes_per_group: 1,
                ..exact
            },
            LockLimit::Nodes,
        ),
        (
            LockModelLimits {
                max_total_nodes: 1,
                ..exact
            },
            LockLimit::TotalNodes,
        ),
        (
            LockModelLimits {
                max_total_references: 6,
                ..exact
            },
            LockLimit::References,
        ),
    ] {
        assert_eq!(
            model.check_limits(limits).unwrap_err(),
            LockModelError::Limit(expected)
        );
    }
}
#[test]
fn inactive_complete_declarations_remain_in_the_borrowed_limit_accounting() {
    let mut fixture = Fixture::new();
    fixture
        .root_declarations
        .push("inactive-private-marker; os_name == 'other'");
    let model = fixture.model(LockModelLimits::default()).unwrap();
    model.check_limits(LockModelLimits::default()).unwrap();
    assert_eq!(
        model
            .check_limits(LockModelLimits {
                max_roots_per_group: 1,
                ..LockModelLimits::default()
            })
            .unwrap_err(),
        LockModelError::Limit(LockLimit::Roots)
    );
    let error = model
        .check_limits(LockModelLimits {
            max_declaration_bytes: 20,
            ..LockModelLimits::default()
        })
        .unwrap_err();
    assert_eq!(error, LockModelError::Limit(LockLimit::DeclarationBytes));
    assert!(!format!("{error:?} {error}").contains("inactive-private-marker"));
}
#[test]
fn tighter_field_and_artifact_limits_are_real_but_raw_document_size_is_not_invented() {
    let model = Fixture::new().model(LockModelLimits::default()).unwrap();
    // A pure model has no original or encoded wire receipt. Only positive
    // configuration validation applies to max_document_bytes at this layer.
    model
        .check_limits(LockModelLimits {
            max_document_bytes: 1,
            ..LockModelLimits::default()
        })
        .unwrap();
    model
        .check_limits(LockModelLimits {
            max_artifact_bytes: 456,
            ..LockModelLimits::default()
        })
        .unwrap();
    for (limits, expected) in [
        (
            LockModelLimits {
                max_artifact_bytes: 455,
                ..LockModelLimits::default()
            },
            LockLimit::ArtifactBytes,
        ),
        (
            LockModelLimits {
                max_field_bytes: 1,
                ..LockModelLimits::default()
            },
            LockLimit::FieldBytes,
        ),
        (
            LockModelLimits {
                max_total_utf8_bytes: 1,
                ..LockModelLimits::default()
            },
            LockLimit::TotalUtf8Bytes,
        ),
    ] {
        assert_eq!(
            model.check_limits(limits).unwrap_err(),
            LockModelError::Limit(expected)
        );
    }
    assert_eq!(
        model
            .check_limits(LockModelLimits {
                max_document_bytes: 0,
                ..LockModelLimits::default()
            })
            .unwrap_err(),
        LockModelError::InvalidLimits
    );
}
