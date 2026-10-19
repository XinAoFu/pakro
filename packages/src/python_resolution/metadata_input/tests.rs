use super::*;
use pakro_index::python_metadata::{
    PythonMetadataExpectation, PythonMetadataFields, PythonMetadataLimits, project_python_metadata,
};
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
fn facts(requires: &[&str]) -> PythonDependencyMetadataFacts {
    facts_with_python(requires, ">=3.9")
}
fn facts_with_python(requires: &[&str], python: &str) -> PythonDependencyMetadataFacts {
    let requirements = requires.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 6),
            name: "demo",
            version: "1.0",
            requires_python: Some(python),
            requires_dist: &requirements,
            provides_extra: &["speed".into()],
            dynamic: &["Requires-Dist".into()],
        },
        PythonMetadataExpectation {
            name: &NormalizedPythonName::new("demo").unwrap(),
            version: "1.0",
        },
        PythonMetadataLimits::default(),
    )
    .unwrap()
}
fn compile(facts: &PythonDependencyMetadataFacts) -> MetadataSolveInput {
    compile_metadata_input(
        facts,
        env(),
        MarkerSemantics::Current2026,
        PythonMarkerLimits::default(),
        ExtraTruthLimits::default(),
        MetadataInputLimits::default(),
    )
    .unwrap()
}

#[test]
fn raw_arbitrary_literals_are_not_replaced_by_normalized_ast_display() {
    let declarations = [
        "dep===01.0",
        "dep (===v1.0, >=1,); python_version >= '3.11'",
        "dep===01.0,===1.0",
    ];
    let output = compile(&facts(&declarations));
    for ((dependency, original), expected) in output.dependencies.iter().zip(declarations).zip([
        "===01.0",
        "(===v1.0, >=1,)",
        "===01.0,===1.0",
    ]) {
        assert_eq!(dependency.original_declaration, original);
        assert!(
            matches!(&dependency.constraint, DependencyConstraint::Version(raw) if raw == expected)
        );
    }
}

