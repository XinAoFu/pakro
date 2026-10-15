use std::collections::HashMap;

use pakro_resolver::solve::{MetadataSource, solve};
use pakro_resolver::{DependencyRequest, ResolvedGraph};

struct Fixture {
    versions: HashMap<&'static str, Vec<&'static str>>,
    deps: HashMap<(&'static str, &'static str), Vec<(&'static str, &'static str)>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            versions: HashMap::new(),
            deps: HashMap::new(),
        }
    }

    fn with_versions(mut self, name: &'static str, versions: &[&'static str]) -> Self {
        self.versions.insert(name, versions.to_vec());
        self
    }

    fn with_deps(
        mut self,
        name: &'static str,
        version: &'static str,
        deps: &[(&'static str, &'static str)],
    ) -> Self {
        self.deps.insert((name, version), deps.to_vec());
        self
    }
}

impl MetadataSource for Fixture {
    fn versions(&self, name: &str) -> Result<Vec<String>, String> {
        self.versions
            .get(name)
            .map(|vs| vs.iter().map(|s| s.to_string()).collect())
            .ok_or_else(|| format!("unknown package {name}"))
    }

    fn dependencies(&self, name: &str, version: &str) -> Result<Vec<DependencyRequest>, String> {
        match self.deps.get(&(name, version)) {
            Some(ds) => Ok(ds
                .iter()
                .map(|(n, c)| DependencyRequest {
                    eco: "python".into(),
                    name: (*n).into(),
                    constraint: (*c).into(),
                })
                .collect()),
            // a known package with no entry behaves as "no dependencies",
            // matching registries where every release carries metadata
            None if self.versions.contains_key(name) => Ok(vec![]),
            None => Err(format!("no metadata for {name} {version}")),
        }
    }
}

fn request(name: &str, constraint: &str) -> DependencyRequest {
    DependencyRequest {
        eco: "python".into(),
        name: name.into(),
        constraint: constraint.into(),
    }
}

fn chosen(graph: &ResolvedGraph, name: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("node {name} missing"))
        .version
        .clone()
}

#[test]
fn exact_pin_resolves_single_package() {
    let fixture = Fixture::new().with_versions("numpy", &["4.2.1", "4.2.2", "4.3.0"]);
    let graph = solve(
        &[request("numpy", "4.2.2")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap();
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(chosen(&graph, "numpy"), "4.2.2");
}

#[test]
fn transitive_range_picks_max_in_range() {
    let fixture = Fixture::new()
        .with_versions("numpy", &["1.0.0"])
        .with_versions("contingent-pyprops", &["0.1.0", "0.2.0", "0.3.0", "0.4.0"])
        .with_deps("numpy", "1.0.0", &[("contingent-pyprops", ">=0.2, <0.4")]);
    let graph = solve(
        &[request("numpy", "1.0.0")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap();
    assert_eq!(chosen(&graph, "contingent-pyprops"), "0.3.0");
}

#[test]
fn shared_dependency_intersects_ranges() {
    let fixture = Fixture::new()
        .with_versions("a", &["1.0.0"])
        .with_versions("b", &["1.0.0"])
        .with_versions("c", &["1.0.0", "2.0.0", "3.0.0"])
        .with_deps("a", "1.0.0", &[("c", ">=1, <3")])
        .with_deps("b", "1.0.0", &[("c", ">=2")]);
    let graph = solve(
        &[request("a", "1.0.0"), request("b", "1.0.0")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap();
    assert_eq!(chosen(&graph, "c"), "2.0.0");
}

#[test]
fn locked_choice_is_preferred_when_still_valid() {
    let fixture = Fixture::new()
        .with_versions("a", &["1.0.0"])
        .with_versions("c", &["1.0.0", "2.0.0", "3.0.0"])
        .with_deps("a", "1.0.0", &[("c", ">=1, <3")]);
    let locked = serde_json::from_str(
        r#"{"nodes":[{"eco":"python","name":"c","version":"1.0.0","digest":null,"deps":[]}]}"#,
    )
    .unwrap();
    let graph = solve(&[request("a", "1.0.0")], &locked, &fixture, "python").unwrap();
    assert_eq!(chosen(&graph, "c"), "1.0.0");

    // a lock outside the current constraints must not win
    let stale = serde_json::from_str(
        r#"{"nodes":[{"eco":"python","name":"c","version":"3.5.0","digest":null,"deps":[]}]}"#,
    )
    .unwrap();
    let graph = solve(&[request("a", "1.0.0")], &stale, &fixture, "python").unwrap();
    assert_eq!(chosen(&graph, "c"), "2.0.0");
}

#[test]
fn conflicting_constraints_report_both_sides() {
    let fixture = Fixture::new()
        .with_versions("a", &["1.0.0"])
        .with_versions("b", &["1.0.0"])
        .with_versions("c", &["1.0.0", "2.0.0"])
        .with_deps("a", "1.0.0", &[("c", "<2")])
        .with_deps("b", "1.0.0", &[("c", ">=2")]);
    let err = solve(
        &[request("a", "1.0.0"), request("b", "1.0.0")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap_err();
    let report = err.to_string();
    assert!(report.contains("a"), "report mentions a: {report}");
    assert!(report.contains("b"), "report mentions b: {report}");
    assert!(report.contains("c"), "report mentions c: {report}");
}

#[test]
fn top_level_requests_must_be_exact_versions() {
    let fixture = Fixture::new().with_versions("numpy", &["1.0.0"]);
    let err = solve(
        &[request("numpy", ">=1.0")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap_err();
    assert!(err.to_string().contains("exact"), "{err}");
}

#[test]
fn solved_graph_nodes_carry_dep_constraints() {
    let fixture = Fixture::new()
        .with_versions("numpy", &["1.0.0"])
        .with_versions("dep", &["0.2.0"])
        .with_deps("numpy", "1.0.0", &[("dep", ">=0.2")]);
    let graph = solve(
        &[request("numpy", "1.0.0")],
        &ResolvedGraph::default(),
        &fixture,
        "python",
    )
    .unwrap();
    let numpy = graph.nodes.iter().find(|n| n.name == "numpy").unwrap();
    assert_eq!(numpy.deps.len(), 1);
    assert_eq!(numpy.deps[0].name, "dep");
    assert_eq!(numpy.deps[0].constraint, ">=0.2");
    // digest is filled by artifact selection, never by the solver
    assert!(numpy.digest.is_none());
}

#[test]
fn node_ecosystem_dispatch() {
    let fixture = Fixture::new().with_versions("typescript", &["5.8.1", "5.8.2"]);
    let graph = solve(
        &[request("typescript", "5.8.2")],
        &ResolvedGraph::default(),
        &fixture,
        "node",
    )
    .unwrap();
    assert_eq!(chosen(&graph, "typescript"), "5.8.2");
    assert_eq!(graph.nodes[0].eco, "node");

    // npm caret semantics: ^5.8.1 allows any 5.x >= 5.8.1, so 5.9.0 wins
    let fixture = Fixture::new()
        .with_versions("root", &["1.0.0"])
        .with_versions("typescript", &["5.8.0", "5.8.2", "5.9.0"])
        .with_deps("root", "1.0.0", &[("typescript", "^5.8.1")]);
    let graph = solve(
        &[request("root", "1.0.0")],
        &ResolvedGraph::default(),
        &fixture,
        "node",
    )
    .unwrap();
    assert_eq!(chosen(&graph, "typescript"), "5.9.0");
}
