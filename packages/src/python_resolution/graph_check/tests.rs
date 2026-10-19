use super::*;
use pakro_index::python_metadata::{
    PythonMetadataExpectation, PythonMetadataFields, project_python_metadata,
};
use pakro_index::python_requirements::parse_python_requirement;
use pakro_index::python_source::NormalizedPythonName;

fn env() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "linux",
        platform_release: "6.8",
        platform_system: "Linux",
        platform_version: "2026",
        platform_machine: "aarch64",
        platform_python_implementation: "CPython",
        python_version: "3.12",
        python_full_version: "3.12.5",
        implementation_name: "cpython",
        implementation_version: "3.12.5",
    }
}
struct Node {
    selected: SelectedCandidate,
    facts: PythonDependencyMetadataFacts,
}
fn node(id: u8, name: &str, version: &str, extras: &[&str], deps: &[&str], python: &str) -> Node {
    let requirements = deps.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let facts = project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 6),
            name,
            version,
            requires_python: Some(python),
            requires_dist: &requirements,
            provides_extra: &[],
            dynamic: &[],
        },
        PythonMetadataExpectation {
            name: &NormalizedPythonName::new(name).unwrap(),
            version,
        },
        Default::default(),
    )
    .expect("actual M1 declaration fixture must succeed before graph checking");
    Node {
        selected: SelectedCandidate {
            name: name.to_owned(),
            key: CandidateKey([id; 32]),
            version: version.to_owned(),
            requested_extras: extras.iter().map(|s| s.to_string()).collect(),
        },
        facts,
    }
}
fn roots(raw: &[&str]) -> Vec<PythonRequirementSyntax> {
    raw.iter()
        .map(|raw| parse_python_requirement(raw, Default::default()).unwrap())
        .collect()
}
fn check(raw: &[&str], nodes: &[Node]) -> Result<CheckedGraphData, GraphCheckError> {
    run(&roots(raw), nodes, env(), Default::default())
}
fn run(
    roots: &[PythonRequirementSyntax],
    nodes: &[Node],
    environment: PythonMarkerEnvironment<'_>,
    limits: GraphCheckLimits,
) -> Result<CheckedGraphData, GraphCheckError> {
    let observations = nodes
        .iter()
        .map(|n| GraphNodeObservation {
            selected: &n.selected,
            facts: &n.facts,
        })
        .collect::<Vec<_>>();
    check_graph(
        roots,
        &observations,
        environment,
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
        limits,
    )
}

