use super::*;
use std::cell::RefCell;
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
fn key(id: u8) -> CandidateKey {
    CandidateKey([id; 32])
}
fn req(name: &str, raw: &str) -> Requirement {
    Requirement {
        name: name.into(),
        constraint: RequirementConstraint::Version(raw.into()),
        requested_extras: vec![],
    }
}
fn complete(roots: &[Requirement], depot: &Depot) -> NativePlan {
    let SolveStep::Complete(plan) =
        solve_step(roots, depot, Default::default()).expect("existing actual pure core setup")
    else {
        panic!("existing core fixture input missing")
    };
    plan
}

struct Policy<F>(F);
impl<F> ActiveCandidatePolicy for Policy<F>
where
    F: for<'a> Fn(ActivePolicyQuery<'a>) -> Result<ActivePolicyDecision, ActivePolicyError>,
{
    fn evaluate(
        &self,
        query: ActivePolicyQuery<'_>,
    ) -> Result<ActivePolicyDecision, ActivePolicyError> {
        (self.0)(query)
    }
}
fn solve(
    roots: &[Requirement],
    depot: &Depot,
    policy: &dyn ActiveCandidatePolicy,
) -> Result<SolveStep, NativeSolveError> {
    solve_step_with_policy(roots, depot, policy, Default::default(), Default::default())
}
fn plan(roots: &[Requirement], depot: &Depot, policy: &dyn ActiveCandidatePolicy) -> NativePlan {
    let step = solve(roots, depot, policy).unwrap();
    let SolveStep::Complete(plan) = step else {
        panic!("unexpected input request: {step:?}")
    };
    plan
}
fn allow() -> impl ActiveCandidatePolicy {
    Policy(|_: ActivePolicyQuery<'_>| Ok(ActivePolicyDecision::Allow))
}
fn basic() -> Depot {
    let mut d = Depot::default();
    d.candidate("demo", "2.0rc1", 1, Some(vec![]));
    d.candidate("demo", "1.0", 2, Some(vec![]));
    d
}

#[test]
fn policy_rejection_chooses_a_final_without_changing_the_candidate_universe() {
    let d = basic();
    let roots = [req("demo", ">=1")];
    assert_eq!(complete(&roots, &d).selected[0].key, key(1));
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        Ok(if q.selected == key(1) {
            ActivePolicyDecision::Reject
        } else {
            ActivePolicyDecision::Allow
        })
    });
    assert_eq!(plan(&roots, &d, &p).selected[0].key, key(2));
    let roots = [req("demo", ">=2.0rc1")];
    assert_eq!(plan(&roots, &d, &allow()).selected[0].key, key(1));
}

#[test]
fn complete_original_root_constraints_and_duplicate_origins_reach_the_callback() {
    let d = basic();
    let roots = [
        req("demo", ">=1"),
        req("demo", "<3"),
        req("demo", ">=1"),
        req("demo", "===2.0rc1"),
    ];
    complete(&roots, &d);
    let seen = RefCell::new(false);
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        assert_eq!(
            q.constraints.iter().map(|r| r.raw).collect::<Vec<_>>(),
            [">=1", "<3", ">=1", "===2.0rc1"]
        );
        for (index, r) in q.constraints.iter().enumerate() {
            assert_eq!(
                r.origin,
                ActiveRequirementOrigin::Root { declaration: index }
            );
        }
        assert_eq!(q.candidates.len(), 2);
        *seen.borrow_mut() = true;
        Ok(ActivePolicyDecision::Allow)
    });
    assert_eq!(plan(&roots, &d, &p).selected[0].key, key(1));
    assert!(*seen.borrow());
}

#[test]
fn transitive_constraints_keep_the_actual_parent_and_original_declaration_index() {
    let mut d = basic();
    d.candidate(
        "app",
        "1",
        3,
        Some(vec![req("demo", ">=1"), req("demo", ">=2.0rc1")]),
    );
    let roots = [req("app", ">=1")];
    complete(&roots, &d);
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        if q.name == "demo" {
            assert_eq!(q.constraints.len(), 2);
            assert_eq!(q.constraints[1].raw, ">=2.0rc1");
            assert_eq!(
                q.constraints[1].origin,
                ActiveRequirementOrigin::Dependency {
                    parent: key(3),
                    declaration: 1
                }
            );
        }
        Ok(ActivePolicyDecision::Allow)
    });
    assert_eq!(plan(&roots, &d, &p).selected.len(), 2);
}

