use super::*;

fn request(name: &str, extras: &[&str]) -> Requirement {
    let mut request = req(name, "");
    request.requested_extras = extras.iter().map(|name| (*name).to_owned()).collect();
    request
}
fn dependency(name: &str, extras: &[&str]) -> Dependency {
    Dependency::unconditional(request(name, extras))
}
fn conditional(name: &str, names: &[&str], enabled: &[usize]) -> Dependency {
    Dependency {
        requirement: request(name, &[]),
        extra_truth: Some(ExtraTruth {
            extra_names: names.iter().map(|name| (*name).to_owned()).collect(),
            enabled_masks: enabled.to_vec(),
        }),
    }
}
fn add(d: &mut Depot, name: &str, version: &str, id: u8, dependencies: Vec<Dependency>) {
    d.candidate(name, version, id, Some(vec![]));
    d.metadata.get_mut(&key(id)).unwrap().dependencies = dependencies;
}
fn names(plan: &NativePlan) -> Vec<&str> {
    plan.selected
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect()
}
fn extras<'a>(plan: &'a NativePlan, name: &str) -> Vec<&'a str> {
    plan.selected
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap()
        .requested_extras
        .iter()
        .map(String::as_str)
        .collect()
}

#[test]
fn a_single_requested_feature_activates_its_truth_and_emits_only_real_distributions() {
    let mut d = Depot::default();
    add(
        &mut d,
        "demo",
        "1",
        1,
        vec![conditional("child", &["fast"], &[1])],
    );
    add(&mut d, "child", "1", 2, vec![]);
    let plan = complete(&[request("demo", &["fast"])], &d);
    assert_eq!(names(&plan), vec!["child", "demo"]);
    assert_eq!(extras(&plan, "demo"), vec!["fast"]);
    assert!(extras(&plan, "child").is_empty());
    assert_eq!(
        plan.selected.iter().find(|c| c.name == "demo").unwrap().key,
        key(1)
    );
}

#[test]
fn extras_from_separate_parents_intersect_guard_constraints_for_and_and_or() {
    let mut d = Depot::default();
    add(&mut d, "a", "1", 1, vec![dependency("shared", &["red"])]);
    add(&mut d, "b", "1", 2, vec![dependency("shared", &["blue"])]);
    add(
        &mut d,
        "shared",
        "1",
        3,
        vec![
            conditional("and-child", &["blue", "red"], &[3]),
            conditional("or-child", &["blue", "red"], &[1, 2, 3]),
        ],
    );
    add(&mut d, "and-child", "1", 4, vec![]);
    add(&mut d, "or-child", "1", 5, vec![]);
    let plan = complete(&[request("a", &[]), request("b", &[])], &d);
    assert_eq!(
        names(&plan),
        vec!["a", "and-child", "b", "or-child", "shared"]
    );
    assert_eq!(extras(&plan, "shared"), vec!["blue", "red"]);
}

#[test]
fn one_extra_does_not_activate_an_and_guard_or_request_its_absent_child_project() {
    let mut d = Depot::default();
    add(
        &mut d,
        "demo",
        "1",
        1,
        vec![conditional("never-loaded", &["blue", "red"], &[3])],
    );
    let plan = complete(&[request("demo", &["red"])], &d);
    assert_eq!(names(&plan), vec!["demo"]);
    assert_eq!(extras(&plan, "demo"), vec!["red"]);
    // The child has no project input at all. Any attempted fetch would have
    // returned NeedProject, rather than the required complete plan.
    assert!(!d.projects.contains_key("never-loaded"));
}

#[test]
fn runtime_cycles_can_grow_extras_until_the_conditional_edge_is_enabled() {
    let mut d = Depot::default();
    add(
        &mut d,
        "a",
        "1",
        1,
        vec![
            dependency("b", &["blue"]),
            conditional("leaf", &["red"], &[1]),
        ],
    );
    let mut back = conditional("a", &["blue"], &[1]);
    back.requirement.requested_extras.push("red".into());
    add(&mut d, "b", "1", 2, vec![back]);
    add(&mut d, "leaf", "1", 3, vec![]);
    let plan = complete(&[request("a", &[])], &d);
    assert_eq!(names(&plan), vec!["a", "b", "leaf"]);
    assert_eq!(extras(&plan, "a"), vec!["red"]);
    assert_eq!(extras(&plan, "b"), vec!["blue"]);
}

#[test]
fn version_backtracking_retracts_an_abandoned_feature_instead_of_sticking_it_globally() {
    let mut d = Depot::default();
    add(&mut d, "app", "2", 1, vec![dependency("shared", &["red"])]);
    add(&mut d, "app", "1", 2, vec![dependency("shared", &["blue"])]);
    add(
        &mut d,
        "shared",
        "1",
        3,
        vec![
            conditional("bad", &["red"], &[1]),
            conditional("good", &["blue"], &[1]),
        ],
    );
    d.projects.insert("bad".into(), vec![]);
    add(&mut d, "good", "1", 4, vec![]);
    let plan = complete(&[request("app", &[])], &d);
    assert_eq!(names(&plan), vec!["app", "good", "shared"]);
    assert_eq!(
        plan.selected.iter().find(|c| c.name == "app").unwrap().key,
        key(2)
    );
    assert_eq!(extras(&plan, "shared"), vec!["blue"]);
}

