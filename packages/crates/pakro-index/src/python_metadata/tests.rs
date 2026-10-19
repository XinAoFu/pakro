use super::*;
use std::error::Error as _;

struct Fixture {
    expected_name: NormalizedPythonName,
    expected_version: String,
    metadata_version: (u32, u32),
    name: String,
    version: String,
    requires_python: Option<String>,
    requirements: Vec<String>,
    extras: Vec<String>,
    dynamic: Vec<String>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            expected_name: NormalizedPythonName::new("demo-pkg").unwrap(),
            expected_version: "1.0".into(),
            metadata_version: (2, 6),
            name: "Demo_Pkg".into(),
            version: "1.0".into(),
            requires_python: None,
            requirements: vec![],
            extras: vec![],
            dynamic: vec![],
        }
    }
    fn fields(&self) -> PythonMetadataFields<'_> {
        PythonMetadataFields {
            metadata_version: self.metadata_version,
            name: &self.name,
            version: &self.version,
            requires_python: self.requires_python.as_deref(),
            requires_dist: &self.requirements,
            provides_extra: &self.extras,
            dynamic: &self.dynamic,
        }
    }
    fn project(
        &self,
        limits: PythonMetadataLimits,
    ) -> PythonMetadataResult<PythonDependencyMetadataFacts> {
        project_python_metadata(
            self.fields(),
            PythonMetadataExpectation {
                name: &self.expected_name,
                version: &self.expected_version,
            },
            limits,
        )
    }
}
fn invalid(
    result: PythonMetadataResult<PythonDependencyMetadataFacts>,
    field: PythonMetadataField,
    index: Option<usize>,
) {
    assert_eq!(
        result.unwrap_err(),
        PythonMetadataError::InvalidField { field, index }
    );
}
fn limit(
    result: PythonMetadataResult<PythonDependencyMetadataFacts>,
    kind: PythonMetadataLimitKind,
) {
    assert_eq!(
        result.unwrap_err(),
        PythonMetadataError::LimitExceeded(kind)
    );
}

#[test]
fn header_identity_uses_normalized_name_and_pep440_equality() {
    let mut fixture = Fixture::new();
    fixture.version = "1.0.0".into();
    let facts = fixture.project(Default::default()).unwrap();
    assert_eq!(facts.name().as_str(), "demo-pkg");
    assert_eq!(
        facts.version().parse::<pep440_rs::Version>().unwrap(),
        "1.0".parse::<pep440_rs::Version>().unwrap()
    );
    for name in ["different", "not a name", ""] {
        fixture.name = name.into();
        assert!(matches!(
            fixture.project(Default::default()),
            Err(PythonMetadataError::IdentityMismatch
                | PythonMetadataError::InvalidField {
                    field: PythonMetadataField::Name,
                    ..
                })
        ));
    }
    fixture.name = "Demo_Pkg".into();
    fixture.version = "2.0".into();
    assert_eq!(
        fixture.project(Default::default()).unwrap_err(),
        PythonMetadataError::IdentityMismatch
    );
    fixture.version = "not-a-version".into();
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::Version,
        None,
    );
    fixture.version = "1.0".into();
    fixture.expected_version = "not-a-version".into();
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::Version,
        None,
    );
}

