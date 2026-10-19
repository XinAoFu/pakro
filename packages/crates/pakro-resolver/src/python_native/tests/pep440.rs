use super::*;

#[test]
fn pre_bounds_match_in_root_feature_and_transitive_requirements() {
    let mut depot = Depot::default();
    depot.candidate("demo", "2.0rc1", 1, Some(vec![]));
    depot.candidate("app", "1.0", 2, Some(vec![req("demo", ">=2.0rc1,<2.0rc2")]));
    let outcomes = [
        vec![req("demo", "<2.0rc2")],
        vec![Requirement {
            requested_extras: vec!["fast".into()],
            ..req("demo", "<2.0rc2")
        }],
        vec![req("app", "==1")],
    ]
    .map(|roots| solve_step(&roots, &depot, NativeSolveLimits::default()));
    assert!(outcomes.iter().all(|result| matches!(result, Ok(SolveStep::Complete(plan)) if plan.selected.iter().any(|node| node.name == "demo" && node.key == key(1)))), "{outcomes:?}");
}
