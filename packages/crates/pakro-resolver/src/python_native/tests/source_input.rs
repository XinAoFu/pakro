use super::*;
use std::cell::RefCell;

fn source(name: &str, extras: &[&str], truth: Option<ExtraTruth>) -> Dependency {
    Dependency {
        requirement: Requirement {
            name: name.into(),
            constraint: RequirementConstraint::SourceReference,
            requested_extras: extras.iter().map(|extra| (*extra).into()).collect(),
        },
        extra_truth: truth,
    }
}
fn truth(names: &[&str], enabled: &[usize]) -> Option<ExtraTruth> {
    Some(ExtraTruth {
        extra_names: names.iter().map(|name| (*name).into()).collect(),
        enabled_masks: enabled.into(),
    })
}
fn add(depot: &mut Depot, name: &str, version: &str, id: u8, deps: Vec<Dependency>) {
    depot.candidate(name, version, id, Some(vec![]));
    depot.metadata.get_mut(&key(id)).unwrap().dependencies = deps;
}
struct Reads<'a> {
    depot: &'a Depot,
    projects: RefCell<Vec<String>>,
}
impl MetadataDepot for Reads<'_> {
    fn project(&self, name: &str) -> Option<&[CandidateVersion]> {
        self.projects.borrow_mut().push(name.into());
        self.depot.project(name)
    }
    fn metadata(&self, key: CandidateKey) -> Option<&CandidateMetadata> {
        self.depot.metadata(key)
    }
}
fn step(roots: &[Requirement], depot: &Depot) -> Result<SolveStep, NativeSolveError> {
    solve_step(roots, depot, NativeSolveLimits::default())
}

#[test]
fn an_unrequested_source_extra_and_an_incomplete_and_guard_never_request_its_project() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "1",
        1,
        vec![source("absent-source", &[], truth(&["blue", "red"], &[3]))],
    );
    let reads = Reads {
        depot: &depot,
        projects: RefCell::default(),
    };
    for extras in [vec![], vec!["red".into()]] {
        let roots = [Requirement {
            requested_extras: extras,
            ..req("demo", "")
        }];
        let SolveStep::Complete(plan) = solve_step(&roots, &reads, Default::default()).unwrap()
        else {
            panic!("false guard requested input")
        };
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].key, key(1));
    }
    assert!(reads.projects.borrow().iter().all(|name| name == "demo"));
}

#[test]
fn a_true_source_guard_returns_the_actual_parent_file_and_original_declaration() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "1",
        1,
        vec![source(
            "absent-source",
            &["fast"],
            truth(&["blue", "red"], &[3]),
        )],
    );
    let roots = [Requirement {
        requested_extras: vec!["blue".into(), "red".into()],
        ..req("demo", "")
    }];
    assert_eq!(
        step(&roots, &depot).unwrap(),
        SolveStep::NeedSource {
            parent: key(1),
            declaration: 0
        }
    );
}

#[test]
fn an_unconditional_source_keeps_its_index_after_false_and_ordinary_declarations() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "1",
        1,
        vec![
            source("inactive-source", &[], truth(&["unused"], &[1])),
            Dependency {
                requirement: req("inactive-version", ""),
                extra_truth: truth(&["unused"], &[1]),
            },
            source("absent-source", &["fast"], None),
        ],
    );
    let reads = Reads {
        depot: &depot,
        projects: RefCell::default(),
    };
    assert_eq!(
        solve_step(&[req("demo", "")], &reads, Default::default()).unwrap(),
        SolveStep::NeedSource {
            parent: key(1),
            declaration: 2
        }
    );
    assert!(reads.projects.borrow().iter().all(|name| name == "demo"));
    let dependency = &depot.metadata[&key(1)].dependencies[2].requirement;
    assert_eq!(dependency.name, "absent-source");
    assert_eq!(dependency.requested_extras, ["fast"]);
    assert!(matches!(
        dependency.constraint,
        RequirementConstraint::SourceReference
    ));
}

