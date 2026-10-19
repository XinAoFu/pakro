use super::*;

fn parsed(raw: &str) -> PythonMarkerExpression {
    parse_python_marker(raw, PythonMarkerLimits::default()).unwrap()
}
fn environment() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "sentinel-os",
        sys_platform: "sentinel-platform",
        platform_release: "14.5.0",
        platform_system: "SentinelOS",
        platform_version: "sentinel build",
        platform_machine: "virtual_cpu",
        platform_python_implementation: "CPython",
        python_version: "3.13",
        python_full_version: "3.13.5",
        implementation_name: "cpython",
        implementation_version: "3.13.5",
    }
}
fn evaluated(
    raw: &str,
    env: PythonMarkerEnvironment<'_>,
    scope: PythonMarkerScope<'_>,
) -> PythonMarkerEvaluation {
    evaluate_python_marker(
        &parsed(raw),
        env,
        scope,
        MarkerSemantics::Current2026,
        PythonMarkerLimits::default(),
    )
    .unwrap()
}
fn has_warning(result: &PythonMarkerEvaluation, kind: PythonMarkerWarningKind) -> bool {
    result.warnings().iter().any(|warning| warning.kind == kind)
}

#[test]
fn parser_preserves_original_quotes_whitespace_literal_and_operator_spans() {
    let raw = "\t(os_name not  in \"a;'b or c\")  or python_version=== '3.13'\t";
    let ast = parsed(raw);
    assert_eq!(ast.raw_for_semantic_resolution(), raw);
    let comparisons: Vec<_> = ast.comparisons().collect();
    assert_eq!(comparisons.len(), 2);
    assert_eq!(
        comparisons[0].left(),
        PythonMarkerOperand::Field(PythonMarkerField::OsName)
    );
    assert_eq!(
        comparisons[0].right(),
        PythonMarkerOperand::Literal("a;'b or c")
    );
    assert_eq!(&raw[comparisons[0].operator_span()], "not  in");
    assert_eq!(&raw[comparisons[1].operator_span()], "===");
    assert_eq!(&raw[comparisons[0].left_span()], "os_name");
    assert_eq!(&raw[comparisons[0].right_span()], "\"a;'b or c\"");
    assert_eq!(&raw[comparisons[0].span()], "os_name not  in \"a;'b or c\"");
}

#[test]
fn parser_recognizes_all_operators_without_keyword_or_prefix_confusion() {
    for (text, expected) in [
        ("<", PythonMarkerOperator::Less),
        ("<=", PythonMarkerOperator::LessEqual),
        ("!=", PythonMarkerOperator::NotEqual),
        ("===", PythonMarkerOperator::ArbitraryEqual),
        ("==", PythonMarkerOperator::Equal),
        (">=", PythonMarkerOperator::GreaterEqual),
        (">", PythonMarkerOperator::Greater),
        ("~=", PythonMarkerOperator::Compatible),
        ("in", PythonMarkerOperator::In),
        ("not in", PythonMarkerOperator::NotIn),
    ] {
        let ast = parsed(&format!("os_name {text} 'posix'"));
        assert_eq!(ast.comparisons().next().unwrap().operator(), expected);
    }
    assert_eq!(
        parsed("'x'in'prefix-x'")
            .comparisons()
            .next()
            .unwrap()
            .operator(),
        PythonMarkerOperator::In
    );
    assert_eq!(
        parsed("'x'not\tin'prefix-x'")
            .comparisons()
            .next()
            .unwrap()
            .operator(),
        PythonMarkerOperator::NotIn
    );
    assert!(matches!(
        parse_python_marker("os_name notin 'posix'", Default::default()),
        Err(PythonMarkerError::Syntax { .. })
    ));
    assert!(matches!(
        parse_python_marker("extra_suffix == 'x'", Default::default()),
        Err(PythonMarkerError::UnknownField { offset: 0 })
    ));
}