#[test]
fn requires_dist_preserves_all_markers_extras_and_repeated_constraints() {
    let mut fixture = Fixture::new();
    fixture.requirements = vec![
        "requests[security,tests]>=2; python_version < '3.8' and extra == 'tls'".into(),
        "never>=1; python_version < '0'".into(),
        "requests<3".into(),
        "requests<3".into(),
    ];
    fixture.extras = vec!["tls".into()];
    let facts = fixture.project(Default::default()).unwrap();
    assert_eq!(facts.requirements().len(), fixture.requirements.len());
    assert_eq!(
        facts
            .requirements()
            .iter()
            .map(PythonRequirementDeclaration::raw_for_semantic_resolution)
            .collect::<Vec<_>>(),
        fixture
            .requirements
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(facts.requirements()[0].name(), "requests");
    assert_eq!(
        facts.requirements()[0]
            .requested_extras()
            .collect::<Vec<_>>(),
        vec!["security", "tests"]
    );
    assert_eq!(
        facts.requirements()[0].marker_semantics(),
        PythonMarkerSemantics::NeedsExactMarkerSemantics
    );
    assert_eq!(
        facts.requirements()[1].marker_semantics(),
        PythonMarkerSemantics::NeedsExactMarkerSemantics
    );
    assert_eq!(
        facts.requirements()[2].marker_semantics(),
        PythonMarkerSemantics::Unconditional
    );
    assert_eq!(facts.provided_extras(), &["tls".to_string()]);
    // Same distribution version in another selected wheel must retain its own
    // declaration facts, not a global name/version-only metadata memo.
    fixture.requirements = vec!["platform-other>=9".into()];
    let other = fixture.project(Default::default()).unwrap();
    assert_eq!(other.requirements()[0].name(), "platform-other");
    assert_eq!(facts.requirements()[0].name(), "requests");
}

#[test]
fn direct_reference_tokens_remain_unresolved_verbatim_without_ambient_expansion() {
    for token in [
        "https://files.example.test/dir;a/pkg.whl?token=synthetic-direct-secret&home=${HOME}",
        "git+https://git.example.test/repo.git@deadbeef#subdirectory=lib",
        "file:../relative-source",
        "../relative.whl",
    ] {
        let mut fixture = Fixture::new();
        fixture.requirements = vec![format!("direct @ {token}")];
        let facts = fixture.project(Default::default()).unwrap();
        let declaration = &facts.requirements()[0];
        let reference = declaration.unresolved_reference().unwrap();
        assert_eq!(reference.raw_for_source_binding(), token);
        assert_eq!(
            declaration.raw_for_semantic_resolution(),
            fixture.requirements[0]
        );
        assert_eq!(
            declaration.marker_semantics(),
            PythonMarkerSemantics::Unconditional
        );
        let rendered = format!(
            "{facts:?} {declaration:?} {reference:?} {reference} {:?}",
            fixture.fields()
        );
        for secret in [
            "synthetic-direct-secret",
            "${HOME}",
            "files.example.test",
            "relative-source",
            "deadbeef",
        ] {
            assert!(!rendered.contains(secret));
        }
    }
}

#[test]
fn invalid_requirement_errors_are_indexed_without_raw_text_or_source_chains() {
    for bad in [
        "broken[; synthetic-requirement-secret",
        "valid; python_version >>> 'synthetic-requirement-secret'",
        "valid; unknown_marker == 'synthetic-requirement-secret'",
    ] {
        let mut fixture = Fixture::new();
        fixture.requirements = vec!["first>=1".into(), bad.into()];
        let error = fixture.project(Default::default()).unwrap_err();
        assert_eq!(
            error,
            PythonMetadataError::InvalidField {
                field: PythonMetadataField::RequiresDist,
                index: Some(1)
            }
        );
        assert!(error.source().is_none());
        assert!(!format!("{error:?} {error}").contains("synthetic-requirement-secret"));
    }
}

#[test]
fn provides_extra_version_rules_and_normalization_collisions_are_explicit() {
    let mut fixture = Fixture::new();
    fixture.metadata_version = (2, 1);
    fixture.extras = vec!["Doc_Test".into(), "Fast.Foo".into()];
    let facts = fixture.project(Default::default()).unwrap();
    assert_eq!(
        facts.provided_extras(),
        &["doc-test".to_string(), "fast-foo".to_string()]
    );
    assert!(
        facts
            .warnings()
            .contains(&PythonMetadataWarning::LegacyExtraNormalized { index: 0 })
    );
    assert!(
        facts
            .warnings()
            .contains(&PythonMetadataWarning::LegacyExtraNormalized { index: 1 })
    );
    fixture.metadata_version = (2, 3);
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::ProvidesExtra,
        Some(0),
    );
    fixture.metadata_version = (2, 1);
    fixture.extras = vec!["doc-test".into(), "Doc_Test".into()];
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::ProvidesExtra,
        Some(1),
    );
    fixture.metadata_version = (2, 6);
    fixture.extras = vec!["doc-test".into(), "doc-test".into()];
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::ProvidesExtra,
        Some(1),
    );
}