#[test]
fn rejected_wide_parent_branch_does_not_permanently_ban_a_legal_prerelease() {
    let mut d = basic();
    d.candidate("app", "2", 3, Some(vec![req("demo", ">=1")]));
    d.candidate("app", "1", 4, Some(vec![req("demo", ">=2.0rc1")]));
    d.metadata.get_mut(&key(2)).unwrap().dependencies =
        vec![Dependency::unconditional(req("impossible", ">=1"))];
    d.projects.insert("impossible".into(), vec![]);
    let roots = [req("app", ">=1")];
    assert_eq!(
        complete(&roots, &d)
            .selected
            .iter()
            .find(|n| n.name == "app")
            .unwrap()
            .key,
        key(3)
    );
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        Ok(
            if q.name == "demo"
                && q.selected == key(1)
                && q.constraints.iter().any(|r| r.raw == ">=1")
            {
                ActivePolicyDecision::Reject
            } else {
                ActivePolicyDecision::Allow
            },
        )
    });
    let selected = plan(&roots, &d, &p);
    assert_eq!(
        selected
            .selected
            .iter()
            .find(|n| n.name == "app")
            .unwrap()
            .key,
        key(4)
    );
    assert_eq!(
        selected
            .selected
            .iter()
            .find(|n| n.name == "demo")
            .unwrap()
            .key,
        key(1)
    );
}

#[test]
fn inactive_guard_constraints_are_absent_until_the_actual_root_requests_the_extra() {
    let mut d = basic();
    d.candidate("app", "1", 3, Some(vec![]));
    d.metadata.get_mut(&key(3)).unwrap().dependencies = vec![Dependency {
        requirement: req("demo", ">=2.0rc1"),
        extra_truth: Some(ExtraTruth {
            extra_names: vec!["fast".into()],
            enabled_masks: vec![1],
        }),
    }];
    for enabled in [false, true] {
        let roots = [Requirement {
            requested_extras: if enabled { vec!["fast".into()] } else { vec![] },
            ..req("app", ">=1")
        }];
        complete(&roots, &d);
        let seen = RefCell::new(false);
        let p = Policy(|q: ActivePolicyQuery<'_>| {
            if q.name == "demo" {
                *seen.borrow_mut() = true;
                assert_eq!(q.constraints[0].raw, ">=2.0rc1");
            }
            Ok(ActivePolicyDecision::Allow)
        });
        let result = plan(&roots, &d, &p);
        assert_eq!(*seen.borrow(), enabled);
        assert_eq!(result.selected.len(), if enabled { 2 } else { 1 });
    }
}

#[test]
fn multiple_joint_nogoods_keep_a_valid_changed_ancestor_witness_for_each() {
    let mut d = Depot::default();
    for (name, high, low) in [("a", 1, 2), ("b", 3, 4)] {
        d.candidate(name, "2", high, Some(vec![]));
        d.candidate(name, "1", low, Some(vec![]));
    }
    let roots = [req("a", ">=1"), req("b", ">=1")];
    complete(&roots, &d);
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        Ok(if [key(1), key(3)].contains(&q.selected) {
            ActivePolicyDecision::Reject
        } else {
            ActivePolicyDecision::Allow
        })
    });
    let result = plan(&roots, &d, &p);
    assert_eq!(
        result.selected.iter().map(|n| n.key).collect::<Vec<_>>(),
        [key(2), key(4)]
    );
}

