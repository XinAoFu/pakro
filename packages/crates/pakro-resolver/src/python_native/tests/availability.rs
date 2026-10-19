use super::*;

fn requests() -> [Requirement; 2] {
    [
        req("demo", ">=1"),
        Requirement {
            requested_extras: vec!["fast".into()],
            ..req("demo", ">=1")
        },
    ]
}

fn reject(depot: &mut Depot, id: u8) {
    depot.metadata.get_mut(&key(id)).unwrap().rejection = Some(CandidateRejection::RequiresPython);
}

#[test]
fn requires_python_rejection_backtracks_to_another_version_with_or_without_features() {
    let mut depot = Depot::default();
    depot.candidate("demo", "2.0", 1, Some(vec![]));
    depot.candidate("demo", "1.0", 2, Some(vec![]));
    reject(&mut depot, 1);
    let plans = requests().map(|request| complete(&[request], &depot));
    assert_eq!(
        plans.each_ref().map(|plan| plan.selected[0].key),
        [key(2); 2]
    );
    assert_eq!(plans[0].selected.len(), 1);
    assert_eq!(plans[1].selected.len(), 1);
    assert_eq!(plans[0].selected[0].requested_extras, Vec::<String>::new());
    assert_eq!(plans[1].selected[0].requested_extras, ["fast"]);
}

#[test]
fn requires_python_rejection_backtracks_to_a_different_file_of_the_same_version() {
    let mut depot = Depot::default();
    depot.candidate("demo", "1.0", 1, Some(vec![]));
    depot.candidate("demo", "1.0", 2, Some(vec![]));
    reject(&mut depot, 1);
    let plans = requests().map(|request| complete(&[request], &depot));
    assert_eq!(
        plans.each_ref().map(|plan| plan.selected[0].key),
        [key(2); 2]
    );
    assert_eq!(plans[1].selected[0].requested_extras, ["fast"]);
    assert!(plans.iter().all(|plan| plan.selected.len() == 1));
}

#[test]
fn all_rejected_candidates_are_no_solution_without_requesting_their_children() {
    let mut depot = Depot::default();
    depot.candidate("demo", "2.0", 1, Some(vec![req("absent-child", ">=1")]));
    depot.candidate("demo", "1.0", 2, Some(vec![req("absent-child", ">=1")]));
    reject(&mut depot, 1);
    reject(&mut depot, 2);
    let outcomes =
        requests().map(|request| solve_step(&[request], &depot, NativeSolveLimits::default()));
    assert_eq!(
        outcomes,
        [
            Err(NativeSolveError::NoSolution),
            Err(NativeSolveError::NoSolution)
        ]
    );
}
