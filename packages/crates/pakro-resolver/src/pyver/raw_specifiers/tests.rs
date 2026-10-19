use super::*;

fn parse(raw: &str) -> RawVersionConstraint {
    RawVersionConstraint::parse(raw, RawVersionConstraintLimits::default()).unwrap()
}

#[test]
fn ordinary_constraints_keep_shared_pep440_semantics() {
    for (raw, candidate, expected) in [
        ("", "1.2", true),
        (">=1,<2", "1.5", true),
        (">=1,<2", "2", false),
        ("==1.0", "1.0+vendor.1", true),
        ("!=1.*", "1.5", false),
        ("<1.0a3", "1.0a2", true),
        (">1.0", "1.0.post1", false),
        (">1.0", "1.0.1", true),
    ] {
        assert_eq!(parse(raw).matches(candidate).unwrap(), expected, "{raw}");
    }
}

#[test]
fn arbitrary_equality_preserves_rhs_spelling_and_ascii_case() {
    for (raw, candidate, expected) in [
        ("===01.0", "1.0", false),
        ("===01.0", "01.0", true),
        ("===v1.0", "1.0", false),
        ("===1.0RC1", "1.0rc1", true),
        ("===1.0+vendor-1", "1.0+vendor.1", false),
        ("===1.0.0", "1.0", false),
    ] {
        assert_eq!(parse(raw).matches(candidate).unwrap(), expected, "{raw}");
    }
}

#[test]
fn every_repeated_and_mixed_clause_remains_a_conjunction() {
    assert!(parse(">=1,===01.0,<=1.0").matches("01.0").unwrap());
    assert!(!parse(">=2,===01.0").matches("01.0").unwrap());
    assert!(!parse("===01.0,===1.0").matches("1.0").unwrap());
    let repeated = parse("===01.0,===01.0");
    assert_eq!(repeated.clause_count(), 2);
    assert!(repeated.matches("01.0").unwrap());
    assert!(repeated.ordinary().is_empty());
}

#[test]
fn literal_only_legacy_comparison_never_fakes_a_numeric_version() {
    assert!(parse("===legacy-tag").matches("LEGACY-tag").unwrap());
    assert!(!parse("===legacy-tag,>=0").matches("legacy-tag").unwrap());
    assert!(!parse(">=0").matches("legacy-tag").unwrap());
    assert!(!parse("").matches("legacy-tag").unwrap());
}

#[test]
fn specifier_only_grammar_handles_parentheses_and_one_trailing_comma() {
    assert!(parse(" ( >=1, ===01.0, ) ").matches("01.0").unwrap());
    assert!(parse("===01.0,").matches("01.0").unwrap());
    for invalid in [
        ",>=1",
        ">=1,,<2",
        ">=1,,",
        "()",
        "((>=1))",
        "(>=1",
        ">=1)",
        "===",
        "===two words",
        "===token;extra",
        "===token\0",
        "=>1",
        "demo>=1",
    ] {
        assert_eq!(
            RawVersionConstraint::parse(invalid, RawVersionConstraintLimits::default())
                .unwrap_err(),
            RawVersionConstraintError::Syntax,
            "invalid shape {invalid:?}"
        );
    }
}

#[test]
fn all_four_budgets_accept_exact_and_reject_the_next_unit() {
    let defaults = RawVersionConstraintLimits::default();
    for (raw, exact, tight, axis) in [
        (
            "===01.0",
            RawVersionConstraintLimits {
                max_specifier_bytes: 7,
                ..defaults
            },
            RawVersionConstraintLimits {
                max_specifier_bytes: 6,
                ..defaults
            },
            RawVersionConstraintLimit::SpecifierBytes,
        ),
        (
            "===1,===1",
            RawVersionConstraintLimits {
                max_clauses: 2,
                ..defaults
            },
            RawVersionConstraintLimits {
                max_clauses: 1,
                ..defaults
            },
            RawVersionConstraintLimit::Clauses,
        ),
        (
            "===01.0",
            RawVersionConstraintLimits {
                max_literal_bytes: 4,
                ..defaults
            },
            RawVersionConstraintLimits {
                max_literal_bytes: 3,
                ..defaults
            },
            RawVersionConstraintLimit::LiteralBytes,
        ),
    ] {
        RawVersionConstraint::parse(raw, exact).unwrap();
        assert_eq!(
            RawVersionConstraint::parse(raw, tight).unwrap_err(),
            RawVersionConstraintError::Limit(axis)
        );
    }
    let constraint = RawVersionConstraint::parse(
        "===01.0",
        RawVersionConstraintLimits {
            max_candidate_bytes: 4,
            ..defaults
        },
    )
    .unwrap();
    assert!(constraint.matches("01.0").unwrap());
    assert_eq!(
        constraint.matches("001.0").unwrap_err(),
        RawVersionConstraintError::Limit(RawVersionConstraintLimit::CandidateBytes)
    );
}

#[test]
fn original_raw_text_and_duplicate_cost_are_not_normalized_away() {
    let raw = " ( ===01.0 , ===01.0 , >= 1 , ) ";
    let constraint = parse(raw);
    assert_eq!(constraint.raw(), raw);
    assert_eq!(constraint.clause_count(), 3);
    assert!(constraint.has_arbitrary_equality());
    assert!(constraint.matches("01.0").unwrap());
    assert_eq!(
        RawVersionConstraint::parse(
            raw,
            RawVersionConstraintLimits {
                max_clauses: 2,
                ..RawVersionConstraintLimits::default()
            }
        )
        .unwrap_err(),
        RawVersionConstraintError::Limit(RawVersionConstraintLimit::Clauses)
    );
}

#[test]
fn zero_limits_and_diagnostics_never_echo_private_inputs() {
    let control = parse("===PRIVATE_LITERAL");
    assert!(!format!("{control:?}").contains("PRIVATE"));
    for limits in [
        RawVersionConstraintLimits {
            max_specifier_bytes: 0,
            ..RawVersionConstraintLimits::default()
        },
        RawVersionConstraintLimits {
            max_clauses: 0,
            ..RawVersionConstraintLimits::default()
        },
        RawVersionConstraintLimits {
            max_literal_bytes: 0,
            ..RawVersionConstraintLimits::default()
        },
        RawVersionConstraintLimits {
            max_candidate_bytes: 0,
            ..RawVersionConstraintLimits::default()
        },
    ] {
        let error = RawVersionConstraint::parse("PRIVATE_TOKEN", limits).unwrap_err();
        assert_eq!(error, RawVersionConstraintError::InvalidLimits);
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    let error = RawVersionConstraint::parse("PRIVATE_TOKEN", RawVersionConstraintLimits::default())
        .unwrap_err();
    assert_eq!(error, RawVersionConstraintError::Syntax);
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
}
