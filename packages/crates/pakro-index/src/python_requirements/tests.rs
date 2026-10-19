use super::*;
use crate::python_markers::{
    MarkerSemantics, PythonMarkerEnvironment, PythonMarkerOperator, PythonMarkerScope,
    evaluate_python_marker,
};
use std::str::FromStr;

fn parsed(raw: &str) -> PythonRequirementSyntax {
    parse_python_requirement(raw, Default::default()).unwrap()
}
fn environment() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "linux",
        platform_release: "6.8",
        platform_system: "Linux",
        platform_version: "fixture",
        platform_machine: "aarch64",
        platform_python_implementation: "CPython",
        python_version: "3.13",
        python_full_version: "3.13.5",
        implementation_name: "cpython",
        implementation_version: "3.13.5",
    }
}

#[test]
fn name_extras_and_legacy_parenthesized_ranges_preserve_the_complete_prefix() {
    let raw = "DeMo.Pkg[Foo_Bar,tests] (>=1.0, !=1.5.*, <2,)";
    let result = parsed(raw);
    assert_eq!(result.name(), "demo-pkg");
    assert_eq!(
        result.requested_extras().collect::<Vec<_>>(),
        vec!["foo-bar", "tests"]
    );
    let specifiers = result.version_specifiers().unwrap();
    assert!(specifiers.contains(&pep440_rs::Version::from_str("1.4").unwrap()));
    assert!(!specifiers.contains(&pep440_rs::Version::from_str("1.5.1").unwrap()));
    assert!(!specifiers.contains(&pep440_rs::Version::from_str("2.0").unwrap()));
    assert_eq!(result.spans().prefix(), 0..raw.len());
    assert!(result.marker().is_none());
    assert!(result.spans().uri().is_none());
    assert_eq!(result.raw_for_semantic_resolution(), raw);
}

#[test]
fn normal_marker_spans_preserve_suffix_whitespace_and_actual_exact_evaluation() {
    let raw = "demo_pkg>=1 ;  python_version >= '3.13' \t";
    let result = parsed(raw);
    let spans = result.spans();
    assert_eq!(&raw[spans.prefix()], "demo_pkg>=1 ");
    assert_eq!(
        &raw[spans.marker().unwrap()],
        "  python_version >= '3.13' \t"
    );
    let marker = result.marker().unwrap();
    assert_eq!(
        marker.raw_for_semantic_resolution(),
        &raw[spans.marker().unwrap()]
    );
    assert!(
        evaluate_python_marker(
            marker,
            environment(),
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            Default::default()
        )
        .unwrap()
        .value()
    );
}

#[test]
fn semicolons_fragments_and_signatures_inside_a_uri_are_not_markers() {
    let uri = "https://user:synthetic-uri-secret@example.test/a;param?ticket=${HOME}#sha256=abc";
    let raw = format!("demo_pkg @ {uri}");
    let result = parsed(&raw);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        uri
    );
    assert_eq!(&raw[result.spans().uri().unwrap()], uri);
    assert!(result.marker().is_none());
    assert!(result.spans().marker().is_none());
    assert_eq!(result.spans().prefix(), 0..raw.len());
}

#[test]
fn whitespace_after_the_uri_distinguishes_the_real_marker_delimiter() {
    let uri = "https://example.test/a;param?ticket=synthetic-uri-secret";
    let raw = format!("demo_pkg[docs] @ {uri} \t; extra == 'docs'");
    let result = parsed(&raw);
    assert_eq!(result.requested_extras().collect::<Vec<_>>(), vec!["docs"]);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        uri
    );
    assert_eq!(&raw[result.spans().uri().unwrap()], uri);
    assert_eq!(&raw[result.spans().marker().unwrap()], " extra == 'docs'");
    assert_eq!(result.marker().unwrap().comparisons().count(), 1);
}