#[test]
fn nogood_witness_ghosts_cannot_seed_sources_features_or_returned_distributions() {
    let mut d = Depot::default();
    d.candidate("parent", "2", 1, Some(vec![req("leaf", ">=1")]));
    d.candidate("parent", "1", 2, Some(vec![]));
    d.candidate("leaf", "2.0rc1", 3, Some(vec![]));
    d.candidate("leaf", "1", 4, Some(vec![]));
    let parent = Requirement {
        requested_extras: vec!["ghost".into()],
        ..req("parent", "==1")
    };
    d.metadata.get_mut(&key(4)).unwrap().dependencies = vec![
        Dependency::unconditional(parent),
        Dependency::unconditional(Requirement {
            name: "private-ghost-source".into(),
            constraint: RequirementConstraint::SourceReference,
            requested_extras: vec![],
        }),
    ];
    d.metadata.get_mut(&key(2)).unwrap().dependencies = vec![Dependency {
        requirement: req("phantom", ">=1"),
        extra_truth: Some(ExtraTruth {
            extra_names: vec!["ghost".into()],
            enabled_masks: vec![1],
        }),
    }];
    d.candidate("phantom", "1", 5, Some(vec![]));
    let roots = [req("parent", ">=1")];
    complete(&roots, &d);
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        Ok(if q.name == "leaf" {
            ActivePolicyDecision::Reject
        } else {
            ActivePolicyDecision::Allow
        })
    });
    let result = plan(&roots, &d, &p);
    assert_eq!(result.selected.len(), 1);
    assert_eq!(result.selected[0].key, key(2));
    assert!(result.selected[0].requested_extras.is_empty());
}

#[test]
fn unknown_final_metadata_is_requested_before_its_absence_can_enable_fallback() {
    let mut d = basic();
    d.metadata.remove(&key(2));
    let roots = [req("demo", ">=1")];
    complete(&roots, &d);
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        let alternative = q
            .candidates
            .iter()
            .find(|c| c.candidate.key == key(2))
            .unwrap();
        Ok(if alternative.metadata.is_none() {
            ActivePolicyDecision::NeedMetadata(key(2))
        } else {
            assert_eq!(
                alternative.metadata.unwrap().rejection,
                Some(CandidateRejection::RequiresPython)
            );
            ActivePolicyDecision::Allow
        })
    });
    assert_eq!(
        solve(&roots, &d, &p).unwrap(),
        SolveStep::NeedMetadata(key(2))
    );
    d.metadata.insert(
        key(2),
        CandidateMetadata {
            rejection: Some(CandidateRejection::RequiresPython),
            ..Default::default()
        },
    );
    assert_eq!(plan(&roots, &d, &p).selected[0].key, key(1));
}

#[test]
fn a_policy_cannot_request_unknown_project_keys_or_already_known_metadata() {
    let d = basic();
    let roots = [req("demo", ">=1")];
    assert_eq!(plan(&roots, &d, &allow()).selected.len(), 1);
    for requested in [key(99), key(1), key(2)] {
        let p = Policy(move |_: ActivePolicyQuery<'_>| {
            Ok(ActivePolicyDecision::NeedMetadata(requested))
        });
        assert_eq!(
            solve(&roots, &d, &p).unwrap_err(),
            NativeSolveError::InvalidInput
        );
    }
}

#[test]
fn exact_yanked_exception_uses_the_actual_constraint_and_observed_alternative() {
    let mut d = Depot::default();
    d.candidate("demo", "1.0", 1, Some(vec![]));
    d.candidate("demo", "1.0", 2, Some(vec![]));
    let p = Policy(|q: ActivePolicyQuery<'_>| {
        let exact = q.constraints.iter().any(|r| r.raw == "==1.0");
        let other_compatible = q.candidates.iter().any(|c| {
            c.candidate.key == key(2) && c.metadata.is_some_and(|m| m.rejection.is_none())
        });
        Ok(if q.selected == key(1) && (!exact || other_compatible) {
            ActivePolicyDecision::Reject
        } else {
            ActivePolicyDecision::Allow
        })
    });
    let roots = [req("demo", "==1.0")];
    complete(&roots, &d);
    assert_eq!(plan(&roots, &d, &p).selected[0].key, key(2));
    d.metadata.get_mut(&key(2)).unwrap().rejection = Some(CandidateRejection::RequiresPython);
    assert_eq!(plan(&roots, &d, &p).selected[0].key, key(1));
    assert_eq!(
        solve(&[req("demo", ">=1")], &d, &p).unwrap_err(),
        NativeSolveError::NoSolution
    );
}

