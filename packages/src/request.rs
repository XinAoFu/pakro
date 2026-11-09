//! Request types and parsing: CLI argument shapes (`runtime@ver+pkg@ver`),
//! config table conversion, and bare-name classification.

use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub static CLI_PACKAGE_ARGS: OnceLock<Vec<PackageInstallRequest>> = OnceLock::new();

/// Rewrites bound arguments (`runtime@ver+pkg@ver[+…]`) in place to their
/// runtime-only form and records the package pins. Plain tool arguments are
/// left untouched; returns whether any pins were recorded.
pub fn extract_bound_package_args(args: &mut [String]) -> bool {
    let mut requests = vec![];
    for arg in args.iter_mut() {
        if !arg.contains('+') || !arg.contains('@') {
            continue;
        }
        if let Ok(InstallArg::Bound {
            tool,
            tool_version: Some(version),
            packages,
        }) = parse_install_arg(arg)
            && let Ok(bound) = requests_from_bound(&tool, Some(&version), &packages)
        {
            requests.extend(bound);
            *arg = format!("{tool}@{version}");
        }
    }
    if requests.is_empty() {
        false
    } else {
        let _ = CLI_PACKAGE_ARGS.set(requests);
        true
    }
}

/// One exact-pinned package bound to an exact runtime, the unit the install
/// pipeline works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInstallRequest {
    pub runtime_name: String,
    pub runtime_version: String,
    pub name: String,
    pub version: String,
    pub source: PackageSource,
    pub channel: Option<String>,
    pub build: Option<String>,
    pub extras: Vec<String>,
    pub marker: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    #[default]
    Pypi,
    Conda,
}

/// The same declaration value is decoded by ordinary and static configuration
/// readers. Neither reader may discard options by rebuilding a version map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageDeclaration {
    Version(String),
    Detailed(PackageDeclarationOptions),
}

