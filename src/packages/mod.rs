//! mise-native package management: request parsing, config tables, and the
//! glue between resolver/index/store and the rest of mise. Backends and
//! materialization live in sibling modules as they land.

pub mod install;
pub(crate) mod lockfile;
pub(crate) mod materialize;
pub(crate) mod materialize_node;

use indexmap::IndexMap;

/// One exact-pinned package bound to an exact runtime, the unit the install
/// pipeline works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInstallRequest {
    pub runtime_name: String,
    pub runtime_version: String,
    pub name: String,
    pub version: String,
}

impl PackageInstallRequest {
    /// The `[packages]` group key this request belongs to.
    pub fn group_key(&self) -> String {
        format!("{}@{}", self.runtime_name, self.runtime_version)
    }
}

/// Parsed form of one CLI argument that may carry package bindings. A bare
/// `name@version` is classified as tool or package later, against the
/// registry and config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallArg {
    Bare {
        name: String,
        version: Option<String>,
    },
    Bound {
        tool: String,
        tool_version: Option<String>,
        packages: Vec<(String, String)>,
    },
}

/// Parses `python@3.10.14+numpy@4.2.2` style arguments. Every `+` segment
/// after the first is a package pinned to the leading runtime.
pub fn parse_install_arg(arg: &str) -> Result<InstallArg, String> {
    let segments: Vec<&str> = arg.split('+').collect();
    if segments.len() == 1 {
        let segment = segments[0];
        if segment.is_empty() {
            return Err("empty argument".to_string());
        }
        return Ok(match segment.split_once('@') {
            Some((name, version)) if !name.is_empty() => InstallArg::Bare {
                name: name.to_string(),
                version: if version.is_empty() {
                    None
                } else {
                    Some(version.to_string())
                },
            },
            _ => InstallArg::Bare {
                name: segment.to_string(),
                version: None,
            },
        });
    }
    let (tool, tool_version) = name_version(segments[0], arg)?;
    let mut packages = vec![];
    for segment in &segments[1..] {
        let (name, version) = name_version(segment, arg)?;
        let version = version.expect("name_version rejects empty versions");
        packages.push((name, version));
    }
    Ok(InstallArg::Bound {
        tool,
        tool_version,
        packages,
    })
}

fn name_version(segment: &str, whole: &str) -> Result<(String, Option<String>), String> {
    let (name, version) = segment
        .split_once('@')
        .ok_or_else(|| format!("segment {segment:?} in {whole:?} must be name@version"))?;
    if name.is_empty() || version.is_empty() {
        return Err(format!(
            "segment {segment:?} in {whole:?} must be name@version"
        ));
    }
    Ok((name.to_string(), Some(version.to_string())))
}

/// Builds requests from a `Bound` CLI argument.
pub fn requests_from_bound(
    tool: &str,
    tool_version: Option<&str>,
    packages: &[(String, String)],
) -> Result<Vec<PackageInstallRequest>, String> {
    let tool_version = tool_version.ok_or_else(|| {
        format!("runtime {tool:?} needs an exact version to bind packages, like {tool}@3.10.14")
    })?;
    Ok(packages
        .iter()
        .map(|(name, version)| PackageInstallRequest {
            runtime_name: tool.to_string(),
            runtime_version: tool_version.to_string(),
            name: name.clone(),
            version: version.clone(),
        })
        .collect())
}

/// Turns the `[packages]` config table into requests. Group keys must be
/// `name@version` runtime identities and every pinned version non-empty —
/// the strict-version rule (ADR-0004) enforced at the boundary.
pub fn requests_from_config(
    table: &IndexMap<String, IndexMap<String, String>>,
) -> Result<Vec<PackageInstallRequest>, String> {
    let mut out = vec![];
    for (group, packages) in table {
        let (runtime_name, runtime_version) = group
            .split_once('@')
            .filter(|(name, version)| !name.is_empty() && !version.is_empty())
            .ok_or_else(|| {
                format!(
                    "[packages] group key {group:?} must be an exact runtime like \"python@3.10.14\""
                )
            })?;
        for (name, version) in packages {
            if name.is_empty() || version.is_empty() {
                return Err(format!(
                    "[packages.{group}] entry {name:?} must pin an exact version"
                ));
            }
            out.push(PackageInstallRequest {
                runtime_name: runtime_name.to_string(),
                runtime_version: runtime_version.to_string(),
                name: name.clone(),
                version: version.clone(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_names() {
        assert_eq!(
            parse_install_arg("numpy@4.2.2").unwrap(),
            InstallArg::Bare {
                name: "numpy".into(),
                version: Some("4.2.2".into())
            }
        );
        assert_eq!(
            parse_install_arg("node").unwrap(),
            InstallArg::Bare {
                name: "node".into(),
                version: None
            }
        );
        assert!(parse_install_arg("").is_err());
    }

    #[test]
    fn parses_bound_arguments() {
        assert_eq!(
            parse_install_arg("python@3.10.14+numpy@4.2.2").unwrap(),
            InstallArg::Bound {
                tool: "python".into(),
                tool_version: Some("3.10.14".into()),
                packages: vec![("numpy".into(), "4.2.2".into())]
            }
        );
        let bound = parse_install_arg("python@3.10.14+numpy@4.2.2+scipy@1.11.4").unwrap();
        let InstallArg::Bound { packages, .. } = bound else {
            panic!("expected bound arg");
        };
        assert_eq!(packages.len(), 2);
    }

    #[test]
    fn rejects_malformed_segments() {
        assert!(parse_install_arg("python@3.10.14+numpy").is_err());
        assert!(parse_install_arg("python@3.10.14+numpy@").is_err());
        assert!(parse_install_arg("python@+numpy@4.2.2").is_err());
    }

    #[test]
    fn bound_requests_require_runtime_version() {
        let err = requests_from_bound("python", None, &[("numpy".into(), "4.2.2".into())]);
        assert!(err.unwrap_err().contains("exact version"));
        let reqs = requests_from_bound(
            "python",
            Some("3.10.14"),
            &[("numpy".into(), "4.2.2".into())],
        )
        .unwrap();
        assert_eq!(reqs[0].group_key(), "python@3.10.14");
    }

    #[test]
    fn config_table_becomes_requests() {
        let mut group = IndexMap::new();
        group.insert("numpy".to_string(), "4.2.2".to_string());
        group.insert("beautifulsoup4".to_string(), "4.13.3".to_string());
        let mut table = IndexMap::new();
        table.insert("python@3.10.14".to_string(), group.clone());
        let reqs = requests_from_config(&table).unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(
            reqs.iter()
                .any(|r| r.name == "numpy" && r.version == "4.2.2")
        );

        let mut bad = IndexMap::new();
        bad.insert("python".to_string(), group);
        assert!(requests_from_config(&bad).is_err());
    }
}