#[test]
fn marker_limits_are_validated_even_when_actual_metadata_has_no_markers() {
    let metadata = facts(&[]);
    let limits = PythonMarkerLimits::default();
    for marker in [
        PythonMarkerLimits {
            max_input_bytes: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_literal_bytes: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_tokens: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_nodes: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_group_depth: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_environment_bytes: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_environment_field_bytes: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_set_entries: 0,
            ..limits
        },
        PythonMarkerLimits {
            max_evaluation_steps: 0,
            ..limits
        },
    ] {
        assert!(
            matches!(
                compile_metadata_input(
                    &metadata,
                    env(),
                    MarkerSemantics::Current2026,
                    marker,
                    ExtraTruthLimits::default(),
                    MetadataInputLimits::default(),
                ),
                Err(MetadataInputError::InvalidLimits)
            ),
            "invalid limits bypassed a marker-free actual metadata projection"
        );
    }
}
#[test]
fn complete_version_extras_raw_text_and_exact_and_condition_are_preserved() {
    let raw = "dep[Speed_Up]>=1, <3; extra == 'a' and extra == 'b'";
    let output = compile(&facts(&[raw]));
    let dependency = &output.dependencies[0];
    assert_eq!(dependency.name, "dep");
    assert_eq!(dependency.requested_extras, ["speed-up"]);
    assert_eq!(dependency.original_declaration, raw);
    assert!(
        matches!(&dependency.constraint,DependencyConstraint::Version(value) if value.contains(">=1") && value.contains("<3"))
    );
    assert_eq!(dependency.condition.as_ref().unwrap().enabled_masks(), [3]);
    assert_eq!(output.requires_python.as_deref(), Some(">=3.9"));
    assert_eq!(output.dynamic_fields, ["requires-dist"]);
    assert_eq!(
        output.dynamic_policy,
        SdistDynamicPolicy::AppendMarkedMultipleUseFields
    );
}
#[test]
fn source_references_keep_the_exact_private_token_without_cwd_or_environment_expansion() {
    let raw = "dep @ ../${PRIVATE_TOKEN}/source;extra";
    let output = compile(&facts(&[raw]));
    let DependencyConstraint::SourceReference(value) = &output.dependencies[0].constraint else {
        panic!("reference became a version range")
    };
    assert_eq!(value, "../${PRIVATE_TOKEN}/source;extra");
    assert!(!format!("{output:?}").contains("PRIVATE_TOKEN"));
}
#[test]
fn repeated_constraints_and_an_inactive_dependency_are_not_deduplicated_or_dropped() {
    let output = compile(&facts(&[
        "dep>=1",
        "dep<2",
        "unused; sys_platform == 'darwin'",
    ]));
    assert_eq!(output.dependencies.len(), 3);
    assert_eq!(output.dependencies[0].name, output.dependencies[1].name);
    assert!(
        output.dependencies[2]
            .condition
            .as_ref()
            .unwrap()
            .enabled_masks()
            .is_empty()
    );
}
#[test]
fn unavailable_marker_context_does_not_become_an_unconditional_dependency() {
    let data = facts(&["dep; 'dev' in dependency_groups"]);
    assert!(matches!(
        compile_metadata_input(
            &data,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default(),
            MetadataInputLimits::default()
        ),
        Err(MetadataInputError::Truth(_))
    ));
}
#[test]
fn truth_budgets_are_shared_by_all_declarations_in_the_actual_metadata() {
    let data = facts(&[
        "a; extra == 'x' or extra == 'y'",
        "b; extra == 'x' or extra == 'y'",
    ]);
    let exact = MetadataInputLimits {
        max_total_truth_states: 8,
        ..Default::default()
    };
    assert_eq!(
        compile_metadata_input(
            &data,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default(),
            exact
        )
        .unwrap()
        .dependencies
        .len(),
        2
    );
    let tight = MetadataInputLimits {
        max_total_truth_states: 7,
        ..Default::default()
    };
    assert!(
        compile_metadata_input(
            &data,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default(),
            tight
        )
        .is_err()
    );
}
#[test]
fn dependency_and_retained_output_limits_reject_instead_of_returning_partial_inputs() {
    let data = facts(&["a>=1", "b>=2"]);
    for limits in [
        MetadataInputLimits {
            max_dependencies: 1,
            ..Default::default()
        },
        MetadataInputLimits {
            max_retained_utf8_bytes: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            compile_metadata_input(
                &data,
                env(),
                MarkerSemantics::Current2026,
                PythonMarkerLimits::default(),
                ExtraTruthLimits::default(),
                limits
            )
            .unwrap_err(),
            MetadataInputError::Limit
        );
    }
    let zero = MetadataInputLimits {
        max_total_truth_states: 0,
        ..Default::default()
    };
    assert_eq!(
        compile_metadata_input(
            &data,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default(),
            zero
        )
        .unwrap_err(),
        MetadataInputError::InvalidLimits
    );
}

#[test]
fn actual_requires_python_rejects_a_candidate_without_dropping_its_dependency_evidence() {
    let output = compile(&facts_with_python(&["child>=1"], "<3.12"));
    assert_eq!(
        output.python_compatibility,
        PythonCompatibility::RequiresPythonMismatch
    );
    assert_eq!(output.dependencies.len(), 1);
    assert_eq!(output.requires_python.as_deref(), Some("<3.12"));
}

#[test]
fn pypy_uses_the_complete_language_version_for_requires_python() {
    let mut target = env();
    target.implementation_name = "pypy";
    target.platform_python_implementation = "PyPy";
    target.implementation_version = "8.0.1";
    for (spec, expected) in [
        ("<4", PythonCompatibility::Compatible),
        (">=8", PythonCompatibility::RequiresPythonMismatch),
        ("<3.12.6", PythonCompatibility::Compatible),
    ] {
        let output = compile_metadata_input(
            &facts_with_python(&[], spec),
            target,
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default(),
            MetadataInputLimits::default(),
        )
        .unwrap();
        assert_eq!(output.python_compatibility, expected);
    }
}

#[test]
fn inconsistent_explicit_context_is_rejected_even_without_dependency_markers() {
    let data = facts(&[]);
    for (short, full, implementation) in [
        ("3.12", "3.12", "3.12"),
        ("3.13", "3.12.5", "3.12.5"),
        ("3.12", "3.12.5", "99.1.0"),
    ] {
        let mut context = env();
        context.python_version = short;
        context.python_full_version = full;
        context.implementation_version = implementation;
        assert_eq!(
            compile_metadata_input(
                &data,
                context,
                MarkerSemantics::Current2026,
                PythonMarkerLimits::default(),
                ExtraTruthLimits::default(),
                MetadataInputLimits::default()
            )
            .unwrap_err(),
            MetadataInputError::InvalidContext
        );
    }
}
