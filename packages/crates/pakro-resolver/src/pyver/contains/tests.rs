use super::*;
use std::str::FromStr;

fn check(spec: &str, candidate: &str, expected: bool) {
    let specs = VersionSpecifiers::from_str(spec).unwrap();
    let version = Version::from_str(candidate).unwrap();
    assert_eq!(
        specifiers_contain(&specs, &version),
        expected,
        "{spec} against {candidate}"
    );
}

#[test]
fn a_pre_bound_accepts_earlier_rc_alpha_and_development_versions() {
    for (spec, version, expected) in [
        ("<2.0rc2", "2.0rc1", true),
        ("<2.0rc2", "2.0.dev1", true),
        ("<2.0rc2", "2.0rc2", false),
        ("<2.0.dev2", "2.0.dev1", true),
        ("<2.0.dev2", "2.0.dev2", false),
        ("<2.0a2", "2.0a1", true),
        ("<2.0a2", "2.0rc1", false),
    ] {
        check(spec, version, expected);
    }
}

#[test]
fn actual_epochs_still_order_before_the_pre_bound() {
    for (spec, version, expected) in [
        ("<1!2.0rc2", "1!2.0rc1", true),
        ("<1!2.0rc2", "0!9999.0", true),
        ("<1!2.0rc2", "2!1.0", false),
        ("<1!2.0rc2", "1!2.0rc2", false),
    ] {
        check(spec, version, expected);
    }
}

#[test]
fn candidate_local_labels_are_ignored_and_every_intersected_clause_still_applies() {
    for (spec, version, expected) in [
        ("<2.0rc2", "2.0rc1+private.1", true),
        ("<2.0rc2", "2.0rc2+private.1", false),
        (">=2.0rc1,<2.0rc2", "2.0rc1+private.1", true),
        (">=2.0rc1,<2.0rc2", "2.0.dev1", false),
        ("<2.0rc2,!=2.0rc1", "2.0rc1", false),
    ] {
        check(spec, version, expected);
    }
}

#[test]
fn other_operators_and_final_bounds_keep_the_original_matcher_contract() {
    for (spec, version, expected) in [
        ("<2.0", "2.0rc1", false),
        ("<2.0", "2.0.dev1", false),
        ("<2.0", "1.9rc1", true),
        (">1.0", "1.0.post1", false),
        (">1.0", "1.0+local", false),
        ("==1.0", "1.0+local", true),
        ("!=1.0", "1.0+local", false),
        ("==1.0.*", "1.0rc1", true),
        (">=1,<3", "2.0rc1", true),
    ] {
        check(spec, version, expected);
    }
}
