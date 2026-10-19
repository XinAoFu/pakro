use super::*;
use pakro_index::python_markers::{PythonMarkerWarningKind, parse_python_marker};

fn env() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "linux",
        platform_release: "6.8.0",
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
fn truth(raw: &str) -> MarkerExtraTruth {
    compile_extra_truth(
        &parse_python_marker(raw, PythonMarkerLimits::default()).unwrap(),
        env(),
        MarkerSemantics::Current2026,
        PythonMarkerLimits::default(),
        ExtraTruthLimits::default(),
    )
    .unwrap()
}
#[test]
fn exact_and_or_grouping_becomes_a_finite_truth_table_without_losing_terms() {
    let t = truth("extra == 'a' and (extra == 'b' or extra == 'c')");
    assert_eq!(t.extra_names(), ["a", "b", "c"]);
    assert_eq!(t.enabled_masks(), [3, 5, 7]);
    assert_eq!(t.semantics(), MarkerSemantics::Current2026);
}
#[test]
fn normalized_extra_aliases_share_one_bit_in_both_operand_directions() {
    let t = truth("extra == 'Speed_Up' and 'speed.up' == extra");
    assert_eq!(t.extra_names(), ["speed-up"]);
    assert_eq!(t.enabled_masks(), [1]);
}
#[test]
fn only_the_explicit_environment_can_activate_constant_marker_branches() {
    let raw = "sys_platform == 'linux' or extra == 'a'";
    assert_eq!(truth(raw).enabled_masks(), [0, 1]);
    let ast = parse_python_marker(raw, PythonMarkerLimits::default()).unwrap();
    let mut environment = env();
    environment.sys_platform = "darwin";
    let t = compile_extra_truth(
        &ast,
        environment,
        MarkerSemantics::Current2026,
        PythonMarkerLimits::default(),
        ExtraTruthLimits::default(),
    )
    .unwrap();
    assert_eq!(t.enabled_masks(), [1]);
}
#[test]
fn unsupported_extra_operations_stay_false_and_retain_safe_diagnostics() {
    let t = truth("extra != 'private-extra-literal' or extra in 'other-private-literal'");
    assert!(t.extra_names().is_empty());
    assert!(t.enabled_masks().is_empty());
    assert!(
        t.possible_warnings()
            .iter()
            .all(|w| w.kind == PythonMarkerWarningKind::UnsupportedExtraComparison)
    );
    assert_eq!(t.possible_warnings().len(), 2);
    assert!(!format!("{t:?}").contains("private-extra-literal"));
}
#[test]
fn unavailable_lock_fields_cannot_hide_in_a_short_circuited_branch() {
    let ast = parse_python_marker(
        "sys_platform == 'linux' or 'dev' in dependency_groups",
        PythonMarkerLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        compile_extra_truth(
            &ast,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            ExtraTruthLimits::default()
        ),
        Err(ExtraTruthError::Marker(_))
    ));
}
#[test]
fn expansion_and_aggregate_node_budgets_are_charged_before_enumeration() {
    let ast = parse_python_marker(
        "extra == 'a' or extra == 'b' or extra == 'c'",
        PythonMarkerLimits::default(),
    )
    .unwrap();
    for limits in [
        ExtraTruthLimits {
            max_relevant_extras: 2,
            ..Default::default()
        },
        ExtraTruthLimits {
            max_truth_states: 7,
            ..Default::default()
        },
        ExtraTruthLimits {
            max_total_evaluation_nodes: ast.node_count() * 8 - 1,
            ..Default::default()
        },
        ExtraTruthLimits {
            max_name_bytes: 2,
            ..Default::default()
        },
    ] {
        assert_eq!(
            compile_extra_truth(
                &ast,
                env(),
                MarkerSemantics::Current2026,
                PythonMarkerLimits::default(),
                limits
            )
            .unwrap_err(),
            ExtraTruthError::Limit
        );
    }
    let zero = ExtraTruthLimits {
        max_truth_states: 0,
        ..Default::default()
    };
    assert_eq!(
        compile_extra_truth(
            &ast,
            env(),
            MarkerSemantics::Current2026,
            PythonMarkerLimits::default(),
            zero
        )
        .unwrap_err(),
        ExtraTruthError::InvalidLimits
    );
}
#[test]
fn an_expression_parsed_under_looser_limits_is_rechecked_by_the_actual_evaluator() {
    let ast = parse_python_marker(
        "extra == 'a' and extra == 'b'",
        PythonMarkerLimits::default(),
    )
    .unwrap();
    let tight = PythonMarkerLimits {
        max_nodes: 1,
        ..Default::default()
    };
    assert!(matches!(
        compile_extra_truth(
            &ast,
            env(),
            MarkerSemantics::Current2026,
            tight,
            ExtraTruthLimits::default()
        ),
        Err(ExtraTruthError::Marker(_))
    ));
}
#[test]
fn invalid_literals_follow_the_actual_evaluators_complete_comparison_validation() {
    for raw in [
        "sys_platform == 'linux' or extra == 'invalid extra name'",
        "extra == 'a' and extra == 'invalid extra name'",
    ] {
        let ast = parse_python_marker(raw, PythonMarkerLimits::default()).unwrap();
        assert_eq!(
            compile_extra_truth(
                &ast,
                env(),
                MarkerSemantics::Current2026,
                PythonMarkerLimits::default(),
                ExtraTruthLimits::default()
            )
            .unwrap_err(),
            ExtraTruthError::Marker(PythonMarkerError::InvalidExtraName { comparison: 1 })
        );
    }
}