#[test]
fn requires_python_is_validated_but_never_filtered_to_the_host() {
    let mut fixture = Fixture::new();
    fixture.requires_python = Some(">=99,!=99.0.1".into());
    let facts = fixture.project(Default::default()).unwrap();
    assert_eq!(facts.requires_python(), Some(">=99,!=99.0.1"));
    for value in [
        "",
        ">=3; os_name == 'posix'",
        "synthetic-requires-python-secret",
    ] {
        fixture.requires_python = Some(value.into());
        let error = fixture.project(Default::default()).unwrap_err();
        assert_eq!(
            error,
            PythonMetadataError::InvalidField {
                field: PythonMetadataField::RequiresPython,
                index: None
            }
        );
        assert!(!format!("{error:?} {error}").contains("synthetic-requires-python-secret"));
    }
}

#[test]
fn dynamic_fields_keep_values_and_metadata26_append_policy_without_early_transition() {
    let mut fixture = Fixture::new();
    fixture.requirements = vec!["base>=1".into()];
    fixture.dynamic = vec!["Requires-Dist".into()];
    fixture.metadata_version = (2, 5);
    let old = fixture.project(Default::default()).unwrap();
    assert_eq!(
        old.sdist_dynamic_policy(),
        SdistDynamicPolicy::ReplaceMarkedFields
    );
    assert_eq!(old.dynamic_fields(), &["requires-dist".to_string()]);
    assert_eq!(
        old.requirements()[0].raw_for_semantic_resolution(),
        "base>=1"
    );
    fixture.metadata_version = (2, 6);
    let current = fixture.project(Default::default()).unwrap();
    assert_eq!(
        current.sdist_dynamic_policy(),
        SdistDynamicPolicy::AppendMarkedMultipleUseFields
    );
    assert_eq!(
        current.requirements()[0].raw_for_semantic_resolution(),
        "base>=1"
    );
    fixture.metadata_version = (2, 1);
    let legacy = fixture.project(Default::default()).unwrap();
    assert_eq!(
        legacy.sdist_dynamic_policy(),
        SdistDynamicPolicy::LegacyUnspecified
    );
    assert!(
        legacy
            .warnings()
            .contains(&PythonMetadataWarning::DynamicBeforeMetadata22 { index: 0 })
    );
    fixture.metadata_version = (2, 6);
    for forbidden in ["Name", "Version", "Metadata-Version"] {
        fixture.dynamic = vec![forbidden.into()];
        invalid(
            fixture.project(Default::default()),
            PythonMetadataField::Dynamic,
            Some(0),
        );
    }
}

#[test]
fn core_metadata_versions_have_an_explicit_forward_compatibility_policy() {
    let mut fixture = Fixture::new();
    for version in [
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 1),
        (2, 2),
        (2, 3),
        (2, 4),
        (2, 5),
        (2, 6),
    ] {
        fixture.metadata_version = version;
        let facts = fixture.project(Default::default()).unwrap();
        assert_eq!(facts.metadata_version(), version);
    }
    fixture.metadata_version = (2, 99);
    assert!(
        fixture
            .project(Default::default())
            .unwrap()
            .warnings()
            .contains(&PythonMetadataWarning::NewerMetadataMinor {
                major: 2,
                minor: 99
            })
    );
    fixture.metadata_version = (3, 0);
    assert_eq!(
        fixture.project(Default::default()).unwrap_err(),
        PythonMetadataError::UnsupportedMajor(3)
    );
    fixture.metadata_version = (2, 0);
    invalid(
        fixture.project(Default::default()),
        PythonMetadataField::MetadataVersion,
        None,
    );
}

