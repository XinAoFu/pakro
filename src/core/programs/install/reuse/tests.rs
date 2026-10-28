use super::*;
use pakro_index::python_metadata::{
    PythonDependencyMetadataFacts, PythonMetadataExpectation, PythonMetadataFields,
    project_python_metadata,
};
use pakro_index::python_source::NormalizedPythonName;

fn environment(version: &str) -> PythonMarkerEnvironment<'_> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "darwin",
        platform_release: "25.0",
        platform_system: "Darwin",
        platform_version: "25.0",
        platform_machine: "arm64",
        platform_python_implementation: "CPython",
        python_version: version,
        python_full_version: if version == "3.13" {
            "3.13.16"
        } else {
            "3.14.9"
        },
        implementation_name: "cpython",
        implementation_version: if version == "3.13" {
            "3.13.16"
        } else {
            "3.14.9"
        },
    }
}

fn facts(name: &str, dependencies: &[&str]) -> PythonDependencyMetadataFacts {
    let dependencies = dependencies
        .iter()
        .map(|raw| raw.to_string())
        .collect::<Vec<_>>();
    project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 4),
            name,
            version: "1.0",
            requires_python: Some(">=3.13"),
            requires_dist: &dependencies,
            provides_extra: &["feature".into()],
            dynamic: &[],
        },
        PythonMetadataExpectation {
            name: &NormalizedPythonName::new(name).unwrap(),
            version: "1.0",
        },
        Default::default(),
    )
    .unwrap()
}

fn roots(raw: &[&str]) -> Vec<PythonRequirementSyntax> {
    raw.iter()
        .map(|raw| parse_python_requirement(raw, Default::default()).unwrap())
        .collect()
}

#[test]
fn p3_shared_reuse_rejects_extra_enabled_closure_for_a_plain_same_version_root() {
    let dependency = "feature==1.0; extra == 'feature'";
    let demo = facts("demo", &[dependency]);
    let feature = facts("feature", &[]);
    let nodes = [
        GraphCandidateObservation {
            key: CandidateKey([1; 32]),
            name: "demo",
            version: "1.0",
            facts: &demo,
        },
        GraphCandidateObservation {
            key: CandidateKey([2; 32]),
            name: "feature",
            version: "1.0",
            facts: &feature,
        },
    ];
    let edges = [(CandidateKey([1; 32]), CandidateKey([2; 32]), dependency)];
    assert!(
        matches_metadata(
            &roots(&["demo[feature]==1.0"]),
            &nodes,
            &[CandidateKey([1; 32])],
            &edges,
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    assert!(
        !matches_metadata(
            &roots(&["demo==1.0"]),
            &nodes,
            &[CandidateKey([1; 32])],
            &edges,
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    assert!(
        matches_metadata(
            &roots(&["demo==1.0"]),
            &nodes[..1],
            &[CandidateKey([1; 32])],
            &[],
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    assert!(
        !matches_metadata(
            &roots(&["demo[feature]==1.0"]),
            &nodes[..1],
            &[CandidateKey([1; 32])],
            &[],
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
}

#[test]
fn p3_shared_reuse_requires_the_metadata_derived_edges_not_just_the_node_set() {
    let dependency = "feature==1.0; extra == 'feature'";
    let demo = facts("demo", &[dependency]);
    let feature = facts("feature", &[]);
    let nodes = [
        GraphCandidateObservation {
            key: CandidateKey([1; 32]),
            name: "demo",
            version: "1.0",
            facts: &demo,
        },
        GraphCandidateObservation {
            key: CandidateKey([2; 32]),
            name: "feature",
            version: "1.0",
            facts: &feature,
        },
    ];
    let roots = roots(&["demo[feature]==1.0", "feature==1.0"]);
    let expected_roots = [CandidateKey([1; 32]), CandidateKey([2; 32])];
    assert!(
        !matches_metadata(
            &roots,
            &nodes,
            &expected_roots,
            &[],
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    let wrong = [(CandidateKey([1; 32]), CandidateKey([2; 32]), "feature>=1")];
    assert!(
        !matches_metadata(
            &roots,
            &nodes,
            &expected_roots,
            &wrong,
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    let correct = [(CandidateKey([1; 32]), CandidateKey([2; 32]), dependency)];
    assert!(
        matches_metadata(
            &roots,
            &nodes,
            &expected_roots,
            &correct,
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
}

#[test]
fn p3_shared_reuse_evaluates_complete_root_and_dependency_markers_on_the_actual_target() {
    let dependency = "feature==1.0; python_version >= '3.14'";
    let demo = facts("demo", &[dependency]);
    let feature = facts("feature", &[]);
    let nodes = [
        GraphCandidateObservation {
            key: CandidateKey([1; 32]),
            name: "demo",
            version: "1.0",
            facts: &demo,
        },
        GraphCandidateObservation {
            key: CandidateKey([2; 32]),
            name: "feature",
            version: "1.0",
            facts: &feature,
        },
    ];
    let roots = roots(&["demo==1.0", "absent==1.0; python_version < '3.0'"]);
    assert!(
        matches_metadata(
            &roots,
            &nodes[..1],
            &[CandidateKey([1; 32])],
            &[],
            environment("3.13"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    assert!(
        !matches_metadata(
            &roots,
            &nodes[..1],
            &[CandidateKey([1; 32])],
            &[],
            environment("3.14"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
    let edges = [(CandidateKey([1; 32]), CandidateKey([2; 32]), dependency)];
    assert!(
        matches_metadata(
            &roots,
            &nodes,
            &[CandidateKey([1; 32])],
            &edges,
            environment("3.14"),
            MarkerSemantics::Current2026
        )
        .unwrap()
    );
}

#[test]
fn p3_installed_metadata_parser_rejects_identity_and_duplicate_singleton_damage() {
    let bytes = b"Metadata-Version: 2.4\nName: demo\nVersion: 1.0\nRequires-Dist: feature==1.0; extra == 'feature'\n".to_vec();
    assert!(
        crate::packages::python_wheel::parse_installed_core_metadata(
            bytes.clone(),
            "another",
            "1.0",
            Default::default()
        )
        .is_err()
    );
    let mut duplicate = bytes;
    duplicate.extend_from_slice(b"Version: 1.0\n");
    assert!(
        crate::packages::python_wheel::parse_installed_core_metadata(
            duplicate,
            "demo",
            "1.0",
            Default::default()
        )
        .is_err()
    );
}