#[test]
fn parser_builds_and_before_or_and_keeps_explicit_group_nodes() {
    let ast = parsed("os_name == 'a' or sys_platform == 'b' and platform_system == 'c'");
    let MarkerNode::Or(left, right) = &ast.nodes[ast.root] else {
        panic!("wrong outer precedence")
    };
    assert!(matches!(ast.nodes[*left], MarkerNode::Comparison(_)));
    assert!(matches!(ast.nodes[*right], MarkerNode::And(_, _)));
    let grouped = parsed("(os_name == 'a' or sys_platform == 'b') and platform_system == 'c'");
    let MarkerNode::And(left, _) = &grouped.nodes[grouped.root] else {
        panic!("explicit group was lost")
    };
    let MarkerNode::Group(child) = &grouped.nodes[*left] else {
        panic!("group node was erased")
    };
    assert!(matches!(grouped.nodes[*child], MarkerNode::Or(_, _)));
    assert_eq!(
        parsed("os_name=='a' or os_name=='b' or os_name=='c'")
            .comparisons()
            .count(),
        3
    );
}

#[test]
fn parser_never_drops_literal_literal_or_variable_variable_comparisons() {
    let ast = parsed("'1' == '1.0' and python_full_version > implementation_version");
    let comparisons: Vec<_> = ast.comparisons().collect();
    assert_eq!(comparisons.len(), 2);
    assert_eq!(comparisons[0].left(), PythonMarkerOperand::Literal("1"));
    assert_eq!(comparisons[0].right(), PythonMarkerOperand::Literal("1.0"));
    assert_eq!(
        comparisons[1].left(),
        PythonMarkerOperand::Field(PythonMarkerField::PythonFullVersion)
    );
    assert_eq!(
        comparisons[1].right(),
        PythonMarkerOperand::Field(PythonMarkerField::ImplementationVersion)
    );
    assert_eq!(comparisons[1].operator(), PythonMarkerOperator::Greater);
}

#[test]
fn parser_recognizes_eleven_base_and_three_contextual_fields_independently_of_scope() {
    for (name, field) in [
        ("os_name", PythonMarkerField::OsName),
        ("sys_platform", PythonMarkerField::SysPlatform),
        ("platform_release", PythonMarkerField::PlatformRelease),
        ("platform_system", PythonMarkerField::PlatformSystem),
        ("platform_version", PythonMarkerField::PlatformVersion),
        ("platform_machine", PythonMarkerField::PlatformMachine),
        (
            "platform_python_implementation",
            PythonMarkerField::PlatformPythonImplementation,
        ),
        ("python_version", PythonMarkerField::PythonVersion),
        ("python_full_version", PythonMarkerField::PythonFullVersion),
        ("implementation_name", PythonMarkerField::ImplementationName),
        (
            "implementation_version",
            PythonMarkerField::ImplementationVersion,
        ),
        ("extra", PythonMarkerField::Extra),
        ("extras", PythonMarkerField::Extras),
        ("dependency_groups", PythonMarkerField::DependencyGroups),
    ] {
        let ast = parsed(&format!("{name} == 'sentinel'"));
        assert_eq!(
            ast.comparisons().next().unwrap().left(),
            PythonMarkerOperand::Field(field)
        );
    }
}

#[test]
fn parser_rejects_unknown_fields_and_invalid_grammar_without_echoing_input() {
    for raw in [
        "",
        "(",
        "os_name = 'posix'",
        "os_name == 'unterminated",
        "python_version > '3' < '4'",
        "not (os_name == 'posix')",
        "'x' in ['x']",
        "os_name=='posix' else sys_platform=='linux'",
    ] {
        assert!(
            matches!(
                parse_python_marker(raw, Default::default()),
                Err(PythonMarkerError::Syntax { .. })
            ),
            "unexpected syntax classification"
        );
    }
    let secret =
        "unknown_marker == 'https://user:synthetic-marker-secret@example.test/?token=private'";
    let error = parse_python_marker(secret, Default::default()).unwrap_err();
    assert_eq!(error, PythonMarkerError::UnknownField { offset: 0 });
    assert!(!format!("{error:?} {error}").contains("synthetic-marker-secret"));
}

