use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Depot {
    projects: BTreeMap<String, Vec<CandidateVersion>>,
    metadata: BTreeMap<CandidateKey, CandidateMetadata>,
}
impl MetadataDepot for Depot {
    fn project(&self, name: &str) -> Option<&[CandidateVersion]> {
        self.projects.get(name).map(Vec::as_slice)
    }
    fn metadata(&self, key: CandidateKey) -> Option<&CandidateMetadata> {
        self.metadata.get(&key)
    }
}
fn req(name: &str, specifier: &str) -> Requirement {
    Requirement {
        name: name.into(),
        constraint: RequirementConstraint::Version(specifier.into()),
        requested_extras: vec![],
    }
}
fn key(id: u8) -> CandidateKey {
    CandidateKey([id; 32])
}
impl Depot {
    fn candidate(&mut self, name: &str, version: &str, id: u8, deps: Option<Vec<Requirement>>) {
        self.projects
            .entry(name.into())
            .or_default()
            .push(CandidateVersion {
                key: key(id),
                version: version.into(),
            });
        if let Some(deps) = deps {
            self.metadata.insert(
                key(id),
                CandidateMetadata {
                    dependencies: deps.into_iter().map(Dependency::unconditional).collect(),
                    ..Default::default()
                },
            );
        }
    }
}

#[path = "tests/availability.rs"]
mod availability;
#[path = "tests/extras.rs"]
mod extras;
#[path = "tests/pep440.rs"]
mod pep440;
#[path = "tests/raw_specifiers.rs"]
mod raw_specifiers;
#[path = "tests/source_input.rs"]
mod source_input;
fn complete(requests: &[Requirement], depot: &Depot) -> NativePlan {
    let SolveStep::Complete(plan) =
        solve_step(requests, depot, NativeSolveLimits::default()).unwrap()
    else {
        panic!("unexpected missing input")
    };
    plan
}

#[test]
fn a_missing_project_is_a_typed_input_request_instead_of_an_empty_graph() {
    assert_eq!(
        solve_step(
            &[req("demo", ">=1")],
            &Depot::default(),
            NativeSolveLimits::default()
        )
        .unwrap(),
        SolveStep::NeedProject("demo".into())
    );
}
#[test]
fn missing_metadata_pauses_on_the_exact_file_candidate() {
    let mut depot = Depot::default();
    depot.candidate("demo", "1.0", 1, None);
    assert_eq!(
        solve_step(
            &[req("demo", "==1.0")],
            &depot,
            NativeSolveLimits::default()
        )
        .unwrap(),
        SolveStep::NeedMetadata(key(1))
    );
    depot.metadata.insert(key(1), CandidateMetadata::default());
    assert_eq!(
        complete(&[req("demo", "==1.0")], &depot).selected[0].key,
        key(1)
    );
}
#[test]
fn transitive_constraints_backtrack_to_an_earlier_compatible_version() {
    let mut d = Depot::default();
    d.candidate(
        "app",
        "1",
        1,
        Some(vec![req("dep", ">=1"), req("bridge", "==1")]),
    );
    d.candidate("dep", "2", 2, Some(vec![]));
    d.candidate("dep", "1", 3, Some(vec![]));
    d.candidate("bridge", "1", 4, Some(vec![req("dep", "<2")]));
    let p = complete(&[req("app", "==1")], &d);
    assert_eq!(
        p.selected.iter().find(|n| n.name == "dep").unwrap().key,
        key(3)
    );
    assert_eq!(p.selected.len(), 3);
}
#[test]
fn runtime_cycles_are_legal_and_each_real_node_is_emitted_once() {
    let mut d = Depot::default();
    d.candidate("a", "1", 1, Some(vec![req("b", ">=1")]));
    d.candidate("b", "1", 2, Some(vec![req("a", "==1")]));
    let p = complete(&[req("a", "==1")], &d);
    assert_eq!(
        p.selected
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}
#[test]
fn same_version_files_have_independent_metadata_and_backtracking_identity() {
    let mut d = Depot::default();
    d.candidate("demo", "1.0", 1, Some(vec![req("missing", ">=1")]));
    d.candidate("demo", "1.0", 2, Some(vec![]));
    d.projects.insert("missing".into(), vec![]);
    let p = complete(&[req("demo", "==1")], &d);
    assert_eq!(p.selected.len(), 1);
    assert_eq!(p.selected[0].key, key(2));
}
#[test]
fn an_incompatible_complete_depot_reports_no_solution_without_source_text() {
    let mut d = Depot::default();
    d.candidate("a", "1", 1, Some(vec![req("b", "<1")]));
    d.candidate("b", "1", 2, Some(vec![]));
    assert_eq!(
        solve_step(&[req("a", "==1")], &d, NativeSolveLimits::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
    let error = solve_step(
        &[req("a", "@ https://private.invalid/?credential-secret")],
        &d,
        NativeSolveLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error, NativeSolveError::InvalidInput);
    assert!(!error.to_string().contains("credential-secret"));
}
#[test]
fn empty_requests_are_an_explicit_empty_plan_and_do_not_request_inputs() {
    assert_eq!(complete(&[], &Depot::default()).selected.len(), 0);
}
#[test]
fn zero_and_actual_candidate_dependency_and_work_budgets_are_enforced() {
    let mut d = Depot::default();
    d.candidate("a", "1", 1, Some(vec![req("b", ">=1")]));
    d.candidate("a", "2", 2, Some(vec![]));
    d.candidate("b", "1", 3, Some(vec![]));
    let zero = NativeSolveLimits {
        max_projects: 0,
        ..Default::default()
    };
    assert_eq!(
        solve_step(&[req("a", ">=1")], &d, zero).unwrap_err(),
        NativeSolveError::InvalidLimits
    );
    for limits in [
        NativeSolveLimits {
            max_candidates: 1,
            ..Default::default()
        },
        NativeSolveLimits {
            max_field_bytes: 1,
            ..Default::default()
        },
        NativeSolveLimits {
            max_solver_calls: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            solve_step(&[req("a", ">=1")], &d, limits).unwrap_err(),
            NativeSolveError::Limit
        );
    }
}

#[test]
fn root_and_transitive_declarations_share_one_total_dependency_budget() {
    let mut d = Depot::default();
    d.candidate("a", "1", 1, Some(vec![req("b", ">=1"), req("c", ">=1")]));
    d.candidate("b", "1", 2, Some(vec![]));
    d.candidate("c", "1", 3, Some(vec![]));
    let roots = [req("a", "==1")];
    let exact = NativeSolveLimits {
        max_dependencies: 3,
        ..Default::default()
    };
    assert!(matches!(
        solve_step(&roots, &d, exact).unwrap(),
        SolveStep::Complete(_)
    ));
    let tight = NativeSolveLimits {
        max_dependencies: 2,
        ..Default::default()
    };
    assert_eq!(
        solve_step(&roots, &d, tight).unwrap_err(),
        NativeSolveError::Limit
    );
}

#[test]
fn one_candidates_complete_declarations_are_validated_before_requesting_any_child_project() {
    let mut d = Depot::default();
    d.candidate(
        "a",
        "1",
        1,
        Some(vec![
            req("unknown", ">=1"),
            req("private", "@ https://private.invalid/?source-secret"),
        ]),
    );
    assert_eq!(
        solve_step(&[req("a", "==1")], &d, NativeSolveLimits::default()).unwrap_err(),
        NativeSolveError::InvalidInput
    );
}
