//! Hostile wire changes use the handwritten fixture, never the encoder as an oracle.

use super::*;

const PRIVATE: &str = "strict-codec-private-marker";
const PIN: &str = "[[python.pins]]";
const GROUP: &str = "[[python.groups]]";
const RUNTIME: &str = "[python.groups.runtime]";
const ROOT: &str = "[[python.groups.roots]]";
const NODE: &str = "[[python.groups.nodes]]";
const EDGE: &str = "[[python.groups.edges]]";

fn section(raw: &str, heading: &str) -> std::ops::Range<usize> {
    let mut offset = 0;
    let mut start = None;
    for line in raw.split_inclusive('\n') {
        if start.is_some() && line.starts_with('[') {
            return start.unwrap()..offset;
        }
        if line.trim_end() == heading {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.expect("handwritten fixture section must exist")..raw.len()
}

fn change_section(raw: &str, heading: &str, edit: impl FnOnce(&str) -> String) -> String {
    let range = section(raw, heading);
    let changed = edit(&raw[range.clone()]);
    let mut result = raw.to_owned();
    result.replace_range(range, &changed);
    assert_ne!(result, raw, "the hostile wire change must actually apply");
    result
}

fn change_field(raw: &str, heading: &str, key: &str, value: Option<&str>) -> String {
    change_section(raw, heading, |body| {
        let mut changed = String::new();
        let mut found = false;
        for line in body.split_inclusive('\n') {
            if line.starts_with(&format!("{key} = ")) {
                assert!(!found, "handwritten valid control has a duplicate field");
                found = true;
                if let Some(value) = value {
                    changed.push_str(&format!("{key} = {value}\n"));
                }
            } else {
                changed.push_str(line);
            }
        }
        assert!(
            found,
            "handwritten fixture field must exist: {heading} {key}"
        );
        changed
    })
}

fn valid_control(raw: &str, limits: LockModelLimits) -> PythonLockModel {
    // Distinguish malformed test construction from a Python schema rejection.
    raw.parse::<toml::Table>()
        .expect("test input must be syntactically valid TOML");
    PythonLockModel::decode_projection(raw, limits)
        .expect("the independent handwritten positive control must pass")
}

fn rejected(raw: &str, label: &str) -> LockModelError {
    let error =
        PythonLockModel::decode_projection(raw, LockModelLimits::default()).expect_err(label);
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(PRIVATE), "{label}: {diagnostic}");
    error
}

#[test]
fn every_nested_record_rejects_unknown_fields_without_revealing_their_values() {
    let raw = golden();
    for heading in [PIN, GROUP, RUNTIME, ROOT, NODE, EDGE] {
        assert_model(&valid_control(&raw, LockModelLimits::default()));
        let changed = change_section(&raw, heading, |body| {
            let (header, fields) = body.split_once('\n').unwrap();
            format!(
                "{header}\nunknown_private_field = {{ nested = [{{ secret = \"{PRIVATE}\" }}] }}\n{fields}"
            )
        });
        changed.parse::<toml::Table>().unwrap();
        assert_eq!(rejected(&changed, heading), LockModelError::InvalidToml);
    }
}

#[test]
fn each_required_nested_scalar_is_required_and_cannot_be_replaced_by_a_secret_table() {
    let raw = golden();
    let cases: &[(&str, &[&str])] = &[
        (
            PIN,
            &[
                "id",
                "registry",
                "canonical_base_fingerprint",
                "project",
                "version",
                "filename",
                "kind",
                "digest",
                "size",
                "evidence",
            ],
        ),
        (
            GROUP,
            &["config_scope", "group", "target", "marker_semantics"],
        ),
        (RUNTIME, &["content_tree", "recipe", "target"]),
        (ROOT, &["declaration_index", "node"]),
        (NODE, &["name", "version", "source_pin"]),
        (EDGE, &["from", "to", "declaration"]),
    ];
    for &(heading, fields) in cases {
        for &field in fields {
            assert_model(&valid_control(&raw, LockModelLimits::default()));
            let missing = change_field(&raw, heading, field, None);
            missing.parse::<toml::Table>().unwrap();
            assert_eq!(
                rejected(&missing, &format!("missing {heading}.{field}")),
                LockModelError::InvalidToml
            );
            let secret_value = format!("{{ secret = \"{PRIVATE}\" }}");
            let wrong_type = change_field(&raw, heading, field, Some(&secret_value));
            wrong_type.parse::<toml::Table>().unwrap();
            assert_eq!(
                rejected(&wrong_type, &format!("wrong type {heading}.{field}")),
                LockModelError::InvalidToml
            );
        }
    }
}

#[test]
fn unsupported_tags_and_duplicate_fields_have_independent_valid_controls() {
    let raw = golden();
    for (heading, field, value, expected) in [
        ("[python]", "schema", "3", LockModelError::UnsupportedSchema),
        (
            PIN,
            "kind",
            "\"git\"",
            LockModelError::UnsupportedSourceKind,
        ),
        (PIN, "evidence", "\"unknown\"", LockModelError::InvalidToml),
        (
            GROUP,
            "marker_semantics",
            "\"unversioned\"",
            LockModelError::UnsupportedMarkerSemantics,
        ),
        (
            RUNTIME,
            "recipe",
            "2",
            LockModelError::UnsupportedRuntimeRecipe,
        ),
    ] {
        assert_model(&valid_control(&raw, LockModelLimits::default()));
        let changed = change_field(&raw, heading, field, Some(value));
        changed.parse::<toml::Table>().unwrap();
        assert_eq!(rejected(&changed, field), expected, "{heading}.{field}");
    }
    for (heading, field) in [
        (PIN, "evidence"),
        (GROUP, "group"),
        (RUNTIME, "recipe"),
        (ROOT, "node"),
        (NODE, "version"),
        (EDGE, "declaration"),
    ] {
        assert_model(&valid_control(&raw, LockModelLimits::default()));
        let changed = change_section(&raw, heading, |body| {
            let original = body
                .lines()
                .find(|line| line.starts_with(&format!("{field} = ")))
                .unwrap();
            format!("{body}{original}\n")
        });
        // Duplicate TOML keys are invalid syntax even before the typed seed.
        // This is a real public projection rejection, not an unknown-field alias.
        assert!(changed.parse::<toml::Table>().is_err());
        assert_eq!(rejected(&changed, heading), LockModelError::InvalidToml);
    }
}

fn two_groups_with_two_roots_and_edges() -> String {
    let raw = golden();
    let raw = raw.replace(
        "root_declarations = [\"demo-pkg==1.0.0\"]",
        "root_declarations = [\"demo-pkg==1.0.0\", \"demo-pkg==1.0.0\"]",
    );
    let repeated = change_section(&raw, ROOT, |body| {
        format!(
            "{body}{}",
            body.replace("declaration_index = 0", "declaration_index = 1")
        )
    });
    let repeated = change_section(&repeated, EDGE, |body| format!("{body}{body}"));
    let group_start = repeated.find("[[python.groups]]").unwrap();
    let second = repeated[group_start..].replacen("group = \"main\"", "group = \"other\"", 1);
    format!("{repeated}{second}")
}

#[test]
fn group_root_edge_node_and_global_reference_budgets_accept_exact_and_reject_next_item() {
    let raw = two_groups_with_two_roots_and_edges();
    // Both groups legitimately share pins and duplicate metadata declarations.
    // Cardinalities are fixed by handwritten wire, not read from the model.
    let exact = LockModelLimits {
        max_groups: 2,
        max_roots_per_group: 2,
        max_edges_per_group: 2,
        max_nodes_per_group: 2,
        max_total_nodes: 4,
        max_total_references: 24,
        ..LockModelLimits::default()
    };
    for (limits, expected) in [
        (
            LockModelLimits {
                max_groups: 1,
                ..exact
            },
            LockLimit::Groups,
        ),
        (
            LockModelLimits {
                max_roots_per_group: 1,
                ..exact
            },
            LockLimit::Roots,
        ),
        (
            LockModelLimits {
                max_edges_per_group: 1,
                ..exact
            },
            LockLimit::Edges,
        ),
        (
            LockModelLimits {
                max_nodes_per_group: 1,
                ..exact
            },
            LockLimit::Nodes,
        ),
        (
            LockModelLimits {
                max_total_nodes: 3,
                ..exact
            },
            LockLimit::TotalNodes,
        ),
        (
            LockModelLimits {
                max_total_references: 23,
                ..exact
            },
            LockLimit::References,
        ),
    ] {
        let control = valid_control(&raw, exact);
        assert_eq!(control.graphs().len(), 2);
        let error = PythonLockModel::decode_projection(&raw, limits).unwrap_err();
        assert_eq!(error, LockModelError::Limit(expected));
        assert_eq!(
            control.encode_projection(limits).unwrap_err(),
            LockModelError::Limit(expected),
            "encoding cannot bypass a tighter {expected:?} model limit"
        );
    }
}

#[test]
fn retained_identity_and_artifact_sizes_obey_exact_limits_in_both_directions() {
    let raw = golden();
    let exact = LockModelLimits {
        max_field_bytes: "demo_pkg-1.0.0-py3-none-any.whl".len(),
        max_declaration_bytes: "demo-pkg==1.0.0".len(),
        max_artifact_bytes: 456,
        ..LockModelLimits::default()
    };
    for (limits, expected) in [
        (
            LockModelLimits {
                max_field_bytes: exact.max_field_bytes - 1,
                ..exact
            },
            LockLimit::FieldBytes,
        ),
        (
            LockModelLimits {
                max_declaration_bytes: exact.max_declaration_bytes - 1,
                ..exact
            },
            LockLimit::DeclarationBytes,
        ),
        (
            LockModelLimits {
                max_artifact_bytes: 455,
                ..exact
            },
            LockLimit::ArtifactBytes,
        ),
    ] {
        let control = valid_control(&raw, exact);
        assert_model(&control);
        assert_eq!(
            PythonLockModel::decode_projection(&raw, limits).unwrap_err(),
            LockModelError::Limit(expected)
        );
        assert_eq!(
            control.encode_projection(limits).unwrap_err(),
            LockModelError::Limit(expected)
        );
    }
}