#[test]
fn raw_arbitrary_rhs_remains_literal_in_roots_and_actual_metadata_dependencies() {
    let leaf = [node(1, "demo", "1.0", &[], &[], ">=3")];
    assert!(check(&["demo===1.0"], &leaf).is_ok());
    assert!(check(&["demo==01.0"], &leaf).is_ok());
    assert_eq!(
        check(&["demo===01.0"], &leaf).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
    let nodes = [
        node(1, "parent", "1.0", &[], &["demo===01.0"], ">=3"),
        node(2, "demo", "1.0", &[], &[], ">=3"),
    ];
    assert_eq!(
        nodes[0].facts.requirements()[0].raw_version_specifiers(),
        Some("===01.0")
    );
    assert_eq!(
        check(&["parent==1"], &nodes).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
}
#[test]
fn original_roots_constraints_and_legal_runtime_cycles_close_to_real_rows() {
    let nodes = [
        node(1, "a", "1.0", &[], &["b>=1", "b<2", "a==1"], ">=3"),
        node(2, "b", "1.0.0", &[], &["a>=1"], ">=3"),
    ];
    let checked = check(&["a==1.0.0", "b<2; sys_platform == 'darwin'"], &nodes).unwrap();
    assert_eq!(
        checked.roots,
        [CheckedRoot {
            declaration: 0,
            to: CandidateKey([1; 32])
        }]
    );
    assert_eq!(checked.edges.len(), 4);
    assert!(checked.edges.contains(&CheckedEdge {
        from: CandidateKey([1; 32]),
        declaration: 1,
        to: CandidateKey([2; 32])
    }));
}
#[test]
fn missing_root_is_not_a_valid_empty_graph() {
    assert_eq!(
        check(&["absent>=1"], &[]).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
}
#[test]
fn active_original_dependency_cannot_be_omitted_by_a_solver_projection() {
    let nodes = [node(1, "a", "1", &["fast"], &["b; extra == 'fast'"], ">=3")];
    assert_eq!(
        check(&["a[fast]"], &nodes).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
}
#[test]
fn every_repeated_active_constraint_is_rechecked_against_actual_selected_version() {
    let nodes = [
        node(1, "a", "1", &[], &["b>=1", "b<2"], ">=3"),
        node(2, "b", "2", &[], &[], ">=3"),
    ];
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
}
#[test]
fn extras_propagate_to_a_least_fixed_point_across_cycles_and_conjunctions() {
    let nodes = [
        node(
            1,
            "a",
            "1",
            &["x", "y"],
            &["b[y]; extra == 'x'", "c; extra == 'x' and extra == 'y'"],
            ">=3",
        ),
        node(2, "b", "1", &["y"], &["a[y]; extra == 'y'"], ">=3"),
        node(3, "c", "1", &[], &[], ">=3"),
    ];
    let checked = check(&["a[x]"], &nodes).unwrap();
    assert_eq!(checked.edges.len(), 3);
}
#[test]
fn a_self_supporting_extra_has_no_authority_without_an_incoming_request() {
    let nodes = [node(
        1,
        "a",
        "1",
        &["ghost"],
        &["a[ghost]; extra == 'ghost'"],
        ">=3",
    )];
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::NonMinimal
    );
}
#[test]
fn unreachable_nodes_are_rejected_and_inactive_requirements_do_not_create_edges() {
    let nodes = [
        node(
            1,
            "a",
            "1",
            &[],
            &["unused; sys_platform == 'darwin'"],
            ">=3",
        ),
        node(2, "unused", "1", &[], &[], ">=3"),
    ];
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::NonMinimal
    );
    let checked = check(&["a"], &nodes[..1]).unwrap();
    assert!(checked.edges.is_empty());
}
#[test]
fn actual_metadata_identity_and_unique_candidate_keys_are_required() {
    let mut nodes = [node(1, "a", "1", &[], &[], ">=3")];
    nodes[0].selected.version = "2".into();
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::InvalidInput
    );
    let nodes = [
        node(1, "a", "1", &[], &[], ">=3"),
        node(1, "b", "1", &[], &[], ">=3"),
    ];
    assert_eq!(
        check(&["a", "b"], &nodes).unwrap_err(),
        GraphCheckError::InvalidInput
    );
}
#[test]
fn requires_python_uses_full_language_version_and_context_is_checked_without_markers() {
    let nodes = [node(1, "a", "1", &[], &[], "<3.12.5")];
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::RequiresPython
    );
    let nodes = [node(1, "a", "1", &[], &[], "<4")];
    let mut pypy = env();
    pypy.platform_python_implementation = "PyPy";
    pypy.implementation_name = "pypy";
    pypy.implementation_version = "8.0.1";
    run(&roots(&["a"]), &nodes, pypy, Default::default()).unwrap();
    pypy.python_full_version = "3.12";
    assert_eq!(
        run(&roots(&["a"]), &nodes, pypy, Default::default()).unwrap_err(),
        GraphCheckError::InvalidInput
    );
}
#[test]
fn active_direct_reference_requires_an_adapter_and_private_tokens_are_not_diagnostics() {
    let nodes = [node(
        1,
        "a",
        "1",
        &[],
        &["b @ https://example.test/${PRIVATE_TOKEN}/b.whl;part ; sys_platform == 'darwin'"],
        ">=3",
    )];
    check(&["a"], &nodes).unwrap();
    let nodes = [node(
        1,
        "a",
        "1",
        &[],
        &["b @ https://example.test/${PRIVATE_TOKEN}/b.whl;part"],
        ">=3",
    )];
    let error = check(&["a"], &nodes).unwrap_err();
    assert_eq!(
        error,
        GraphCheckError::SourceReference(GraphSourceLocation::Dependency {
            owner: CandidateKey([1; 32]),
            declaration: 0,
        })
    );
    assert!(!format!("{error:?} {error}").contains("PRIVATE_TOKEN"));
}
#[test]
fn exact_final_marker_warnings_and_unavailable_context_cannot_be_lost() {
    let nodes = [node(1, "a", "1", &[], &["unused; extra != 'speed'"], ">=3")];
    let checked = check(&["a"], &nodes).unwrap();
    assert!(checked.edges.is_empty());
    assert_eq!(checked.warnings.len(), 1);
    assert_eq!(checked.warnings[0].owner, Some(CandidateKey([1; 32])));
    assert_eq!(checked.warnings[0].declaration, 0);
    assert_eq!(
        checked.warnings[0].warnings[0].kind,
        pakro_index::python_markers::PythonMarkerWarningKind::UnsupportedExtraComparison
    );
    let nodes = [node(
        1,
        "a",
        "1",
        &[],
        &["unused; python_version >= '1' or 'test' in dependency_groups"],
        ">=3",
    )];
    assert_eq!(check(&["a"], &nodes).unwrap_err(), GraphCheckError::Marker);
}
#[test]
fn independent_counts_and_cumulative_marker_work_are_bounded() {
    let nodes = [
        node(
            1,
            "a",
            "1",
            &[],
            &["b; sys_platform == 'linux'", "b; os_name == 'posix'"],
            ">=3",
        ),
        node(2, "b", "1", &[], &[], ">=3"),
    ];
    let roots = roots(&["a"]);
    for limits in [
        GraphCheckLimits {
            max_roots: 0,
            ..Default::default()
        },
        GraphCheckLimits {
            max_nodes: 0,
            ..Default::default()
        },
        GraphCheckLimits {
            max_edges: 0,
            ..Default::default()
        },
        GraphCheckLimits {
            max_extra_references: 0,
            ..Default::default()
        },
        GraphCheckLimits {
            max_total_utf8_bytes: 0,
            ..Default::default()
        },
        GraphCheckLimits {
            max_evaluation_nodes: 0,
            ..Default::default()
        },
    ] {
        assert_eq!(
            run(&roots, &nodes, env(), limits).unwrap_err(),
            GraphCheckError::InvalidLimits
        );
    }
    for limits in [
        GraphCheckLimits {
            max_nodes: 1,
            ..Default::default()
        },
        GraphCheckLimits {
            max_edges: 1,
            ..Default::default()
        },
        GraphCheckLimits {
            max_total_utf8_bytes: 3,
            ..Default::default()
        },
        GraphCheckLimits {
            max_evaluation_nodes: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            run(&roots, &nodes, env(), limits).unwrap_err(),
            GraphCheckError::Limit
        );
    }
    assert_eq!(
        run(
            &roots,
            &nodes,
            env(),
            GraphCheckLimits {
                max_edges: 2,
                ..Default::default()
            }
        )
        .unwrap()
        .edges
        .len(),
        2
    );
}