#[test]
fn parser_handles_utf8_and_opposite_quotes_but_not_controls_or_undefined_escapes() {
    let ast = parsed("os_name == 'pösix' and platform_version == \"a'b\"");
    let values: Vec<_> = ast
        .comparisons()
        .map(|comparison| comparison.right())
        .collect();
    assert_eq!(
        values,
        vec![
            PythonMarkerOperand::Literal("pösix"),
            PythonMarkerOperand::Literal("a'b")
        ]
    );
    assert_eq!(parsed("os_name == 'po\tsix'").comparisons().count(), 1);
    for raw in [
        "os_name == 'a\nb'",
        "os_name == 'a\0b'",
        r#"os_name == 'a\nb'"#,
        "os_name\n== 'x'",
        "os_name\u{a0}== 'x'",
    ] {
        assert!(matches!(
            parse_python_marker(raw, Default::default()),
            Err(PythonMarkerError::Syntax { .. })
        ));
    }
}

fn zero_limits() -> Vec<PythonMarkerLimits> {
    let d = PythonMarkerLimits::default();
    vec![
        PythonMarkerLimits {
            max_input_bytes: 0,
            ..d
        },
        PythonMarkerLimits {
            max_literal_bytes: 0,
            ..d
        },
        PythonMarkerLimits { max_tokens: 0, ..d },
        PythonMarkerLimits { max_nodes: 0, ..d },
        PythonMarkerLimits {
            max_group_depth: 0,
            ..d
        },
        PythonMarkerLimits {
            max_environment_bytes: 0,
            ..d
        },
        PythonMarkerLimits {
            max_environment_field_bytes: 0,
            ..d
        },
        PythonMarkerLimits {
            max_set_entries: 0,
            ..d
        },
        PythonMarkerLimits {
            max_evaluation_steps: 0,
            ..d
        },
    ]
}

