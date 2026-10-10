//! PyPI JSON parsing. Explicit contexts supply interpreter marker fields;
//! raw release listing remains available without target compatibility checks.

use std::str::FromStr;

use pep508_rs::{MarkerEnvironment, Requirement};
use serde_json::Value;

use crate::python_context::PythonMarkerContext;
use crate::types::{
    ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata, normalize_py_name,
};
use crate::wheel::parse_wheel_filename;
use crate::{IndexError, Result};

/// Evaluate dependencies using all explicit marker fields. Invalid info or file
/// Requires-Python is an error. Valid nonmatching files are removed after every
/// supplied file condition is validated; no remaining artifacts is an error.
/// A valid info constraint is retained for the caller's install check.
/// Requested extras and direct URL dependencies remain outside this batch.
pub fn parse_pypi_response_for_context(
    value: &Value,
    context: &PythonMarkerContext,
) -> Result<PackageMetadata> {
    if let Some(spec) = value.pointer("/info/requires_python") {
        check_requires_python_value(spec, context)?;
    }
    let Some(files) = value.get("urls").and_then(Value::as_array) else {
        return parse_pypi_response_with_environment(value, context.environment());
    };
    let accepted = files
        .iter()
        .map(|file| match file.get("requires_python") {
            None => Ok(true),
            Some(spec) => check_requires_python_value(spec, context),
        })
        .collect::<Result<Vec<_>>>()?;
    let mut filtered = value.clone();
    filtered["urls"] = Value::Array(
        files
            .iter()
            .zip(accepted)
            .filter(|(_, accepted)| *accepted)
            .map(|(file, _)| file.clone())
            .collect(),
    );
    parse_pypi_response_with_environment(&filtered, context.environment())
}

fn parse_pypi_response_with_environment(
    value: &Value,
    environment: &MarkerEnvironment,
) -> Result<PackageMetadata> {
    let bad = |reason: &str| IndexError::BadResponse {
        package: value
            .pointer("/info/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        reason: reason.to_string(),
    };
    let info = value.get("info").ok_or_else(|| bad("missing info"))?;
    let name = info
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing name"))?;
    let version = info
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing version"))?;
    let requires_python = info
        .get("requires_python")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut deps = vec![];
    if let Some(dist) = info.get("requires_dist").and_then(Value::as_array) {
        for entry in dist {
            let Some(req_str) = entry.as_str() else {
                continue;
            };
            let req = Requirement::parse(req_str, std::path::Path::new(""))
                .map_err(|error| IndexError::BadRequirement(req_str.to_string(), error))?;
            if !req.evaluate_markers(environment, &[]) {
                continue;
            }
            let constraint = match &req.version_or_url {
                Some(pep508_rs::VersionOrUrl::VersionSpecifier(specifiers)) => {
                    specifiers.to_string()
                }
                Some(pep508_rs::VersionOrUrl::Url(_)) => {
                    return Err(bad(&format!(
                        "dependency {req_str} is URL-based; source dependencies are not implemented"
                    )));
                }
                None => String::new(),
            };
            deps.push(DependencyConstraint {
                name: normalize_py_name(req.name.as_ref()),
                constraint,
            });
        }
    }
    let mut artifacts = vec![];
    if let Some(urls) = value.get("urls").and_then(Value::as_array) {
        for url_entry in urls {
            let filename = url_entry.get("filename").and_then(Value::as_str);
            let url = url_entry.get("url").and_then(Value::as_str);
            let (Some(filename), Some(url)) = (filename, url) else {
                continue;
            };
            let kind = if filename.ends_with(".whl") {
                match parse_wheel_filename(filename) {
                    Some(wheel) => ArtifactKind::Wheel(wheel.tags),
                    None => continue,
                }
            } else {
                ArtifactKind::Sdist
            };
            artifacts.push(ArtifactDescriptor {
                filename: filename.to_string(),
                url: url.to_string(),
                digest: url_entry
                    .pointer("/digests/sha256")
                    .and_then(Value::as_str)
                    .map(|hex| format!("sha256-{hex}")),
                kind,
            });
        }
    }
    if artifacts.is_empty() {
        return Err(bad("no downloadable artifacts"));
    }
    Ok(PackageMetadata {
        name: normalize_py_name(name),
        version: version.to_string(),
        requires_python,
        deps,
        artifacts,
    })
}

/// Rewrite the official artifact host to a mirror preserving its package path.
pub fn rewrite_files_host(url: &str, base: &str) -> String {
    match url.strip_prefix("https://files.pythonhosted.org") {
        Some(rest) => format!("{base}{rest}"),
        None => url.to_string(),
    }
}

/// Raw release names with files, without target-context or Requires-Python filtering.
/// This listing alone does not prove a release is installable in an environment.
pub fn parse_pypi_versions(value: &Value) -> Result<Vec<String>> {
    parse_pypi_versions_matching(value, |_| Ok(true))
}

/// A candidate survives if at least one file permits the full language version.
/// Every supplied file constraint is validated, even after an admitting file.
pub fn parse_pypi_versions_for_context(
    value: &Value,
    context: &PythonMarkerContext,
) -> Result<Vec<String>> {
    parse_pypi_versions_matching(value, |file| match file.get("requires_python") {
        None => Ok(true),
        Some(spec) => check_requires_python_value(spec, context),
    })
}

fn parse_pypi_versions_matching(
    value: &Value,
    accepts_file: impl Fn(&Value) -> Result<bool>,
) -> Result<Vec<String>> {
    let releases = value
        .get("releases")
        .and_then(Value::as_object)
        .ok_or_else(|| IndexError::BadResponse {
            package: value
                .pointer("/info/name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            reason: "missing releases map".to_string(),
        })?;
    let mut versions = Vec::new();
    for (version, files) in releases {
        let Some(files) = files.as_array() else {
            continue;
        };
        let mut accepted = false;
        for file in files {
            accepted |= accepts_file(file)?;
        }
        if accepted {
            versions.push(version.clone());
        }
    }
    versions.sort();
    Ok(versions)
}

fn check_requires_python_value(spec: &Value, context: &PythonMarkerContext) -> Result<bool> {
    match spec {
        Value::Null => Ok(true),
        Value::String(spec) => python_requires_matches_for_context(spec, context),
        _ => Err(IndexError::InvalidPythonSpecifier {
            specifier: spec.to_string(),
            reason: "requires_python must be a string or null".to_string(),
        }),
    }
}

fn requires_matches_version(spec: &str, version: &pep440_rs::Version) -> Result<bool> {
    let specifiers = pep440_rs::VersionSpecifiers::from_str(spec).map_err(|error| {
        IndexError::InvalidPythonSpecifier {
            specifier: spec.to_string(),
            reason: error.to_string(),
        }
    })?;
    Ok(specifiers.contains(version))
}

/// Compare a Requires-Python constraint against the full explicit language version.
/// Invalid specifiers return an error; a valid nonmatching constraint returns false.
/// This check does not establish interpreter provenance or wheel compatibility.
pub fn python_requires_matches_for_context(
    spec: &str,
    context: &PythonMarkerContext,
) -> Result<bool> {
    requires_matches_version(spec, &context.environment().python_full_version().version)
}