#[test]
fn equivalent_version_files_do_not_share_the_source_declaration_identity() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "1.0",
        1,
        vec![source("first-source", &[], None)],
    );
    add(
        &mut depot,
        "demo",
        "1.0.0",
        2,
        vec![
            source("inactive-source", &[], truth(&["unused"], &[1])),
            source("second-source", &[], None),
        ],
    );
    assert_eq!(
        step(&[req("demo", "")], &depot).unwrap(),
        SolveStep::NeedSource {
            parent: key(1),
            declaration: 0
        }
    );
    depot.projects.get_mut("demo").unwrap().reverse();
    assert_eq!(
        step(&[req("demo", "")], &depot).unwrap(),
        SolveStep::NeedSource {
            parent: key(2),
            declaration: 1
        }
    );
}

#[test]
fn candidate_backtracking_does_not_request_a_false_source_on_the_abandoned_file() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "2",
        1,
        vec![
            source("inactive-source", &["red"], truth(&["never"], &[1])),
            Dependency::unconditional(req("blocked", ">=1")),
        ],
    );
    add(
        &mut depot,
        "demo",
        "1",
        2,
        vec![source("inactive-source", &[], truth(&["never"], &[1]))],
    );
    depot.projects.insert("blocked".into(), vec![]);
    let plan = complete(&[req("demo", "")], &depot);
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].key, key(2));
    assert!(plan.selected[0].requested_extras.is_empty());
}

#[test]
fn a_runtime_cycle_only_requests_the_source_when_extras_have_actually_grown() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "a",
        "1",
        1,
        vec![
            Dependency::unconditional(req("b", "")),
            source("active-source", &[], truth(&["blue"], &[1])),
        ],
    );
    add(
        &mut depot,
        "b",
        "1",
        2,
        vec![Dependency::unconditional(Requirement {
            requested_extras: vec!["blue".into()],
            ..req("a", "")
        })],
    );
    assert_eq!(
        step(&[req("a", "")], &depot).unwrap(),
        SolveStep::NeedSource {
            parent: key(1),
            declaration: 1
        }
    );
}

#[test]
fn root_source_and_invalid_false_declarations_fail_before_any_project_or_source_request() {
    let depot = Depot::default();
    let reads = Reads {
        depot: &depot,
        projects: RefCell::default(),
    };
    let root = source("root-source", &[], None).requirement;
    assert_eq!(
        solve_step(
            &[req("absent-project", ""), root],
            &reads,
            Default::default()
        )
        .unwrap_err(),
        NativeSolveError::InvalidInput
    );
    assert!(reads.projects.borrow().is_empty());
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "1",
        1,
        vec![
            source("valid-source", &[], None),
            source("INVALID", &[], truth(&["unused"], &[1])),
        ],
    );
    assert_eq!(
        step(&[req("demo", "")], &depot).unwrap_err(),
        NativeSolveError::InvalidInput
    );
    let mut extra = source("valid-source", &["INVALID"], truth(&["unused"], &[1]));
    extra.extra_truth = truth(&["unused"], &[1]);
    depot.metadata.get_mut(&key(1)).unwrap().dependencies[1] = extra;
    assert_eq!(
        step(&[req("demo", "")], &depot).unwrap_err(),
        NativeSolveError::InvalidInput
    );
}

#[test]
fn requires_python_rejection_precedes_source_input_and_source_fields_share_the_limits() {
    let mut depot = Depot::default();
    add(
        &mut depot,
        "demo",
        "2",
        1,
        vec![source("absent-source", &[], None)],
    );
    depot.metadata.get_mut(&key(1)).unwrap().rejection = Some(CandidateRejection::RequiresPython);
    add(&mut depot, "demo", "1", 2, vec![]);
    assert_eq!(complete(&[req("demo", "")], &depot).selected[0].key, key(2));
    for limits in [
        NativeSolveLimits {
            max_dependencies: 1,
            ..Default::default()
        },
        NativeSolveLimits {
            max_field_bytes: 4,
            ..Default::default()
        },
    ] {
        assert_eq!(
            solve_step(&[req("demo", "")], &depot, limits).unwrap_err(),
            NativeSolveError::Limit
        );
    }
}