#[test]
fn quoted_marker_semicolons_do_not_replace_the_outer_uri_boundary() {
    let uri = "https://example.test/a;first?data=semi;colon";
    let marker = " os_name == 'a;and;or' or platform_version == \"https://example.test/x;y\"";
    let raw = format!("demo @ {uri} ;{marker}");
    let result = parsed(&raw);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        uri
    );
    assert_eq!(
        result.marker().unwrap().raw_for_semantic_resolution(),
        marker
    );
    assert_eq!(&raw[result.spans().marker().unwrap()], marker);
    assert_eq!(result.marker().unwrap().comparisons().count(), 2);
}

#[test]
fn missing_uris_empty_markers_and_trailing_junk_cannot_be_silently_truncated() {
    for raw in [
        "demo @",
        "demo @ https://example.test/a garbage",
        "demo;",
        "demo @ https://example.test/a ; \t",
        "demo[] anything",
        "demo[not-closed",
    ] {
        assert!(
            matches!(
                parse_python_requirement(raw, Default::default()),
                Err(PythonRequirementError::Syntax { .. })
            ),
            "outer syntax was not rejected explicitly"
        );
    }
}

#[test]
fn invalid_markers_fail_instead_of_becoming_unconditional_or_library_approximations() {
    let error = parse_python_requirement(
        "demo;unknown_marker == 'synthetic-marker-secret'",
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        PythonRequirementError::Marker(PythonMarkerError::UnknownField { offset: 0 })
    );
    assert!(!format!("{error:?} {error}").contains("synthetic-marker-secret"));
    assert!(matches!(
        parse_python_requirement("demo;os_name >>> 'x'", Default::default()),
        Err(PythonRequirementError::Marker(
            PythonMarkerError::Syntax { .. }
        ))
    ));
}

#[test]
fn exact_marker_text_never_reaches_the_actual_library_prefix_parser() {
    let raw = "dep[Foo.Bar] >=1 ; 'synthetic-marker-secret' == 'different' and python_full_version > implementation_version and python_version in '3.13'";
    let mut observed = Vec::new();
    let result = parse_with_prefix_observer(raw, Default::default(), &mut |prefix| {
        observed.push(prefix.to_owned())
    })
    .unwrap();
    assert_eq!(observed, vec!["dep[Foo.Bar] >=1 "]);
    assert!(result.prefix.marker.is_true());
    let marker = result.marker().unwrap();
    assert_eq!(marker.comparisons().count(), 3);
    assert_eq!(
        marker.comparisons().last().unwrap().operator(),
        PythonMarkerOperator::In
    );
    assert_eq!(result.raw_for_semantic_resolution(), raw);
}

#[test]
fn debug_and_errors_hide_source_and_marker_secrets_without_changing_explicit_raw_getters() {
    let uri = "https://user:synthetic-source-secret@example.test/a?token=private";
    let raw = format!("demo @ {uri} ; os_name == 'synthetic-marker-secret'");
    let result = parsed(&raw);
    let formatted = format!(
        "{result:?} {:?} {:?} {:?}",
        result.spans(),
        result.marker(),
        result.unresolved_reference()
    );
    assert!(!formatted.contains("synthetic-source-secret"));
    assert!(!formatted.contains("synthetic-marker-secret"));
    assert_eq!(result.raw_for_semantic_resolution(), raw);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        uri
    );
    let error = parse_python_requirement(
        "bad name @ https://user:synthetic-source-secret@example.test/a",
        Default::default(),
    )
    .unwrap_err();
    assert!(matches!(error, PythonRequirementError::Syntax { .. }));
    assert!(!format!("{error:?} {error}").contains("synthetic-source-secret"));
}

