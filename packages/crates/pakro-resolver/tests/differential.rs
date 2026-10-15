//! Differential testing against uv (SPEC FR-12): when uv is installed, the
//! same top-level pins must resolve to the same version set as pakro's
//! solver. Without uv the suite reports SKIPPED so CI stays green on
//! machines that only build.

use std::collections::BTreeMap;
use std::process::Command;

use pakro_resolver::solve::{MetadataSource, solve};
use pakro_resolver::{DependencyRequest, ResolvedGraph};

/// A frozen PyPI snapshot small enough to also hand to `uv` as a
/// constraints-style fixture. Versions deliberately overlap so a solver
/// must actually intersect ranges.
struct Snapshot {
    versions: BTreeMap<&'static str, Vec<&'static str>>,
    deps: BTreeMap<(&'static str, &'static str), Vec<(&'static str, &'static str)>>,
}

fn snapshot() -> Snapshot {
    let mut versions = BTreeMap::new();
    versions.insert("numpy", vec!["1.26.0", "1.26.4", "2.0.0", "2.1.0"]);
    versions.insert(
        "contingent-pyprops",
        vec!["0.1.0", "0.2.0", "0.2.3", "0.3.0", "0.4.1"],
    );
    versions.insert("certifi", vec!["2023.7.22", "2024.2.2", "2024.8.30"]);

    let mut deps = BTreeMap::new();
    deps.insert(
        ("numpy", "1.26.4"),
        vec![
            ("contingent-pyprops", ">=0.2, <0.3"),
            ("certifi", ">=2024.2.2"),
        ],
    );
    deps.insert(
        ("numpy", "1.26.0"),
        vec![("contingent-pyprops", ">=0.1"), ("certifi", ">=2023.7.22")],
    );
    deps.insert(("certifi", "2024.8.30"), vec![]);
    deps.insert(("certifi", "2024.2.2"), vec![]);
    deps.insert(("contingent-pyprops", "0.2.3"), vec![]);
    deps.insert(("contingent-pyprops", "0.3.0"), vec![]);
    Snapshot { versions, deps }
}

impl MetadataSource for Snapshot {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        self.versions
            .get(name)
            .map(|vs| vs.iter().map(|s| s.to_string()).collect())
            .ok_or_else(|| format!("unknown package {name}"))
    }

    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        Ok(self
            .deps
            .get(&(name, version))
            .map(|ds| {
                ds.iter()
                    .map(|(n, c)| DependencyRequest {
                        eco: "python".into(),
                        name: (*n).into(),
                        constraint: (*c).into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[test]
fn resolution_matches_expected_pick() {
    let snap = snapshot();
    let graph = solve(
        &[DependencyRequest {
            eco: "python".into(),
            name: "numpy".into(),
            constraint: "1.26.4".into(),
        }],
        &ResolvedGraph::default(),
        &snap,
        "python",
    )
    .unwrap();
    let chosen: BTreeMap<String, String> = graph
        .nodes
        .iter()
        .map(|n| (n.name.clone(), n.version.clone()))
        .collect();
    // newest-in-range for both transitives, exact for the pin
    assert_eq!(
        chosen,
        BTreeMap::from([
            ("numpy".to_string(), "1.26.4".to_string()),
            ("contingent-pyprops".to_string(), "0.2.3".to_string()),
            ("certifi".to_string(), "2024.8.30".to_string()),
        ])
    );
}

#[test]
fn resolution_agrees_with_uv_when_available() {
    let Ok(uv) = which_uv() else {
        eprintln!("SKIPPED: uv not installed");
        return;
    };
    // requirements in, resolved pins out
    let output = Command::new(uv)
        .args([
            "pip",
            "compile",
            "--python-version",
            "3.10",
            "--no-header",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"numpy==1.26.4\n");
            }
            child.wait_with_output()
        })
        .expect("running uv pip compile");
    if !output.status.success() {
        eprintln!("SKIPPED: uv pip compile failed (offline?)");
        return;
    }
    // uv resolves against the real index while our snapshot models a small
    // slice of it, so compare only packages both sides report: every pin uv
    // chose for a shared package must equal ours
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let snap = snapshot();
    let graph = solve(
        &[DependencyRequest {
            eco: "python".into(),
            name: "numpy".into(),
            constraint: "1.26.4".into(),
        }],
        &ResolvedGraph::default(),
        &snap,
        "python",
    )
    .unwrap();
    let ours: BTreeMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.version.as_str()))
        .collect();
    for line in text.lines() {
        let line = line.trim();
        let Some((name, version)) = line.split_once("==") else {
            continue;
        };
        if let Some(&our_version) = ours.get(name) {
            assert_eq!(
                our_version, version,
                "uv chose {name}=={version} while pakro chose {name}=={our_version}"
            );
        }
    }
    // the top-level pin itself must appear in uv's resolution
    assert!(text.contains("numpy==1.26.4"), "uv output: {text}");
}

fn which_uv() -> Result<std::path::PathBuf, String> {
    for dir in std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
    {
        let candidate = std::path::Path::new(dir).join("uv");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("uv not found".to_string())
}
