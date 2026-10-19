use super::*;
use pakro_index::python_metadata::{
    PythonMetadataExpectation, PythonMetadataFields, project_python_metadata,
};
use pakro_index::python_requirements::parse_python_requirement;

fn environment() -> PythonMarkerEnvironment<'static> {
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
fn facts(name: &str, dependencies: &[&str]) -> PythonDependencyMetadataFacts {
    let dependencies = dependencies
        .iter()
        .map(|raw| raw.to_string())
        .collect::<Vec<_>>();
    project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 6),
            name,
            version: "1.0",
            requires_python: Some(">=3"),
            requires_dist: &dependencies,
            provides_extra: &[],
            dynamic: &[],
        },
        PythonMetadataExpectation {
            name: &NormalizedPythonName::new(name).unwrap(),
            version: "1.0",
        },
        Default::default(),
    )
    .expect("real M1 prelude")
}
fn roots(raw: &[&str]) -> Vec<PythonRequirementSyntax> {
    raw.iter()
        .map(|raw| parse_python_requirement(raw, Default::default()).unwrap())
        .collect()
}
fn candidate<'a>(
    key: u8,
    name: &'a str,
    facts: &'a PythonDependencyMetadataFacts,
) -> GraphCandidateObservation<'a> {
    GraphCandidateObservation {
        key: CandidateKey([key; 32]),
        name,
        version: "1.0",
        facts,
    }
}
fn derive(
    raw: &[&str],
    nodes: &[GraphCandidateObservation<'_>],
    scopes: &[PythonMarkerScope<'_>],
) -> Result<DerivedGraphData, GraphCheckError> {
    derive_graph_scoped(
        &roots(raw),
        nodes,
        environment(),
        RootMarkerScopes::PerDeclaration(scopes),
        MarkerSemantics::Current2026,
        Default::default(),
        Default::default(),
    )
}

#[test]
fn project_extras_and_group_environment_keep_distinct_scopes_and_original_positions() {
    let a = facts("a", &[]);
    let b = facts("b", &[]);
    let result = derive(
        &[
            "unused; python_version<'2'",
            "a; extra=='ui'",
            "b; python_version>='3'",
        ],
        &[candidate(1, "a", &a), candidate(2, "b", &b)],
        &[
            PythonMarkerScope::Environment,
            PythonMarkerScope::Dependency {
                requested_extras: &["ui"],
            },
            PythonMarkerScope::Environment,
        ],
    )
    .unwrap();
    assert_eq!(
        result
            .graph
            .roots
            .iter()
            .map(|root| root.declaration)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}
#[test]
fn a_group_cannot_read_an_extra_defined_only_for_the_project() {
    let a = facts("a", &[]);
    let b = facts("b", &[]);
    assert_eq!(
        derive(
            &["a; extra=='ui'", "b; extra=='ui'"],
            &[candidate(1, "a", &a), candidate(2, "b", &b)],
            &[
                PythonMarkerScope::Dependency {
                    requested_extras: &["ui"]
                },
                PythonMarkerScope::Environment,
            ]
        )
        .unwrap_err(),
        GraphCheckError::Marker
    );
}
#[test]
fn lock_only_sets_are_not_made_available_by_selecting_a_group() {
    let a = facts("a", &[]);
    assert_eq!(
        derive(
            &["a; 'dev' in dependency_groups"],
            &[candidate(1, "a", &a)],
            &[PythonMarkerScope::Environment]
        )
        .unwrap_err(),
        GraphCheckError::Marker
    );
    assert_eq!(
        derive(
            &["a; 'ui' in extras"],
            &[candidate(1, "a", &a)],
            &[PythonMarkerScope::Dependency {
                requested_extras: &["ui"]
            }]
        )
        .unwrap_err(),
        GraphCheckError::Marker
    );
}
#[test]
fn scoped_roots_preserve_active_direct_reference_locations() {
    let a = facts("a", &[]);
    assert_eq!(
        derive(
            &["a; extra=='other'", "a @ https://example.test/PRIVATE.whl"],
            &[candidate(1, "a", &a)],
            &[
                PythonMarkerScope::Dependency {
                    requested_extras: &["ui"]
                },
                PythonMarkerScope::Environment,
            ]
        )
        .unwrap_err(),
        GraphCheckError::SourceReference(GraphSourceLocation::Root { declaration: 1 })
    );
}
#[test]
fn project_selection_extras_never_seed_a_distributions_dependency_extras() {
    let a = facts("a", &["b; extra=='ui'"]);
    let result = derive(
        &["a; extra=='ui'"],
        &[candidate(1, "a", &a)],
        &[PythonMarkerScope::Dependency {
            requested_extras: &["ui"],
        }],
    )
    .unwrap();
    assert!(result.graph.edges.is_empty());
    assert!(result.extras[&CandidateKey([1; 32])].is_empty());
}
#[test]
fn scoped_claim_validation_uses_the_same_least_fixed_point() {
    let a = facts("a", &[]);
    let selected = SelectedCandidate {
        key: CandidateKey([1; 32]),
        name: "a".into(),
        version: "1.0".into(),
        requested_extras: vec!["ghost".into()],
    };
    let nodes = [GraphNodeObservation {
        selected: &selected,
        facts: &a,
    }];
    assert_eq!(
        check_graph_scoped(
            &roots(&["a; extra=='ui'"]),
            &nodes,
            environment(),
            RootMarkerScopes::PerDeclaration(&[PythonMarkerScope::Dependency {
                requested_extras: &["ui"]
            }]),
            MarkerSemantics::Current2026,
            Default::default(),
            Default::default()
        )
        .unwrap_err(),
        GraphCheckError::NonMinimal
    );
}
#[test]
fn scope_cardinality_is_checked_even_when_no_roots_are_active() {
    for scopes in [vec![], vec![PythonMarkerScope::Environment; 2]] {
        assert_eq!(
            derive(&["a; python_version<'2'"], &[], &scopes).unwrap_err(),
            GraphCheckError::InvalidInput
        );
    }
}
#[test]
fn unused_scope_input_is_charged_before_marker_evaluation() {
    let scope = [PythonMarkerScope::Dependency {
        requested_extras: &["ui", "ui"],
    }];
    let limits = GraphCheckLimits {
        max_extra_references: 1,
        ..Default::default()
    };
    assert_eq!(
        derive_graph_scoped(
            &roots(&["absent; python_version<'2'"]),
            &[],
            environment(),
            RootMarkerScopes::PerDeclaration(&scope),
            MarkerSemantics::Current2026,
            Default::default(),
            limits
        )
        .unwrap_err(),
        GraphCheckError::Limit
    );
}