#[test]
fn public_input_limits_are_charged_before_parsing_or_collecting_declarations() {
    let mut fixture = Fixture::new();
    for dimension in 0..9 {
        let mut limits = PythonMetadataLimits::default();
        match dimension {
            0 => limits.max_total_bytes = 0,
            1 => limits.max_field_bytes = 0,
            2 => limits.max_requirements = 0,
            3 => limits.max_provided_extras = 0,
            4 => limits.max_requested_extras = 0,
            5 => limits.max_dynamic_fields = 0,
            6 => limits.max_marker_depth = 0,
            7 => limits.max_marker_terms = 0,
            _ => limits.max_syntax_tokens = 0,
        }
        assert_eq!(
            fixture.project(limits).unwrap_err(),
            PythonMetadataError::InvalidLimits
        );
    }
    let bytes = fixture.name.len() + fixture.version.len();
    fixture
        .project(PythonMetadataLimits {
            max_total_bytes: bytes,
            ..Default::default()
        })
        .unwrap();
    limit(
        fixture.project(PythonMetadataLimits {
            max_total_bytes: bytes - 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::TotalBytes,
    );
    let exact = fixture.name.len();
    fixture
        .project(PythonMetadataLimits {
            max_field_bytes: exact,
            ..Default::default()
        })
        .unwrap();
    limit(
        fixture.project(PythonMetadataLimits {
            max_field_bytes: exact - 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::FieldBytes,
    );
    fixture.requirements = vec!["broken[synthetic-count-secret".into(), "second".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_requirements: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::Requirements,
    );
    fixture.requirements.clear();
    fixture.extras = vec!["one".into(), "two".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_provided_extras: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::ProvidedExtras,
    );
    fixture.extras.clear();
    fixture.requirements = vec!["dep[one,two]>=1".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_requested_extras: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::RequestedExtras,
    );
    fixture.requirements.clear();
    fixture.dynamic = vec!["Requires-Dist".into(), "Provides-Extra".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_dynamic_fields: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::DynamicFields,
    );
    fixture.dynamic.clear();
    fixture.requirements = vec!["dep; ((os_name == 'posix'))".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_marker_depth: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::MarkerDepth,
    );
    fixture.requirements = vec!["dep; os_name == 'posix' and sys_platform == 'linux'".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_marker_terms: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::MarkerTerms,
    );
    fixture.requirements = vec!["dep; os_name == 'posix'".into()];
    limit(
        fixture.project(PythonMetadataLimits {
            max_syntax_tokens: 1,
            ..Default::default()
        }),
        PythonMetadataLimitKind::SyntaxTokens,
    );
}

#[test]
fn exact_marker_syntax_is_retained_without_approximating_effective_dependencies() {
    let mut fixture = Fixture::new();
    fixture.requirements = vec![
        "literal; 'synthetic-marker-secret' == 'different'".into(),
        "member; python_version in '3.10,3.11'".into(),
        "notmember; python_version not in '3.10,3.11'".into(),
    ];
    let facts = fixture.project(Default::default()).unwrap();
    assert_eq!(facts.requirements().len(), 3);
    for (original, declaration) in fixture.requirements.iter().zip(facts.requirements()) {
        assert_eq!(declaration.raw_for_semantic_resolution(), original);
        assert_eq!(
            declaration.marker_semantics(),
            PythonMarkerSemantics::NeedsExactMarkerSemantics
        );
        assert_eq!(declaration.exact_marker().unwrap().comparisons().count(), 1);
        assert!(!format!("{declaration:?}").contains("synthetic-marker-secret"));
    }
    assert!(
        facts
            .warnings()
            .iter()
            .all(|warning| !matches!(warning, PythonMetadataWarning::MarkerSyntax { .. }))
    );
    assert!(
        !format!("{facts:?} {:?} {:?}", facts.warnings(), fixture.fields())
            .contains("synthetic-marker-secret")
    );
}
