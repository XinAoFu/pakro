use base64::Engine;
use pakro_index::npm::{parse_npm_version_metadata, parse_npm_versions};
use serde_json::json;

fn valid_integrity() -> String {
    // 64 bytes of 0x07 → valid sha512-length integrity
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode([7u8; 64])
    )
}

fn expected_digest_hex() -> String {
    format!("sha512-{}", "07".repeat(64))
}

fn typescript_fixture() -> serde_json::Value {
    json!({
        "name": "typescript",
        "version": "5.8.2",
        "dependencies": {
            "@ryanson34/yesno-mocha-reporter": "^2.0.0",
            "commander": "2.0.0"
        },
        "dist": {
            "tarball": "https://registry.npmjs.org/typescript/-/typescript-5.8.2.tgz",
            "integrity": valid_integrity()
        }
    })
}

#[test]
fn parses_dependencies_tarball_and_digest() {
    let md = parse_npm_version_metadata(&typescript_fixture()).unwrap();
    assert_eq!(md.name, "typescript");
    assert_eq!(md.version, "5.8.2");
    assert_eq!(md.deps.len(), 2);
    assert_eq!(md.deps[0].name, "@ryanson34/yesno-mocha-reporter");
    assert_eq!(md.deps[0].constraint, "^2.0.0");
    assert_eq!(md.deps[1].name, "commander");
    assert_eq!(md.deps[1].constraint, "2.0.0");

    assert_eq!(md.artifacts.len(), 1);
    let artifact = &md.artifacts[0];
    assert_eq!(artifact.filename, "typescript-5.8.2.tgz");
    assert!(artifact.url.contains("typescript-5.8.2.tgz"));
    assert!(!artifact.kind.is_wheel());
    assert_eq!(
        artifact.digest.as_deref(),
        Some(expected_digest_hex().as_str())
    );
}

#[test]
fn missing_or_malformed_integrity_is_rejected() {
    let mut fixture = typescript_fixture();
    fixture["dist"]["integrity"] = serde_json::Value::Null;
    assert!(parse_npm_version_metadata(&fixture).is_err());

    let mut fixture = typescript_fixture();
    fixture["dist"]["integrity"] = json!("sha512-not-base64!!");
    assert!(parse_npm_version_metadata(&fixture).is_err());

    // a sha1 integrity is not a content digest we can verify cheaply
    let mut fixture = typescript_fixture();
    fixture["dist"]["integrity"] = json!("sha1-AAAA");
    assert!(parse_npm_version_metadata(&fixture).is_err());
}

#[test]
fn missing_dist_or_name_is_rejected() {
    let mut fixture = typescript_fixture();
    fixture["dist"] = serde_json::Value::Null;
    assert!(parse_npm_version_metadata(&fixture).is_err());

    let mut fixture = typescript_fixture();
    fixture["name"] = serde_json::Value::Null;
    assert!(parse_npm_version_metadata(&fixture).is_err());
}

#[test]
fn empty_dependencies_is_ok() {
    let mut fixture = typescript_fixture();
    fixture["dependencies"] = serde_json::Value::Null;
    let md = parse_npm_version_metadata(&fixture).unwrap();
    assert!(md.deps.is_empty());
}

#[test]
fn parses_version_listing() {
    let value = json!({
        "name": "typescript",
        "versions": {
            "5.8.1": {"name": "typescript"},
            "5.8.2": {"name": "typescript"},
            "5.9.0-beta": {"name": "typescript"}
        }
    });
    let versions = parse_npm_versions(&value).unwrap();
    assert_eq!(
        versions,
        vec![
            "5.8.1".to_string(),
            "5.8.2".to_string(),
            "5.9.0-beta".to_string()
        ]
    );
    // prereleases stay in the listing; the solver's provider layer decides
    // their eligibility
}
