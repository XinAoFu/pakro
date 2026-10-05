use mise_resolver::pyver::constraint_to_ranges;
use pep440_rs::Version;
use std::str::FromStr;

fn contains(constraint: &str, version: &str) -> bool {
    constraint_to_ranges(constraint)
        .unwrap()
        .contains(&Version::from_str(version).unwrap())
}

#[test]
fn empty_constraint_is_full_range() {
    assert!(contains("", "0.0.1"));
    assert!(contains("", "99.99"));
}

#[test]
fn comparison_operators() {
    assert!(contains(">=0.1, <0.3", "0.1"));
    assert!(contains(">=0.1, <0.3", "0.2"));
    assert!(!contains(">=0.1, <0.3", "0.3"));
    assert!(!contains(">1.0", "1.0"));
    assert!(contains(">1.0", "1.0.1"));
    assert!(contains("<=1.0", "1.0"));
    assert!(!contains("<=1.0", "1.0.1"));
}

#[test]
fn equality_operators() {
    assert!(contains("==1.2.3", "1.2.3"));
    assert!(!contains("==1.2.3", "1.2.4"));
    assert!(contains("===1.2.3", "1.2.3"));
}

#[test]
fn star_equality_is_prefix_range() {
    assert!(contains("==1.2.*", "1.2"));
    assert!(contains("==1.2.*", "1.2.9"));
    assert!(!contains("==1.2.*", "1.3"));
    assert!(contains("==1.*", "1.9"));
    assert!(!contains("==1.*", "2.0"));
}

#[test]
fn tilde_equals_covers_prefix_segment() {
    assert!(contains("~=1.4.2", "1.4.2"));
    assert!(contains("~=1.4.2", "1.4.99"));
    assert!(!contains("~=1.4.2", "1.5.0"));
    assert!(!contains("~=1.4.2", "1.4.1"));
}

#[test]
fn not_equal_excludes_only_that_version() {
    assert!(!contains("!=1.2.3", "1.2.3"));
    assert!(contains("!=1.2.3", "1.2.4"));
    assert!(!contains("!=1.2.*", "1.2.5"));
    assert!(contains("!=1.2.*", "1.3.0"));
}

#[test]
fn invalid_constraint_is_rejected() {
    assert!(constraint_to_ranges("not-a-specifier").is_err());
    assert!(constraint_to_ranges(">= 1.0, <").is_err());
}