impl<'de> Deserialize<'de> for PackageDeclaration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DeclarationVisitor;
        impl<'de> serde::de::Visitor<'de> for DeclarationVisitor {
            type Value = PackageDeclaration;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .write_str("an exact package version string or a package declaration table")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(PackageDeclaration::Version(value.to_owned()))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(PackageDeclaration::Version(value))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                PackageDeclarationOptions::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )
                .map(PackageDeclaration::Detailed)
            }
        }
        deserializer.deserialize_any(DeclarationVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDeclarationOptions {
    pub version: String,
    #[serde(default)]
    pub source: PackageSource,
    pub channel: Option<String>,
    pub build: Option<String>,
    #[serde(default)]
    pub extras: Vec<String>,
    pub marker: Option<String>,
}

impl From<String> for PackageDeclaration {
    fn from(version: String) -> Self {
        Self::Version(version)
    }
}

pub type PackageConfigTable = IndexMap<String, IndexMap<String, PackageDeclaration>>;

impl PackageInstallRequest {
    pub fn pypi(
        runtime_name: impl Into<String>,
        runtime_version: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            runtime_name: runtime_name.into(),
            runtime_version: runtime_version.into(),
            name: name.into(),
            version: version.into(),
            source: PackageSource::Pypi,
            channel: None,
            build: None,
            extras: Vec::new(),
            marker: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        use pakro_index::python_markers::{
            PythonMarkerField, PythonMarkerLimits, PythonMarkerOperand, parse_python_marker,
        };
        use pakro_index::python_source::NormalizedPythonName;
        if self.runtime_name.is_empty() || self.runtime_version.is_empty() {
            return Err("package declarations require an explicit runtime binding".into());
        }
        if self.name.len() > 4096 || self.version.len() > 4096 {
            return Err("package declaration exceeds its text limit".into());
        }
        if self.source == PackageSource::Conda {
            if self.name.is_empty() {
                return Err("Conda package name cannot be empty".into());
            }
            rattler_conda_types::PackageName::try_from(self.name.as_str())
                .map_err(|error| format!("invalid Conda package name {:?}: {error}", self.name))?;
            if !self.version.starts_with(|c: char| c.is_ascii_digit())
                || self.version.chars().any(|c| {
                    c.is_whitespace() || matches!(c, '*' | '<' | '>' | '=' | ',' | '|' | '~')
                })
                || self
                    .version
                    .parse::<rattler_conda_types::Version>()
                    .is_err()
            {
                return Err(format!(
                    "Conda package {} requires a complete exact version",
                    self.name
                ));
            }
            if !self.extras.is_empty() {
                return Err("Conda packages do not support PyPI extras".into());
            }
            if self
                .channel
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.trim() != s)
                || self.build.as_ref().is_some_and(|s| {
                    s.is_empty()
                        || s.trim() != s
                        || s.chars()
                            .any(|c| c.is_whitespace() || matches!(c, '*' | '|' | ','))
                })
            {
                return Err("Conda channel and build must be nonempty exact values".into());
            }
        } else {
            if self.channel.is_some() || self.build.is_some() {
                return Err("channel and build are only supported for Conda packages".into());
            }
            NormalizedPythonName::new(&self.name)
                .map_err(|error| format!("invalid package name {:?}: {error}", self.name))?;
            if self.version.is_empty()
                || self.version.trim() != self.version
                || self.version.parse::<pep440_rs::Version>().is_err()
            {
                return Err(format!(
                    "package {} requires a complete exact version, not a range or selector",
                    self.name
                ));
            }
        }
        if self.extras.len() > 256 {
            return Err("package extras exceed the declaration limit".into());
        }
        let mut extras = BTreeSet::new();
        for extra in &self.extras {
            if extra.len() > 4096 {
                return Err("package extra exceeds its text limit".into());
            }
            let normalized = NormalizedPythonName::new(extra)
                .map_err(|error| format!("invalid package extra {extra:?}: {error}"))?;
            if !extras.insert(normalized.as_str().to_owned()) {
                return Err(format!("duplicate normalized package extra {extra:?}"));
            }
        }
        if let Some(marker) = &self.marker {
            let expression = parse_python_marker(marker, PythonMarkerLimits::default())
                .map_err(|error| format!("invalid package marker: {error}"))?;
            if expression.comparisons().any(|comparison| {
                [comparison.left(), comparison.right()]
                    .iter()
                    .any(|operand| {
                        matches!(
                            operand,
                            PythonMarkerOperand::Field(
                                PythonMarkerField::Extra
                                    | PythonMarkerField::Extras
                                    | PythonMarkerField::DependencyGroups
                            )
                        )
                    })
            }) {
                return Err(
                    "project package markers cannot reference extra, extras or dependency_groups"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// A complete requirement; evaluation belongs to the actual interpreter
    /// target, never to configuration parsing or host defaults.
    pub fn requirement(&self) -> Result<String, String> {
        use pakro_index::python_source::NormalizedPythonName;
        self.validate()?;
        if self.source == PackageSource::Conda {
            return Err("Conda package installation is not supported yet".into());
        }
        let name = NormalizedPythonName::new(&self.name).map_err(|error| error.to_string())?;
        let mut extras = self
            .extras
            .iter()
            .map(|extra| {
                NormalizedPythonName::new(extra)
                    .map(|name| name.as_str().to_owned())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        extras.sort();
        let mut requirement = name.as_str().to_owned();
        if !extras.is_empty() {
            requirement.push('[');
            requirement.push_str(&extras.join(","));
            requirement.push(']');
        }
        requirement.push_str("==");
        requirement.push_str(&self.version);
        if let Some(marker) = &self.marker {
            requirement.push_str("; ");
            requirement.push_str(marker);
        }
        Ok(requirement)
    }

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
    packages
        .iter()
        .map(|(name, version)| {
            let request = PackageInstallRequest::pypi(tool, tool_version, name, version);
            request.validate()?;
            Ok(request)
        })
        .collect()
}

/// Turns the `[packages]` config table into requests. Group keys must be
/// `name@version` runtime identities and every pinned version non-empty —
/// the strict-version rule (ADR-0004) enforced at the boundary.
pub fn requests_from_config(
    table: &PackageConfigTable,
) -> Result<Vec<PackageInstallRequest>, String> {
    let mut out = vec![];
    for (group, packages) in table {
        let (runtime_name, runtime_version) = group
            .split_once('@')
            .filter(|(name, version)| !name.is_empty() && !version.is_empty())
            .ok_or_else(|| {
                format!(
                    "[packages] group key {group:?} must name a runtime like \"python@3.10.14\""
                )
            })?;
        let mut pypi_names = BTreeSet::new();
        let mut conda_names = BTreeSet::new();
        for (name, declaration) in packages {
            let mut request = match declaration {
                PackageDeclaration::Version(version) => {
                    PackageInstallRequest::pypi(runtime_name, runtime_version, name, version)
                }
                PackageDeclaration::Detailed(options) => PackageInstallRequest {
                    runtime_name: runtime_name.to_owned(),
                    runtime_version: runtime_version.to_owned(),
                    name: name.clone(),
                    version: options.version.clone(),
                    source: options.source,
                    channel: options.channel.clone(),
                    build: options.build.clone(),
                    extras: options.extras.clone(),
                    marker: options.marker.clone(),
                },
            };
            request.validate()?;
            let conflict = if request.source == PackageSource::Conda {
                !conda_names.insert(name.to_ascii_lowercase())
            } else {
                let normalized = pakro_index::python_source::NormalizedPythonName::new(name)
                    .map_err(|error| error.to_string())?
                    .as_str()
                    .to_owned();
                !pypi_names.insert(normalized)
            };
            if conflict {
                return Err(format!(
                    "conflicting normalized package name {name:?} in [packages.{group}]"
                ));
            }
            for extra in &mut request.extras {
                *extra = pakro_index::python_source::NormalizedPythonName::new(extra)
                    .map_err(|error| error.to_string())?
                    .as_str()
                    .to_owned();
            }
            request.extras.sort();
            out.push(request);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarePackageBindingError {
    MissingRuntime,
    AmbiguousRuntime { candidate_count: usize },
}

/// Choose among already-classified package-capable candidates without IO.
/// Both the legacy argument adapter and the program request boundary use this
/// rule; installed-path and lock-owner validation belongs to the later binder.
pub(crate) fn select_bare_package_runtime<T>(
    runtimes: &[T],
) -> Result<&T, BarePackageBindingError> {
    match runtimes {
        [] => Err(BarePackageBindingError::MissingRuntime),
        [runtime] => Ok(runtime),
        many => Err(BarePackageBindingError::AmbiguousRuntime {
            candidate_count: many.len(),
        }),
    }
}

/// Splits CLI tool arguments into real tools and bare package pins. An
/// argument naming something pakro cannot resolve as a backend is a package
/// candidate; it binds to the single package-capable runtime
/// (`python`/`node`) among the given runtimes. No runtime → instructions;
/// several → the bound `+` syntax is required.
pub fn split_bare_packages(
    tools: Vec<crate::args::ToolArg>,
    runtimes: &[(String, String)],
    is_tool: impl Fn(&str) -> bool,
) -> Result<(Vec<crate::args::ToolArg>, Vec<PackageInstallRequest>), String> {
    let mut kept = vec![];
    let mut pins = vec![];
    for tool in tools {
        if is_tool(&tool.ba.short) {
            kept.push(tool);
            continue;
        }
        let Some(request) = &tool.tvr else {
            // no version at all: keep it so the normal tool error explains
            kept.push(tool);
            continue;
        };
        match select_bare_package_runtime(runtimes) {
            Err(BarePackageBindingError::MissingRuntime) => {
                return Err(format!(
                    "{:?} looks like a package but no package-capable runtime (python/node) is declared; declare an exact Python version in pakro.toml first",
                    tool.ba.short
                ));
            }
            Ok((name, version)) => {
                let pin =
                    PackageInstallRequest::pypi(name, version, &tool.ba.short, request.version());
                pin.validate()?;
                pins.push(pin);
            }
            Err(BarePackageBindingError::AmbiguousRuntime { .. }) => {
                let many = runtimes;
                return Err(format!(
                    "{:?} is ambiguous between multiple runtimes ({}); use the bound syntax like `{}@{}+{}@{}`",
                    tool.ba.short,
                    many.iter()
                        .map(|(n, v)| format!("{n}@{v}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    many[0].0,
                    many[0].1,
                    tool.ba.short,
                    request.version(),
                ));
            }
        }
    }
    Ok((kept, pins))
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
    fn bound_args_are_rewritten_in_place() {
        let mut args = vec![
            "install".to_string(),
            "python@3.10.14+numpy@4.2.2+scipy@1.11.4".to_string(),
            "node@22.14.0".to_string(),
        ];
        assert!(extract_bound_package_args(&mut args));
        assert_eq!(args[1], "python@3.10.14");
        assert_eq!(args[2], "node@22.14.0");

        let mut plain = vec!["install".to_string(), "node@22.14.0".to_string()];
        assert!(!extract_bound_package_args(&mut plain));
        assert_eq!(plain[1], "node@22.14.0");
    }

    #[test]
    fn bare_names_split_by_known_backend_and_runtime() {
        use crate::args::ToolArg;
        use std::str::FromStr;
        let tools = vec![
            ToolArg::from_str("node@22.14.0").unwrap(),
            ToolArg::from_str("numpy@1.26.4").unwrap(),
        ];
        let (kept, pins) =
            split_bare_packages(tools, &[("python".into(), "3.10.14".into())], |short| {
                short == "node"
            })
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].ba.short, "node");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].name, "numpy");
        assert_eq!(pins[0].version, "1.26.4");
        assert_eq!(pins[0].group_key(), "python@3.10.14");
    }

    #[test]
    fn bare_names_need_exactly_one_runtime() {
        use crate::args::ToolArg;
        use std::str::FromStr;
        let tools = vec![ToolArg::from_str("numpy@1.26.4").unwrap()];
        let err = split_bare_packages(tools, &[], |_| false).unwrap_err();
        assert!(err.contains("no package-capable runtime"), "{err}");

        let tools = vec![ToolArg::from_str("numpy@1.26.4").unwrap()];
        let err = split_bare_packages(
            tools,
            &[
                ("python".into(), "3.10.14".into()),
                ("node".into(), "22.14.0".into()),
            ],
            |_| false,
        )
        .unwrap_err();
        assert!(err.contains("bound syntax"), "{err}");
    }

    #[test]
    fn config_table_becomes_requests() {
        let mut group = IndexMap::new();
        group.insert(
            "numpy".to_string(),
            PackageDeclaration::Version("4.2.2".into()),
        );
        group.insert(
            "beautifulsoup4".to_string(),
            PackageDeclaration::Version("4.13.3".into()),
        );
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

    fn config_requests(raw: &str) -> Result<Vec<PackageInstallRequest>, String> {
        let table: PackageConfigTable = toml::from_str(raw).map_err(|error| error.to_string())?;
        requests_from_config(&table)
    }

    #[test]
    fn p3_string_and_table_declarations_share_validation_and_keep_options() {
        let plain = config_requests("['python@3.13.16']\ndemo='1.0'\n").unwrap();
        let detailed =
            config_requests("['python@3.13.16']\ndemo={version='1.0',source='pypi'}\n").unwrap();
        assert_eq!(plain, detailed);
        let options = config_requests("['python@3.13.16']\nDemo_Pkg={version='1.0',extras=['Foo_Bar','baz'],marker=\"python_version >= '3.13'\"}\n").unwrap();
        assert_eq!(options[0].source, PackageSource::Pypi);
        assert_eq!(options[0].extras, ["baz", "foo-bar"]);
        assert_eq!(
            options[0].requirement().unwrap(),
            "demo-pkg[baz,foo-bar]==1.0; python_version >= '3.13'"
        );
        let parsed = pakro_index::python_requirements::parse_python_requirement(
            &options[0].requirement().unwrap(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            parsed.requested_extras().collect::<Vec<_>>(),
            ["baz", "foo-bar"]
        );
        assert!(parsed.marker().is_some());
    }

    #[test]
    fn p3_invalid_declarations_never_become_empty_or_another_source() {
        for value in [
            "''",
            "'>=1.0'",
            "'latest'",
            "'1.*'",
            "' 1.0'",
            "{version='1.0',unknown=true}",
            "{version='1.0',source='private'}",
            "{version='>=1.0',source='conda'}",
            "{version='latest',source='conda'}",
            "{version='1.0',source='conda',build='py*'}",
            "{version='1.0',source='conda',extras=['feature']}",
            "{version='1.0',channel='conda-forge'}",
            "{version='1.0',build='py_0'}",
            "{extras=['feature']}",
            "{version='1.0',extras=['feature','Feature']}",
            "{version='1.0',marker='unknown_field == 1'}",
            "{version='1.0',marker=\"extra == 'feature'\"}",
            "{version='1.0',marker=\"'feature' in dependency_groups\"}",
        ] {
            assert!(
                config_requests(&format!("['python@3.13.16']\ndemo={value}\n")).is_err(),
                "accepted {value}"
            );
        }
        let requests = config_requests("['python@3.13.16']\ndemo={version='1.0',source='conda',channel='conda-forge',build='py_0'}\n")
            .unwrap();
        assert_eq!(requests[0].source, PackageSource::Conda);
        assert_eq!(requests[0].channel.as_deref(), Some("conda-forge"));
        assert_eq!(requests[0].build.as_deref(), Some("py_0"));
        assert!(
            requests[0]
                .requirement()
                .unwrap_err()
                .contains("Conda package installation is not supported")
        );
        let mut empty = requests[0].clone();
        empty.name.clear();
        assert!(empty.validate().is_err());
        assert!(config_requests("['python@3.13.16']\nDemo_Pkg='1.0'\ndemo-pkg='1.0'\n").is_err());
        assert!(config_requests("['python@3.13.16']\nDemo_Pkg='1.0'\ndemo-pkg={version='1.0',source='conda'}\n").is_ok());
        assert!(config_requests("['python@3.13.16']\ndemo-pkg={version='1.0',source='conda'}\nDemo_Pkg='1.0'\n").is_ok());
        assert!(config_requests("['python@3.13.16']\nDemo_Pkg={version='1.0',source='conda'}\ndemo_pkg={version='1.0',source='conda'}\n").is_err());
        assert!(config_requests("['python@3.13.16']\ndemo_pkg={version='1.0',source='conda'}\ndemo-pkg={version='1.0',source='conda'}\n").is_ok());
    }

    #[test]
    fn p3_cli_string_requests_are_explicit_pypi_defaults_and_still_exact() {
        let request =
            requests_from_bound("python", Some("3.13.16"), &[("demo".into(), "1.0".into())])
                .unwrap();
        assert_eq!(request[0].source, PackageSource::Pypi);
        assert!(request[0].extras.is_empty());
        assert!(request[0].marker.is_none());
        assert!(
            requests_from_bound(
                "python",
                Some("3.13.16"),
                &[("demo".into(), ">=1.0".into())]
            )
            .is_err()
        );
    }
}