#[test]
fn policy_limits_are_positive_and_charge_across_rounds_before_retention() {
    let d = basic();
    let roots = [req("demo", ">=1")];
    assert_eq!(plan(&roots, &d, &allow()).selected.len(), 1);
    let defaults = NativePolicyLimits::default();
    for limits in [
        NativePolicyLimits {
            max_rounds: 0,
            ..defaults
        },
        NativePolicyLimits {
            max_nogood_terms: 0,
            ..defaults
        },
        NativePolicyLimits {
            max_active_references: 0,
            ..defaults
        },
        NativePolicyLimits {
            max_propagation_steps: 0,
            ..defaults
        },
    ] {
        assert_eq!(
            solve_step_with_policy(&[], &d, &allow(), Default::default(), limits).unwrap_err(),
            NativeSolveError::InvalidLimits
        );
    }
    let reject = Policy(|q: ActivePolicyQuery<'_>| {
        Ok(if q.selected == key(1) {
            ActivePolicyDecision::Reject
        } else {
            ActivePolicyDecision::Allow
        })
    });
    let exact = NativePolicyLimits {
        max_rounds: 2,
        max_nogood_terms: 1,
        max_active_references: 2,
        max_propagation_steps: 64,
    };
    assert!(matches!(
        solve_step_with_policy(&roots, &d, &reject, Default::default(), exact),
        Ok(SolveStep::Complete(_))
    ));
    for limits in [
        NativePolicyLimits {
            max_rounds: 1,
            ..exact
        },
        NativePolicyLimits {
            max_active_references: 1,
            ..exact
        },
    ] {
        assert_eq!(
            solve_step_with_policy(&roots, &d, &reject, Default::default(), limits).unwrap_err(),
            NativeSolveError::Limit
        );
    }
    let doubled = [req("demo", ">=1"), req("demo", "<3")];
    assert_eq!(
        solve_step_with_policy(
            &doubled,
            &d,
            &allow(),
            Default::default(),
            NativePolicyLimits {
                max_propagation_steps: 1,
                ..defaults
            }
        )
        .unwrap_err(),
        NativeSolveError::Limit
    );
    let mut two_projects = basic();
    two_projects.candidate("other", "1", 3, Some(vec![]));
    let roots = [req("demo", ">=1"), req("other", ">=1")];
    let exact_terms = NativePolicyLimits {
        max_nogood_terms: 2,
        ..defaults
    };
    assert!(matches!(
        solve_step_with_policy(
            &roots,
            &two_projects,
            &reject,
            Default::default(),
            exact_terms
        ),
        Ok(SolveStep::Complete(_))
    ));
    assert_eq!(
        solve_step_with_policy(
            &roots,
            &two_projects,
            &reject,
            Default::default(),
            NativePolicyLimits {
                max_nogood_terms: 1,
                ..defaults
            }
        )
        .unwrap_err(),
        NativeSolveError::Limit
    );
}

#[test]
fn callback_errors_remain_typed_and_do_not_grant_a_complete_plan() {
    let d = basic();
    let roots = [req("demo", ">=1")];
    assert_eq!(plan(&roots, &d, &allow()).selected.len(), 1);
    for (error, expected) in [
        (
            ActivePolicyError::InvalidInput,
            NativeSolveError::InvalidInput,
        ),
        (ActivePolicyError::Limit, NativeSolveError::Limit),
    ] {
        assert_eq!(
            solve(
                &roots,
                &d,
                &Policy(move |_: ActivePolicyQuery<'_>| Err(error))
            )
            .unwrap_err(),
            expected
        );
    }
}

#[test]
fn an_actual_reachable_source_still_pauses_before_any_policy_success() {
    let mut d = Depot::default();
    d.candidate("app", "1", 1, Some(vec![]));
    d.metadata.get_mut(&key(1)).unwrap().dependencies = vec![
        Dependency::unconditional(req("app", "==1")),
        Dependency::unconditional(Requirement {
            name: "source-child".into(),
            constraint: RequirementConstraint::SourceReference,
            requested_extras: vec![],
        }),
    ];
    let calls = RefCell::new(0);
    let p = Policy(|_: ActivePolicyQuery<'_>| {
        *calls.borrow_mut() += 1;
        Ok(ActivePolicyDecision::Allow)
    });
    assert_eq!(
        solve(&[req("app", "==1")], &d, &p).unwrap(),
        SolveStep::NeedSource {
            parent: key(1),
            declaration: 1
        }
    );
    assert_eq!(*calls.borrow(), 0);
}
