use node_semver::Version;
use pakro_resolver::npmver::range_to_ranges;
use std::str::FromStr;

fn contains(range: &str, version: &str) -> bool {
    range_to_ranges(range)
        .unwrap_or_else(|e| panic!("range {range:?}: {e}"))
        .contains(&Version::from_str(version).unwrap())
}

#[test]
fn exact_versions() {
    assert!(contains("1.2.3", "1.2.3"));
    assert!(!contains("1.2.3", "1.2.4"));
    assert!(contains("=1.2.3", "1.2.3"));
}

#[test]
fn caret_ranges() {
    assert!(contains("^1.2.3", "1.2.3"));
    assert!(contains("^1.2.3", "1.9.9"));
    assert!(!contains("^1.2.3", "2.0.0"));
    assert!(!contains("^1.2.3", "1.2.2"));
    // 0.x: caret only advances the minor
    assert!(contains("^0.2.3", "0.2.9"));
    assert!(!contains("^0.2.3", "0.3.0"));
    // 0.0.x: caret pins the patch
    assert!(contains("^0.0.3", "0.0.3"));
    assert!(!contains("^0.0.3", "0.0.4"));
}

#[test]
fn tilde_ranges() {
    assert!(contains("~1.2.3", "1.2.3"));
    assert!(contains("~1.2.3", "1.2.9"));
    assert!(!contains("~1.2.3", "1.3.0"));
    assert!(contains("~1.2", "1.2.0"));
    assert!(!contains("~1.2", "1.3.0"));
    assert!(contains("~1", "1.9.0"));
    assert!(!contains("~1", "2.0.0"));
}

#[test]
fn x_ranges() {
    assert!(contains("1.2.x", "1.2.9"));
    assert!(!contains("1.2.x", "1.3.0"));
    assert!(contains("1.2.*", "1.2.0"));
    assert!(contains("1.2", "1.2.5"));
    assert!(!contains("1.2", "1.3.0"));
    assert!(contains("*", "9.9.9"));
    assert!(contains("x", "0.0.1"));
    assert!(contains("", "1.0.0"));
}

#[test]
fn comparators_and_whitespace() {
    assert!(contains(">=1.2.0", "1.2.0"));
    assert!(!contains(">=1.2.0", "1.1.9"));
    assert!(contains(">1.2.0", "1.2.1"));
    assert!(!contains(">1.2.0", "1.2.0"));
    assert!(contains("<=2.0.0", "2.0.0"));
    assert!(!contains("<2.0.0", "2.0.0"));
    assert!(contains(">=1.2.0 <2.0.0", "1.5.0"));
    assert!(!contains(">=1.2.0 <2.0.0", "2.0.0"));
    assert!(contains("  >=  1.2.0   < 2.0.0 ", "1.5.0"));
}

#[test]
fn hyphen_ranges() {
    assert!(contains("1.2.3 - 2.3.4", "1.2.3"));
    assert!(contains("1.2.3 - 2.3.4", "2.3.4"));
    assert!(contains("1.2.3 - 2.3.4", "2.0.0"));
    assert!(!contains("1.2.3 - 2.3.4", "1.2.2"));
    assert!(!contains("1.2.3 - 2.3.4", "2.3.5"));
    assert!(contains("1.2 - 2", "1.9.0"));
    assert!(!contains("1.2 - 2", "3.0.0"));
}

#[test]
fn unions() {
    assert!(contains("^1.0.0 || ^2.0.0", "1.5.0"));
    assert!(contains("^1.0.0 || ^2.0.0", "2.5.0"));
    assert!(!contains("^1.0.0 || ^2.0.0", "3.0.0"));
    assert!(contains("1.2.3 || >2", "2.5.0"));
}

#[test]
fn prerelease_rules_by_layer() {
    // prerelease EXCLUSION is enforced by the metadata provider (it filters
    // prereleases unless exactly pinned); intervals follow semver ordering,
    // where 2.0.0-alpha.1 sorts before 2.0.0
    assert!(contains("^1.0.0", "2.0.0-alpha.1"));
    assert!(contains("1.5.0-beta", "1.5.0-beta"));
    assert!(contains("2.0.0-alpha.1", "2.0.0-alpha.1"));
    assert!(!contains("^1.0.0", "2.0.0"));
}

#[test]
fn rejects_dist_tags_and_garbage() {
    // strict-version policy: dist-tags must never resolve to "latest"
    assert!(range_to_ranges("latest").is_err());
    assert!(range_to_ranges("next").is_err());
    assert!(range_to_ranges("not a range").is_err());
    assert!(range_to_ranges(">=1.2.3 <").is_err());
    assert!(range_to_ranges("1.2.3 -").is_err());
}
