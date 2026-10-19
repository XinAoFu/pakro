use super::*;

#[test]
fn arbitrary_root_literal_cannot_admit_a_normalized_different_rhs() {
    let mut depot = Depot::default();
    depot.candidate("demo", "1.0", 1, Some(vec![]));
    assert_eq!(
        complete(&[req("demo", "===1.0")], &depot).selected[0].key,
        key(1)
    );
    assert_eq!(
        solve_step(&[req("demo", "===01.0")], &depot, Default::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
}

#[test]
fn arbitrary_dependency_and_feature_use_the_same_original_literal() {
    let mut depot = Depot::default();
    depot.candidate("demo", "1.0", 1, Some(vec![]));
    depot.candidate("app", "1.0", 2, Some(vec![req("demo", "===01.0")]));
    assert_eq!(
        complete(&[req("demo", "==1")], &depot).selected[0].key,
        key(1)
    );
    assert_eq!(
        solve_step(&[req("app", "==1")], &depot, Default::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
    let roots = [Requirement {
        requested_extras: vec!["fast".into()],
        ..req("demo", "===01.0")
    }];
    assert_eq!(
        solve_step(&roots, &depot, Default::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
}

#[test]
fn conflicting_literal_conjunction_cannot_collapse_to_one_normalized_clause() {
    let mut depot = Depot::default();
    depot.candidate("demo", "01.0", 1, Some(vec![]));
    assert_eq!(
        complete(&[req("demo", "==1.0")], &depot).selected[0].key,
        key(1)
    );
    assert_eq!(
        solve_step(&[req("demo", "===01.0,===1.0")], &depot, Default::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
}
