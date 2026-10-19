use super::*;

fn parsed(raw: &str) -> PythonRequirementSyntax {
    parse_python_requirement(raw, Default::default()).unwrap()
}

#[test]
fn raw_rhs_preserves_leading_zero_v_prefix_case_and_repeated_clauses() {
    for spec in ["===01.0", "===v1.0", "===1.0RC1", "===01.0,===1.0"] {
        let requirement = parsed(&format!("demo-pkg{spec}"));
        assert!(requirement.version_specifiers().is_some());
        assert_eq!(requirement.raw_version_specifiers(), Some(spec));
    }
}

#[test]
fn raw_specifier_range_preserves_parentheses_spaces_and_optional_tail_comma() {
    let raw = "  Demo.Pkg[fast] ( ===01.0 , >= 1, ) \t; python_version >= '3.11'";
    let requirement = parsed(raw);
    assert_eq!(requirement.name(), "demo-pkg");
    assert!(requirement.marker().is_some());
    assert_eq!(
        requirement.raw_version_specifiers(),
        Some("( ===01.0 , >= 1, )")
    );
    assert_eq!(
        raw.get(requirement.spans().version_specifiers().unwrap()),
        requirement.raw_version_specifiers()
    );
}

#[test]
fn uri_and_quoted_marker_literals_cannot_be_misread_as_version_constraints() {
    let control = parsed("demo-pkg>=1; os_name == '===01.0'");
    assert_eq!(control.raw_version_specifiers(), Some(">=1"));
    for raw in [
        "demo-pkg; os_name == '===01.0'",
        "demo-pkg[fast]",
        "demo-pkg @ https://example.invalid/===01.0;sig=abc ; os_name == '===2'",
    ] {
        let requirement = parsed(raw);
        assert!(requirement.version_specifiers().is_none());
        assert!(requirement.raw_version_specifiers().is_none());
        assert!(requirement.spans().version_specifiers().is_none());
    }
}

#[test]
fn actual_metadata_requirement_forwards_the_same_raw_specifier() {
    use crate::python_metadata::*;
    use crate::python_source::NormalizedPythonName;
    let name = NormalizedPythonName::new("demo-pkg").unwrap();
    let declarations = ["child===01.0; python_version >= '3.11'".to_owned()];
    let facts = project_python_metadata(
        PythonMetadataFields {
            metadata_version: (2, 6),
            name: "demo-pkg",
            version: "1.0",
            requires_python: None,
            requires_dist: &declarations,
            provides_extra: &[],
            dynamic: &[],
        },
        PythonMetadataExpectation {
            name: &name,
            version: "1.0",
        },
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        facts.requirements()[0].raw_for_semantic_resolution(),
        declarations[0]
    );
    assert_eq!(
        facts.requirements()[0].raw_version_specifiers(),
        Some("===01.0")
    );
}