#[test]
fn a_feature_is_tied_to_the_same_concrete_file_even_when_versions_are_equal() {
    let mut d = Depot::default();
    add(
        &mut d,
        "demo",
        "1.0",
        1,
        vec![conditional("bad", &["fast"], &[1])],
    );
    add(
        &mut d,
        "demo",
        "1.0",
        2,
        vec![conditional("good", &["fast"], &[1])],
    );
    d.projects.insert("bad".into(), vec![]);
    add(&mut d, "good", "1", 3, vec![]);
    let plan = complete(&[request("demo", &["fast"])], &d);
    assert_eq!(names(&plan), vec!["demo", "good"]);
    assert_eq!(
        plan.selected.iter().find(|c| c.name == "demo").unwrap().key,
        key(2)
    );
    assert_eq!(extras(&plan, "demo"), vec!["fast"]);
}

#[test]
fn a_feature_requires_the_selected_files_actual_metadata_and_can_resume() {
    let mut d = Depot::default();
    d.candidate("demo", "1", 7, None);
    let roots = [request("demo", &["fast"])];
    assert_eq!(
        solve_step(&roots, &d, NativeSolveLimits::default()).unwrap(),
        SolveStep::NeedMetadata(key(7))
    );
    d.metadata.insert(key(7), CandidateMetadata::default());
    let plan = complete(&roots, &d);
    assert_eq!(names(&plan), vec!["demo"]);
    assert_eq!(extras(&plan, "demo"), vec!["fast"]);
}

#[test]
fn truth_state_expansion_extra_references_fields_and_work_are_prechecked() {
    let mut d = Depot::default();
    add(
        &mut d,
        "demo",
        "1",
        1,
        vec![conditional("child", &["blue", "red"], &[3])],
    );
    add(&mut d, "child", "1", 2, vec![]);
    let roots = [request("demo", &["blue", "red"])];
    assert_eq!(names(&complete(&roots, &d)), vec!["child", "demo"]);
    for limits in [
        NativeSolveLimits {
            max_extras_per_requirement: 1,
            ..Default::default()
        },
        NativeSolveLimits {
            max_truth_extras: 1,
            ..Default::default()
        },
        NativeSolveLimits {
            max_total_extra_references: 3,
            ..Default::default()
        },
        NativeSolveLimits {
            max_total_guard_states: 3,
            ..Default::default()
        },
        NativeSolveLimits {
            max_truth_work: 7,
            ..Default::default()
        },
        NativeSolveLimits {
            max_field_bytes: 3,
            ..Default::default()
        },
    ] {
        assert_eq!(
            solve_step(&roots, &d, limits).unwrap_err(),
            NativeSolveError::Limit
        );
    }
    let mut overflow = Depot::default();
    let bits = usize::BITS as usize;
    let names = (0..bits).map(|i| format!("e{i:03}")).collect();
    add(
        &mut overflow,
        "demo",
        "1",
        1,
        vec![Dependency {
            requirement: request("never-loaded", &[]),
            extra_truth: Some(ExtraTruth {
                extra_names: names,
                enabled_masks: vec![],
            }),
        }],
    );
    let limits = NativeSolveLimits {
        max_truth_extras: bits,
        max_total_guard_states: usize::MAX,
        max_truth_work: usize::MAX,
        ..Default::default()
    };
    assert_eq!(
        solve_step(&[request("demo", &[])], &overflow, limits).unwrap_err(),
        NativeSolveError::Limit
    );
}

#[test]
fn nonmonotone_truth_or_invalid_false_declarations_fail_before_any_child_request() {
    for truth in [
        ExtraTruth {
            extra_names: vec!["fast".into()],
            enabled_masks: vec![0],
        },
        ExtraTruth {
            extra_names: vec!["red".into(), "blue".into()],
            enabled_masks: vec![3],
        },
        ExtraTruth {
            extra_names: vec!["fast".into(), "fast".into()],
            enabled_masks: vec![],
        },
        ExtraTruth {
            extra_names: vec!["Fast_Name".into()],
            enabled_masks: vec![],
        },
        ExtraTruth {
            extra_names: vec!["fast".into()],
            enabled_masks: vec![1, 1],
        },
        ExtraTruth {
            extra_names: vec!["fast".into()],
            enabled_masks: vec![2],
        },
    ] {
        let mut d = Depot::default();
        add(
            &mut d,
            "demo",
            "1",
            1,
            vec![Dependency {
                requirement: request("never-loaded", &[]),
                extra_truth: Some(truth),
            }],
        );
        assert_eq!(
            solve_step(&[request("demo", &[])], &d, NativeSolveLimits::default()).unwrap_err(),
            NativeSolveError::InvalidInput
        );
    }
    let mut d = Depot::default();
    let mut invalid = conditional("private", &["fast"], &[]);
    invalid.requirement.constraint =
        RequirementConstraint::Version("@ https://private.invalid/?source-secret".into());
    add(
        &mut d,
        "demo",
        "1",
        1,
        vec![conditional("never-loaded", &["fast"], &[]), invalid],
    );
    let error = solve_step(&[request("demo", &[])], &d, NativeSolveLimits::default()).unwrap_err();
    assert_eq!(error, NativeSolveError::InvalidInput);
    assert!(!error.to_string().contains("source-secret"));
}

#[test]
fn an_always_true_monotone_guard_cannot_invent_an_extra_to_evade_no_solution() {
    let mut d = Depot::default();
    add(
        &mut d,
        "demo",
        "1",
        1,
        vec![conditional("bad", &["fast"], &[0, 1])],
    );
    d.projects.insert("bad".into(), vec![]);
    assert_eq!(
        solve_step(&[request("demo", &[])], &d, NativeSolveLimits::default()).unwrap_err(),
        NativeSolveError::NoSolution
    );
}
