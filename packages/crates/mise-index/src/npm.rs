//! npm registry client and response parsing. Metadata comes from
//! `registry.npmjs.org/{name}/{version}` (single version) and
//! `/{name}` (all versions); tarball digests are SRI integrity strings
//! (`sha512-<base64>`), converted here to `sha512-<hex>` digest strings.

use base64::Engine;
use serde_json::Value;

use crate::types::{ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata};
use crate::{IndexError, Result};

/// Parses a single-version registry document. The SRI integrity in `dist` is
/// mandatory: a tarball we cannot digest-verify is not installable.
pub fn parse_npm_version_metadata(value: &Value) -> Result<PackageMetadata> {
    let bad = |reason: &str| IndexError::BadResponse {
        package: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        reason: reason.to_string(),
    };
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing name"))?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing version"))?;
    let dist = value
        .get("dist")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("missing dist"))?;
    let tarball = dist
        .get("tarball")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing dist.tarball"))?;
    let integrity = dist
        .get("integrity")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing dist.integrity"))?;
    let digest_hex =
        sri_sha512_to_hex(integrity).map_err(|reason| bad(&format!("dist.integrity: {reason}")))?;
    let mut deps = vec![];
    if let Some(dependencies) = value.get("dependencies").and_then(Value::as_object) {
        for (dep_name, constraint) in dependencies {
            let Some(constraint) = constraint.as_str() else {
                continue;
            };
            deps.push(DependencyConstraint {
                name: dep_name.trim().to_string(),
                constraint: constraint.trim().to_string(),
            });
        }
    }
    let filename = tarball.rsplit('/').next().unwrap_or_default().to_string();
    Ok(PackageMetadata {
        name: name.to_string(),
        version: version.to_string(),
        requires_python: None,
        deps,
        artifacts: vec![ArtifactDescriptor {
            filename,
            url: tarball.to_string(),
            digest: Some(format!("sha512-{digest_hex}")),
            kind: ArtifactKind::Sdist,
        }],
    })
}

/// Version keys of a full `/{name}` document, sorted.
pub fn parse_npm_versions(value: &Value) -> Result<Vec<String>> {
    let bad = |reason: &str| IndexError::BadResponse {
        package: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        reason: reason.to_string(),
    };
    let versions = value
        .get("versions")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("missing versions map"))?;
    let mut out: Vec<String> = versions.keys().cloned().collect();
    out.sort();
    Ok(out)
}

/// `sha512-BASE64` → 128 lowercase hex chars.
fn sri_sha512_to_hex(integrity: &str) -> std::result::Result<String, String> {
    let b64 = integrity
        .strip_prefix("sha512-")
        .ok_or("only sha512 integrity is supported")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "integrity is not valid base64".to_string())?;
    if bytes.len() != 64 {
        return Err("integrity is not a sha512 digest".to_string());
    }
    Ok(hex_of(&bytes))
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
