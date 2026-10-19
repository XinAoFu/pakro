//! Registered only with the metadata exact-marker getter's wrong baseline.
//! Existing metadata behavior stays unchanged until both real targets are RED.

use super::*;

fn facts(raw: &[&str]) -> PythonDependencyMetadataFacts {
    let requirements: Vec<String> = raw.iter().map(|value| (*value).to_owned()).collect();
    let name = NormalizedPythonName::new("demo-pkg").unwrap();
    project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 5),
            name: "demo_pkg",
            version: "1.0",
            requires_python: None,
            requires_dist: &requirements,
            provides_extra: &[],
            dynamic: &[],
        },
        PythonMetadataExpectation {
            name: &name,
            version: "1.0",
        },
        Default::default(),
    )
    .unwrap()
}

#[test]
fn projected_declarations_carry_exact_ast_without_losing_any_comparison() {
    let input = [
        "literal; 'synthetic-marker-secret' == 'different'",
        "fields; python_full_version > implementation_version",
        "membership; python_version in '3.13'",
    ];
    let facts = facts(&input);
    for (raw, declaration) in input.iter().zip(facts.requirements()) {
        assert_eq!(declaration.raw_for_semantic_resolution(), *raw);
        assert_eq!(
            declaration
                .exact_marker()
                .expect("projection has no exact marker syntax")
                .comparisons()
                .count(),
            1
        );
    }
    assert!(!format!("{facts:?}").contains("synthetic-marker-secret"));
}

#[test]
fn metadata_projection_no_longer_constructs_or_warns_about_library_marker_approximations() {
    let facts = facts(&["literal; 'synthetic-marker-secret' == 'different'"]);
    // Check this first: the regression must witness the actual old library
    // warning rather than failing only at the new getter's None stub.
    assert!(
        !facts
            .warnings()
            .iter()
            .any(|warning| matches!(warning, PythonMetadataWarning::MarkerSyntax { .. })),
        "metadata still sent marker text into the algebraic library parser"
    );
    assert_eq!(
        facts.requirements()[0]
            .exact_marker()
            .unwrap()
            .comparisons()
            .count(),
        1
    );
}