#[test]
fn unconditional_declarations_also_consume_the_shared_evaluation_budget() {
    let nodes = [
        node(1, "a", "1", &[], &["b"], ">=3"),
        node(2, "b", "1", &[], &[], ">=3"),
    ];
    assert_eq!(
        run(
            &roots(&["a"]),
            &nodes,
            env(),
            GraphCheckLimits {
                max_evaluation_nodes: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        GraphCheckError::Limit,
    );
}

#[test]
fn root_extra_without_a_distribution_context_is_not_silently_false() {
    assert_eq!(
        check(&["a; extra == 'speed'"], &[]).unwrap_err(),
        GraphCheckError::Marker,
    );
    let nodes = [node(1, "a", "1", &[], &[], ">=3")];
    let observed = [GraphNodeObservation {
        selected: &nodes[0].selected,
        facts: &nodes[0].facts,
    }];
    let checked = check_graph(
        &roots(&["a; extra == 'speed'"]),
        &observed,
        env(),
        PythonMarkerScope::Dependency {
            requested_extras: &["speed"],
        },
        MarkerSemantics::Current2026,
        Default::default(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(checked.roots.len(), 1);
}

#[test]
fn prerelease_upper_bounds_use_the_same_specifier_semantics_as_the_solver() {
    let nodes = [
        node(1, "a", "2.0rc1", &[], &["b<2.0rc2"], ">=3"),
        node(2, "b", "2.0rc1", &[], &[], ">=3"),
    ];
    let checked = check(&["a<2.0rc2"], &nodes).unwrap();
    assert_eq!(checked.edges.len(), 1);
}

fn observations(nodes: &[Node]) -> Vec<GraphCandidateObservation<'_>> {
    nodes
        .iter()
        .map(|node| GraphCandidateObservation {
            key: node.selected.key,
            name: &node.selected.name,
            version: &node.selected.version,
            facts: &node.facts,
        })
        .collect()
}
fn derive(raw: &[&str], nodes: &[Node]) -> Result<DerivedGraphData, GraphCheckError> {
    derive_graph(
        &roots(raw),
        &observations(nodes),
        env(),
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
        Default::default(),
    )
}

#[test]
fn derive_seeds_extras_only_from_roots_and_propagates_them_across_a_real_scc() {
    let nodes = [
        node(
            1,
            "a",
            "1",
            &[],
            &["b[y]; extra == 'x'", "c; extra == 'x' and extra == 'y'"],
            ">=3",
        ),
        node(2, "b", "1", &[], &["a[y]; extra == 'y'"], ">=3"),
        node(3, "c", "1", &["untrusted-claim"], &[], ">=3"),
    ];
    let result = derive(&["a[x]"], &nodes).unwrap();
    assert_eq!(result.extras[&CandidateKey([1; 32])], ["x", "y"]);
    assert_eq!(result.extras[&CandidateKey([2; 32])], ["y"]);
    assert!(result.extras[&CandidateKey([3; 32])].is_empty());
    assert_eq!(result.graph.edges.len(), 3);
    assert_eq!(result.graph.roots[0].declaration, 0);
}

#[test]
fn derive_never_invents_a_self_supporting_extra_but_accepts_an_actual_seed() {
    let nodes = [node(
        1,
        "a",
        "1",
        &["ghost"],
        &["a[ghost]; extra == 'ghost'"],
        ">=3",
    )];
    let result = derive(&["a"], &nodes).unwrap();
    assert!(result.extras[&CandidateKey([1; 32])].is_empty());
    assert!(result.graph.edges.is_empty());
    let result = derive(&["a[ghost]"], &nodes).unwrap();
    assert_eq!(result.extras[&CandidateKey([1; 32])], ["ghost"]);
    assert_eq!(result.graph.edges.len(), 1);
    assert_eq!(
        check(&["a"], &nodes).unwrap_err(),
        GraphCheckError::NonMinimal
    );
}

#[test]
fn derive_rechecks_duplicate_specs_requires_python_and_the_explicit_root_scope() {
    let nodes = [
        node(1, "a", "1", &[], &["b>=1", "b<2"], ">=3"),
        node(2, "b", "1", &[], &[], ">=3"),
    ];
    assert_eq!(derive(&["a", "a==1"], &nodes).unwrap().graph.edges.len(), 2);
    let bad = [
        node(1, "a", "1", &[], &["b>=1", "b<2"], ">=3"),
        node(2, "b", "2", &[], &[], ">=3"),
    ];
    assert_eq!(
        derive(&["a"], &bad).unwrap_err(),
        GraphCheckError::Unsatisfied
    );
    let incompatible = [node(1, "a", "1", &[], &[], ">=99")];
    assert_eq!(
        derive(&["a"], &incompatible).unwrap_err(),
        GraphCheckError::RequiresPython
    );
    assert_eq!(
        derive(&["a; extra == 'speed'"], &nodes[..1]).unwrap_err(),
        GraphCheckError::Marker
    );
    let result = derive_graph(
        &roots(&["a[x]; extra == 'speed'"]),
        &observations(&[node(1, "a", "1", &[], &[], ">=3")]),
        env(),
        PythonMarkerScope::Dependency {
            requested_extras: &["speed"],
        },
        MarkerSemantics::Current2026,
        Default::default(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.extras[&CandidateKey([1; 32])], ["x"]);
}

#[test]
fn derive_source_locations_keep_inactive_original_indices_and_never_echo_the_token() {
    let nodes = [node(
        1,
        "a",
        "1",
        &[],
        &[
            "b @ https://example.test/INACTIVE/b.whl ; sys_platform == 'darwin'",
            "unused; extra == 'unused'",
            "b @ https://example.test/PRIVATE_TOKEN/b.whl",
        ],
        ">=3",
    )];
    let error = derive(&["a"], &nodes).unwrap_err();
    assert_eq!(
        error,
        GraphCheckError::SourceReference(GraphSourceLocation::Dependency {
            owner: CandidateKey([1; 32]),
            declaration: 2
        })
    );
    assert!(!format!("{error:?} {error}").contains("PRIVATE_TOKEN"));
    assert_eq!(
        derive(
            &[
                "a; sys_platform == 'darwin'",
                "a @ https://example.test/root.whl"
            ],
            &nodes
        )
        .unwrap_err(),
        GraphCheckError::SourceReference(GraphSourceLocation::Root { declaration: 1 })
    );
    let inactive = [node(
        1,
        "a",
        "1",
        &[],
        &["b @ https://example.test/b.whl ; sys_platform == 'darwin'"],
        ">=3",
    )];
    assert!(derive(&["a"], &inactive).unwrap().graph.edges.is_empty());
}

#[test]
fn derive_rejects_unreachable_rows_and_keeps_final_context_warnings() {
    let unreachable = [
        node(1, "a", "1", &[], &[], ">=3"),
        node(2, "b", "1", &[], &[], ">=3"),
    ];
    assert_eq!(
        derive(&["a"], &unreachable).unwrap_err(),
        GraphCheckError::NonMinimal
    );
    let warnings = [node(
        1,
        "a",
        "1",
        &["ignored"],
        &["b; extra != 'speed'"],
        ">=3",
    )];
    let result = derive(&["a"], &warnings).unwrap();
    assert!(result.extras[&CandidateKey([1; 32])].is_empty());
    assert!(result.graph.edges.is_empty());
    assert_eq!(result.graph.warnings.len(), 1);
    assert_eq!(result.graph.warnings[0].owner, Some(CandidateKey([1; 32])));
    assert_eq!(result.graph.warnings[0].declaration, 0);
}

#[test]
fn derive_and_check_share_cumulative_budgets_including_owned_output_extra_copies() {
    let nodes = [node(1, "a", "1", &["x"], &[], ">=3")];
    let seed_roots = roots(&["a[x]"]);
    let exact = GraphCheckLimits {
        max_roots: 1,
        max_nodes: 1,
        max_edges: 1,
        max_extra_references: 2,
        max_total_utf8_bytes: 10,
        max_evaluation_nodes: 1,
    };
    let result = derive_graph(
        &seed_roots,
        &observations(&nodes),
        env(),
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
        exact,
    )
    .unwrap();
    assert_eq!(result.extras[&CandidateKey([1; 32])], ["x"]);
    for limits in [
        GraphCheckLimits {
            max_extra_references: 1,
            ..exact
        },
        GraphCheckLimits {
            max_total_utf8_bytes: 9,
            ..exact
        },
    ] {
        assert_eq!(
            derive_graph(
                &seed_roots,
                &observations(&nodes),
                env(),
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                Default::default(),
                limits
            )
            .unwrap_err(),
            GraphCheckError::Limit
        );
    }
    assert_eq!(
        run(
            &seed_roots,
            &nodes,
            env(),
            GraphCheckLimits {
                max_total_utf8_bytes: 11,
                ..exact
            }
        )
        .unwrap_err(),
        GraphCheckError::Limit
    );
    run(
        &seed_roots,
        &nodes,
        env(),
        GraphCheckLimits {
            max_extra_references: 3,
            max_total_utf8_bytes: 11,
            ..exact
        },
    )
    .unwrap();
    let general = [
        node(1, "a", "1", &[], &["b; sys_platform == 'linux'"], ">=3"),
        node(2, "b", "1", &[], &[], ">=3"),
    ];
    assert_eq!(
        derive_graph(
            &self::roots(&["a"]),
            &observations(&general),
            env(),
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            Default::default(),
            GraphCheckLimits {
                max_evaluation_nodes: 2,
                ..Default::default()
            }
        )
        .unwrap_err(),
        GraphCheckError::Limit
    );
}