#[test]
fn public_limits_are_charged_before_prefix_library_calls_or_owned_collection() {
    let defaults = PythonRequirementLimits::default();
    let mut zero_marker = defaults.marker;
    zero_marker.max_tokens = 0;
    for limits in [
        PythonRequirementLimits {
            max_input_bytes: 0,
            ..defaults
        },
        PythonRequirementLimits {
            max_requested_extras: 0,
            ..defaults
        },
        PythonRequirementLimits {
            max_syntax_tokens: 0,
            ..defaults
        },
        PythonRequirementLimits {
            max_marker_comparisons: 0,
            ..defaults
        },
        PythonRequirementLimits {
            marker: zero_marker,
            ..defaults
        },
    ] {
        assert_eq!(
            parse_python_requirement("foo", limits).unwrap_err(),
            PythonRequirementError::InvalidLimits
        );
    }
    assert_eq!(
        parse_python_requirement(
            "foo",
            PythonRequirementLimits {
                max_input_bytes: 3,
                max_syntax_tokens: 1,
                ..defaults
            }
        )
        .unwrap()
        .name(),
        "foo"
    );
    for (raw, limits, kind) in [
        (
            "foo",
            PythonRequirementLimits {
                max_input_bytes: 2,
                ..defaults
            },
            PythonRequirementLimitKind::InputBytes,
        ),
        (
            "foo[a,b,c]",
            PythonRequirementLimits {
                max_requested_extras: 2,
                ..defaults
            },
            PythonRequirementLimitKind::RequestedExtras,
        ),
        (
            "foo>=1",
            PythonRequirementLimits {
                max_syntax_tokens: 2,
                ..defaults
            },
            PythonRequirementLimitKind::SyntaxTokens,
        ),
        (
            "foo;os_name=='a' and sys_platform=='b'",
            PythonRequirementLimits {
                max_marker_comparisons: 1,
                ..defaults
            },
            PythonRequirementLimitKind::MarkerComparisons,
        ),
    ] {
        let mut calls = 0;
        assert_eq!(
            parse_with_prefix_observer(raw, limits, &mut |_| calls += 1).unwrap_err(),
            PythonRequirementError::LimitExceeded(kind)
        );
        assert_eq!(calls, 0);
    }
    assert_eq!(
        parse_python_requirement(
            "foo[a,b]",
            PythonRequirementLimits {
                max_requested_extras: 2,
                ..defaults
            }
        )
        .unwrap()
        .requested_extras()
        .count(),
        2
    );
    let raw = "foo @ https://example.test/a;param \t;os_name=='x'";
    let spans = preflight_python_requirement(raw, defaults).unwrap();
    assert_eq!(&raw[spans.uri().unwrap()], "https://example.test/a;param");
}

#[test]
fn adjacent_at_uri_trailing_semicolons_and_version_parentheses_have_distinct_roles() {
    let raw = "demo@https://example.test/a;";
    let result = parsed(raw);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "https://example.test/a;"
    );
    assert!(result.marker().is_none());
    let raw = "demo @ https://example.test/a; \t; extra=='docs'";
    let result = parsed(raw);
    assert_eq!(
        result
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "https://example.test/a;"
    );
    assert_eq!(result.marker().unwrap().comparisons().count(), 1);
    let raw = "demo (>=1,!=2) ;os_name=='x'";
    let result = parsed(raw);
    assert_eq!(&raw[result.spans().prefix()], "demo (>=1,!=2) ");
    assert!(result.version_specifiers().is_some());
}

#[test]
fn direct_reference_tokens_do_not_expand_environment_or_bind_relative_paths() {
    for uri in [
        "../local;tree",
        "git+https://example.test/repo.git@full-commit#subdirectory=src",
        "file:../literal/${HOME}",
        "https://example.test/signed?token=${HOME}",
    ] {
        let raw = format!("DeMo_Pkg[Foo_Bar] @ {uri}");
        let result = parsed(&raw);
        assert_eq!(result.name(), "demo-pkg");
        assert_eq!(
            result.requested_extras().collect::<Vec<_>>(),
            vec!["foo-bar"]
        );
        assert_eq!(
            result
                .unresolved_reference()
                .unwrap()
                .raw_for_source_binding(),
            uri
        );
        assert_eq!(result.raw_for_semantic_resolution(), raw);
        assert!(result.marker().is_none());
    }
}