#[test]
fn parser_charges_public_limits_before_collecting_nodes_or_literals() {
    let raw = "os_name=='posix'";
    for limits in zero_limits() {
        assert_eq!(
            parse_python_marker(raw, limits).unwrap_err(),
            PythonMarkerError::InvalidLimits
        );
    }
    let exact = PythonMarkerLimits {
        max_input_bytes: raw.len(),
        max_literal_bytes: 5,
        max_tokens: 3,
        max_nodes: 1,
        ..Default::default()
    };
    assert_eq!(parse_python_marker(raw, exact).unwrap().node_count(), 1);
    for (limits, kind) in [
        (
            PythonMarkerLimits {
                max_input_bytes: raw.len() - 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::InputBytes,
        ),
        (
            PythonMarkerLimits {
                max_literal_bytes: 4,
                ..Default::default()
            },
            PythonMarkerLimitKind::LiteralBytes,
        ),
        (
            PythonMarkerLimits {
                max_tokens: 2,
                ..Default::default()
            },
            PythonMarkerLimitKind::Tokens,
        ),
    ] {
        assert_eq!(
            parse_python_marker(raw, limits).unwrap_err(),
            PythonMarkerError::LimitExceeded(kind)
        );
    }
    assert_eq!(
        parse_python_marker(
            "os_name=='a' and os_name=='b'",
            PythonMarkerLimits {
                max_nodes: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::Nodes)
    );
    assert!(
        parse_python_marker(
            "(os_name=='a')",
            PythonMarkerLimits {
                max_group_depth: 1,
                ..Default::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse_python_marker(
            "((os_name=='a'))",
            PythonMarkerLimits {
                max_group_depth: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::GroupDepth)
    );
}

#[test]
fn evaluation_uses_all_eleven_supplied_values_without_host_defaults() {
    let raw = "os_name=='sentinel-os' and sys_platform=='sentinel-platform' and platform_release=='14.5.0' and platform_system=='SentinelOS' and platform_version=='sentinel build' and platform_machine=='virtual_cpu' and platform_python_implementation=='CPython' and python_version=='3.13' and python_full_version=='3.13.5' and implementation_name=='cpython' and implementation_version=='3.13.5'";
    assert!(evaluated(raw, environment(), PythonMarkerScope::Environment).value());
    assert!(
        !evaluated(
            "sys_platform=='linux'",
            environment(),
            PythonMarkerScope::Environment
        )
        .value()
    );
    let env = PythonMarkerEnvironment {
        sys_platform: "explicit-second-platform",
        ..environment()
    };
    assert!(
        evaluated(
            "sys_platform=='explicit-second-platform'",
            env,
            PythonMarkerScope::Environment
        )
        .value()
    );
}

#[test]
fn string_fields_follow_current_ordering_and_explicit_discouraged_compatibility() {
    let env = PythonMarkerEnvironment {
        os_name: "3.10",
        ..environment()
    };
    for (operator, literal, expected) in [
        ("==", "3.10", true),
        ("!=", "3.9", true),
        (">=", "3.9", false),
        ("<=", "3.10", true),
        (">", "3.1", false),
        ("<", "9", false),
        ("in", "prefix-3.10-suffix", true),
        ("not in", "other", true),
        ("~=", "3.10", true),
        ("===", "3.10", true),
    ] {
        let result = evaluated(
            &format!("os_name {operator} '{literal}'"),
            env,
            PythonMarkerScope::Environment,
        );
        assert_eq!(result.value(), expected);
        if ["~=", "==="].contains(&operator) {
            assert!(has_warning(
                &result,
                PythonMarkerWarningKind::StringCompatibleEquality
            ));
        }
        if ["<", "<=", ">", ">="].contains(&operator) {
            assert!(has_warning(
                &result,
                PythonMarkerWarningKind::StringOrderedComparison
            ));
        }
    }
    assert!(
        !evaluated(
            "os_name=='3.10'",
            PythonMarkerEnvironment {
                os_name: "3.010",
                ..env
            },
            PythonMarkerScope::Environment
        )
        .value()
    );
}

#[test]
fn version_fields_reuse_pep440_without_candidate_prerelease_filtering() {
    for (actual, clause, expected) in [
        ("3.13.5", ">= '3.13'", true),
        ("3.13.5", "~= '3.13.0'", true),
        ("3.13.5", "== '3.13.*'", true),
        ("3.13.5+vendor", "== '3.13.5'", true),
        ("3.13.5+vendor", "== '3.13.5+other'", false),
        ("1!3.0", "> '3.99'", true),
        ("3.14.0a1", ">= '3.13'", true),
        ("3.14.0a1", "< '3.14'", false),
        ("3.14.0a1", "== '3.14.*'", true),
        ("3.13.5", "!= '3.13.*'", false),
    ] {
        let result = evaluated(
            &format!("python_full_version {clause}"),
            PythonMarkerEnvironment {
                python_full_version: actual,
                ..environment()
            },
            PythonMarkerScope::Environment,
        );
        assert_eq!(result.value(), expected, "PEP440 marker comparison differs");
    }
    assert!(
        evaluated(
            "'3.12' < python_version",
            environment(),
            PythonMarkerScope::Environment
        )
        .value()
    );
}

#[test]
fn ordinary_pre_bound_markers_keep_single_clause_parsing_and_the_original_direction() {
    let env = PythonMarkerEnvironment {
        python_version: "3.11",
        python_full_version: "3.11.0rc1",
        implementation_version: "3.11.0rc1",
        ..environment()
    };
    let results = [
        "python_full_version < '3.11.0rc2'",
        "'3.11.0rc0' < python_full_version",
        "implementation_version < '3.11.0rc2'",
        "python_full_version < '3.11.0'",
        "python_full_version < '3.11.0rc1'",
    ]
    .map(|raw| evaluated(raw, env, PythonMarkerScope::Environment));
    let single_clause = evaluated(
        "python_full_version < '3.11.0rc2,>=99'",
        env,
        PythonMarkerScope::Environment,
    );
    assert!(has_warning(
        &single_clause,
        PythonMarkerWarningKind::VersionStringFallback
    ));
    assert!(has_warning(
        &single_clause,
        PythonMarkerWarningKind::StringOrderedComparison
    ));
    assert!(!single_clause.value());
    assert_eq!(
        results.each_ref().map(|result| result.value()),
        [true, true, true, false, false]
    );
    assert!(results.iter().all(|result| result.warnings().is_empty()));
}

#[test]
fn reversed_version_operands_keep_the_original_pep440_candidate_direction() {
    assert!(
        !evaluated(
            "'3.13' ~= python_full_version",
            environment(),
            PythonMarkerScope::Environment,
        )
        .value()
    );
    assert!(
        evaluated(
            "python_full_version ~= '3.13'",
            environment(),
            PythonMarkerScope::Environment,
        )
        .value()
    );
    let env = PythonMarkerEnvironment {
        implementation_version: "3.13.post1",
        ..environment()
    };
    assert!(
        evaluated(
            "'3.13' < implementation_version",
            env,
            PythonMarkerScope::Environment,
        )
        .value()
    );
    assert!(
        !evaluated(
            "implementation_version > '3.13'",
            env,
            PythonMarkerScope::Environment,
        )
        .value()
    );
}

#[test]
fn version_arbitrary_equality_uses_original_ascii_casefold_without_padding() {
    assert!(
        evaluated(
            "implementation_version === 'legacybuild'",
            PythonMarkerEnvironment {
                implementation_version: "LegacyBuild",
                ..environment()
            },
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        !evaluated(
            "python_full_version === '3.13'",
            PythonMarkerEnvironment {
                python_full_version: "3.13.0",
                ..environment()
            },
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        !evaluated(
            "python_full_version === '3.13.5'",
            PythonMarkerEnvironment {
                python_full_version: "3.13.05",
                ..environment()
            },
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        evaluated(
            "python_full_version == '3.13.5'",
            PythonMarkerEnvironment {
                python_full_version: "3.13.05",
                ..environment()
            },
            PythonMarkerScope::Environment
        )
        .value()
    );
    let result = evaluated(
        "os_name === 'POSIX'",
        PythonMarkerEnvironment {
            os_name: "posix",
            ..environment()
        },
        PythonMarkerScope::Environment,
    );
    assert!(!result.value());
    assert!(has_warning(
        &result,
        PythonMarkerWarningKind::StringCompatibleEquality
    ));
}

#[test]
fn version_containment_is_explicit_false_in_both_directions() {
    for raw in [
        "python_version in '3.13 3.14'",
        "python_version not in 'other'",
        "'3.13' in python_version",
        "'other' not in python_version",
    ] {
        let result = evaluated(raw, environment(), PythonMarkerScope::Environment);
        assert!(!result.value());
        assert!(has_warning(
            &result,
            PythonMarkerWarningKind::VersionContainment
        ));
    }
}

#[test]
fn both_platform_version_fields_support_version_string_and_fallback_semantics() {
    for field in ["platform_release", "platform_version"] {
        let env = PythonMarkerEnvironment {
            platform_release: "14.5",
            platform_version: "14.5",
            ..environment()
        };
        assert!(
            evaluated(
                &format!("{field} > '12'"),
                env,
                PythonMarkerScope::Environment
            )
            .value()
        );
        let env = PythonMarkerEnvironment {
            platform_release: "6.8-lab",
            platform_version: "6.8-lab",
            ..environment()
        };
        assert!(
            evaluated(
                &format!("'lab' in {field}"),
                env,
                PythonMarkerScope::Environment
            )
            .value()
        );
        let result = evaluated(
            &format!("{field} > '0'"),
            env,
            PythonMarkerScope::Environment,
        );
        assert!(!result.value());
        assert!(has_warning(
            &result,
            PythonMarkerWarningKind::VersionStringFallback
        ));
    }
    let result = evaluated(
        "python_version == 'not-a-version'",
        PythonMarkerEnvironment {
            python_version: "not-a-version",
            ..environment()
        },
        PythonMarkerScope::Environment,
    );
    assert!(result.value());
    assert!(has_warning(
        &result,
        PythonMarkerWarningKind::VersionStringFallback
    ));
}

#[test]
fn literal_and_variable_comparisons_are_evaluated_instead_of_dropped() {
    assert!(
        !evaluated(
            "'1' == '1.0'",
            environment(),
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        evaluated(
            "'part' in 'counterpart'",
            environment(),
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(!evaluated("'a' < 'b'", environment(), PythonMarkerScope::Environment).value());
    let env = PythonMarkerEnvironment {
        os_name: "same",
        implementation_name: "same",
        implementation_version: "3.12.0",
        ..environment()
    };
    assert!(
        evaluated(
            "os_name == implementation_name",
            env,
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        evaluated(
            "python_full_version > implementation_version",
            env,
            PythonMarkerScope::Environment
        )
        .value()
    );
    assert!(
        !evaluated(
            "os_name != implementation_name",
            env,
            PythonMarkerScope::Environment
        )
        .value()
    );
}

#[test]
fn dependency_extra_uses_the_complete_normalized_requested_set() {
    let scope = PythonMarkerScope::Dependency {
        requested_extras: &["foo", "bar", "Foo_Bar"],
    };
    assert!(evaluated("extra=='foo' and extra=='bar'", environment(), scope).value());
    assert!(evaluated("extra=='foo.bar'", environment(), scope).value());
    assert!(evaluated("'FOO' == extra", environment(), scope).value());
    assert!(!evaluated("extra=='absent'", environment(), scope).value());
    assert!(
        !evaluated(
            "extra=='foo'",
            environment(),
            PythonMarkerScope::Dependency {
                requested_extras: &[]
            }
        )
        .value()
    );
    for raw in ["extra!='absent'", "extra in 'foo'", "'foo' != extra"] {
        let result = evaluated(raw, environment(), scope);
        assert!(!result.value());
        assert!(has_warning(
            &result,
            PythonMarkerWarningKind::UnsupportedExtraComparison
        ));
    }
}

#[test]
fn lock_membership_uses_only_selected_normalized_sets_and_explicit_false_for_other_ops() {
    let scope = PythonMarkerScope::Lock {
        selected_extras: &["Gui"],
        dependency_groups: &["test-env"],
    };
    assert!(
        evaluated(
            "'GUI' in extras and 'test_env' in dependency_groups",
            environment(),
            scope
        )
        .value()
    );
    assert!(evaluated("'absent' not in extras", environment(), scope).value());
    assert!(!evaluated("'unselected' in dependency_groups", environment(), scope).value());
    for raw in [
        "extras=='gui'",
        "'gui'==extras",
        "extras in 'gui'",
        "os_name in extras",
    ] {
        let result = evaluated(raw, environment(), scope);
        assert!(!result.value());
        assert!(has_warning(
            &result,
            PythonMarkerWarningKind::UnsupportedSetComparison
        ));
    }
}

#[test]
fn unavailable_context_fields_are_not_hidden_by_boolean_short_circuiting() {
    for (raw, scope, field) in [
        (
            "os_name=='never' and extra=='foo'",
            PythonMarkerScope::Environment,
            PythonMarkerField::Extra,
        ),
        (
            "os_name=='sentinel-os' or 'gui' in extras",
            PythonMarkerScope::Dependency {
                requested_extras: &[],
            },
            PythonMarkerField::Extras,
        ),
        (
            "'test' in dependency_groups",
            PythonMarkerScope::Dependency {
                requested_extras: &[],
            },
            PythonMarkerField::DependencyGroups,
        ),
        (
            "os_name=='never' and extra=='foo'",
            PythonMarkerScope::Lock {
                selected_extras: &[],
                dependency_groups: &[],
            },
            PythonMarkerField::Extra,
        ),
    ] {
        assert_eq!(
            evaluate_python_marker(
                &parsed(raw),
                environment(),
                scope,
                MarkerSemantics::Current2026,
                Default::default()
            )
            .unwrap_err(),
            PythonMarkerError::ContextUnavailable(field)
        );
    }
    assert!(matches!(
        parse_python_marker(
            "os_name=='sentinel-os' or unknown_field=='x'",
            Default::default()
        ),
        Err(PythonMarkerError::UnknownField { .. })
    ));
}

#[test]
fn evaluation_rechecks_ast_environment_set_and_work_budgets() {
    let ast = parsed("os_name=='sentinel-os'");
    for limits in zero_limits() {
        assert_eq!(
            evaluate_python_marker(
                &ast,
                environment(),
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                limits
            )
            .unwrap_err(),
            PythonMarkerError::InvalidLimits
        );
    }
    for (limits, kind) in [
        (
            PythonMarkerLimits {
                max_input_bytes: 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::InputBytes,
        ),
        (
            PythonMarkerLimits {
                max_literal_bytes: 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::LiteralBytes,
        ),
        (
            PythonMarkerLimits {
                max_tokens: 2,
                ..Default::default()
            },
            PythonMarkerLimitKind::Tokens,
        ),
        (
            PythonMarkerLimits {
                max_environment_bytes: 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::EnvironmentBytes,
        ),
        (
            PythonMarkerLimits {
                max_environment_field_bytes: 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::EnvironmentFieldBytes,
        ),
        (
            PythonMarkerLimits {
                max_evaluation_steps: 1,
                ..Default::default()
            },
            PythonMarkerLimitKind::EvaluationSteps,
        ),
    ] {
        assert_eq!(
            evaluate_python_marker(
                &ast,
                environment(),
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                limits
            )
            .unwrap_err(),
            PythonMarkerError::LimitExceeded(kind)
        );
    }
    assert!(
        evaluate_python_marker(
            &ast,
            environment(),
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            PythonMarkerLimits {
                max_evaluation_steps: 3,
                ..Default::default()
            }
        )
        .unwrap()
        .value()
    );
    assert_eq!(
        evaluate_python_marker(
            &ast,
            environment(),
            PythonMarkerScope::Dependency {
                requested_extras: &["foo", "foo"]
            },
            MarkerSemantics::Current2026,
            PythonMarkerLimits {
                max_set_entries: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::SetEntries)
    );
    assert_eq!(
        evaluate_python_marker(
            &ast,
            environment(),
            PythonMarkerScope::Dependency {
                requested_extras: &["invalid extra"]
            },
            MarkerSemantics::Current2026,
            Default::default()
        )
        .unwrap_err(),
        PythonMarkerError::InvalidContextName {
            field: PythonMarkerField::Extra,
            index: 0
        }
    );
    let larger = parsed("os_name=='a' and os_name=='b'");
    assert_eq!(
        evaluate_python_marker(
            &larger,
            environment(),
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            PythonMarkerLimits {
                max_nodes: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        PythonMarkerError::LimitExceeded(PythonMarkerLimitKind::Nodes)
    );
}

#[test]
fn semantic_version_is_explicit_and_all_diagnostics_hide_literal_and_context_secrets() {
    let raw = "'https://user:synthetic-marker-secret@example.test/?token=private' ~= 'different'";
    let ast = parsed(raw);
    let scope = PythonMarkerScope::Dependency {
        requested_extras: &["synthetic-extra-secret"],
    };
    let env = PythonMarkerEnvironment {
        os_name: "https://user:synthetic-context-secret@example.test/",
        ..environment()
    };
    let result = evaluate_python_marker(
        &ast,
        env,
        scope,
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap();
    assert_eq!(result.semantics(), MarkerSemantics::Current2026);
    assert!(!result.value());
    assert!(has_warning(
        &result,
        PythonMarkerWarningKind::StringCompatibleEquality
    ));
    let views: Vec<_> = ast.comparisons().collect();
    let formatted = format!("{ast:?} {views:?} {env:?} {scope:?} {result:?}");
    for secret in [
        "synthetic-marker-secret",
        "synthetic-context-secret",
        "synthetic-extra-secret",
    ] {
        assert!(!formatted.contains(secret));
    }
    assert_eq!(ast.raw_for_semantic_resolution(), raw);
    assert!(
        result
            .warnings()
            .iter()
            .all(|warning| warning.comparison_index == 0)
    );
}
