use mise_resolver::{DependencyRequest, ResolvedGraph, ResolvedNode, ResolverError};

fn node(eco: &str, name: &str, version: &str, deps: &[(&str, &str, &str)]) -> ResolvedNode {
    ResolvedNode {
        eco: eco.into(),
        name: name.into(),
        version: version.into(),
        digest: format!("sha256-{name}"),
        deps: deps
            .iter()
            .map(|(eco, name, constraint)| DependencyRequest {
                eco: (*eco).into(),
                name: (*name).into(),
                constraint: (*constraint).into(),
            })
            .collect(),
    }
}

fn diamond() -> ResolvedGraph {
    ResolvedGraph {
        nodes: vec![
            node(
                "python",
                "A",
                "1.0.0",
                &[("python", "B", "1.0"), ("python", "C", "1.0")],
            ),
            node("python", "B", "1.0.0", &[("python", "C", "1.0")]),
            node("python", "C", "1.0.0", &[]),
        ],
    }
}

#[test]
fn install_order_puts_dependencies_first() {
    let order = diamond().install_order().unwrap();
    let pos = |k: &str| order.iter().position(|s| s == k).unwrap();
    assert!(pos("python:C") < pos("python:B"));
    assert!(pos("python:B") < pos("python:A"));
}

#[test]
fn install_order_detects_cycles() {
    let graph = ResolvedGraph {
        nodes: vec![
            node("python", "A", "1.0.0", &[("python", "B", "1.0")]),
            node("python", "B", "1.0.0", &[("python", "A", "1.0")]),
        ],
    };
    assert!(matches!(
        graph.install_order(),
        Err(ResolverError::Cycle(_))
    ));
}

#[test]
fn install_order_rejects_dangling_dependency() {
    let graph = ResolvedGraph {
        nodes: vec![node("python", "A", "1.0.0", &[("python", "ghost", "1.0")])],
    };
    assert!(matches!(
        graph.install_order(),
        Err(ResolverError::MissingNode(_))
    ));
}

#[test]
fn install_order_rejects_duplicate_node() {
    let graph = ResolvedGraph {
        nodes: vec![
            node("python", "A", "1.0.0", &[]),
            node("python", "A", "1.1.0", &[]),
        ],
    };
    assert!(matches!(
        graph.install_order(),
        Err(ResolverError::DuplicateNode(_))
    ));
}

#[test]
fn sorted_is_deterministic() {
    let mut shuffled = diamond();
    shuffled.nodes.reverse();
    assert_eq!(diamond().sorted().nodes, shuffled.sorted().nodes);
}

#[test]
fn serde_roundtrip() {
    let graph = diamond();
    let json = serde_json::to_string(&graph).unwrap();
    assert_eq!(serde_json::from_str::<ResolvedGraph>(&json).unwrap(), graph);
}
