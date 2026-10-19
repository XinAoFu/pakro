//! Schema two retains all original roots, separately from effective graph edges.
use super::*;

fn inactive(raw: &[&str], limits: LockModelLimits) -> LockModelResult<PythonLockModel> {
    let key = GroupKey::new("", "main", [0x44; 32], LockModelLimits::default()).unwrap();
    PythonLockModel::from_parts(
        &[],
        &[LockGraphInput {
            key: &key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: RuntimePin::new([0x55; 32], 1, [0x44; 32]).unwrap(),
            root_declarations: raw,
            roots: &[],
            nodes: &[],
            edges: &[],
        }],
        limits,
    )
}
fn inactive_wire(declarations: &str) -> String {
    // Independent wire oracle. No encoder call or active-root inference.
    format!(
        "[python]\nschema = 2\npins = []\n[[python.groups]]\nconfig_scope = \"\"\ngroup = \"main\"\ntarget = \"{}\"\nmarker_semantics = \"current2026\"\nroot_declarations = {declarations}\nroots = []\nnodes = []\nedges = []\n[python.groups.runtime]\ncontent_tree = \"{}\"\nrecipe = 1\ntarget = \"{}\"\n",
        "44".repeat(32),
        "55".repeat(32),
        "44".repeat(32)
    )
}
fn safe_error(error: LockModelError, expected: LockModelError) {
    assert!(!format!("{error:?} {error}").contains("private-root-secret"));
    assert_eq!(error, expected);
}

#[test]
fn full_ordered_originals_keep_inactive_duplicates_tabs_and_quoted_uri() {
    let mut fixture = Fixture::new();
    let quoted = "inactive; os_name == 'https://private-root-secret@host/a'";
    fixture.root_declarations = vec![
        "demo-pkg==1.0.0",
        quoted,
        "demo-pkg==1.0.0",
        "inactive\t>=2",
    ];
    let model = fixture.model(LockModelLimits::default()).unwrap();
    let graph = model.graph(&fixture.key).unwrap();
    assert_eq!(graph.root_declarations(), fixture.root_declarations);
    assert_eq!(graph.roots()[0].declaration_index(), 0);
    assert!(!format!("{model:?} {graph:?}").contains("private-root-secret"));
}
#[test]
fn entirely_inactive_originals_survive_an_empty_effective_graph() {
    let declarations = [
        "inactive; sys_platform == 'win32'",
        "inactive; sys_platform == 'win32'",
    ];
    let model = inactive(&declarations, LockModelLimits::default()).unwrap();
    let graph = &model.graphs()[0];
    assert_eq!(graph.root_declarations(), declarations);
    assert!(graph.roots().is_empty() && graph.nodes().is_empty() && graph.edges().is_empty());
    assert!(model.pins().is_empty());
    let encoded = model.encode_projection(LockModelLimits::default()).unwrap();
    assert_eq!(
        PythonLockModel::decode_projection(&encoded, LockModelLimits::default()).unwrap(),
        model
    );
}
#[test]
fn active_references_are_canonical_by_index_without_merging_duplicate_originals() {
    let mut fixture = Fixture::new();
    fixture.root_declarations = vec!["demo-pkg==1.0.0", "inactive", "demo-pkg==1.0.0"];
    fixture.roots = vec![
        RootInput {
            declaration_index: 2,
            node: "demo-pkg",
        },
        RootInput {
            declaration_index: 0,
            node: "demo-pkg",
        },
    ];
    let model = fixture.model(LockModelLimits::default()).unwrap();
    assert_eq!(
        model.graphs()[0]
            .roots()
            .iter()
            .map(LockedRoot::declaration_index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert_eq!(
        model.graphs()[0].root_declarations(),
        fixture.root_declarations
    );
}
#[test]
fn invalid_active_indices_are_not_guessed_from_node_or_raw_text() {
    for declaration_index in [1, usize::MAX] {
        let mut fixture = Fixture::new();
        fixture.roots[0].declaration_index = declaration_index;
        safe_error(
            fixture.model(LockModelLimits::default()).unwrap_err(),
            LockModelError::Reference(LockReference::RootDeclaration),
        );
    }
}
#[test]
fn one_original_position_cannot_claim_two_active_roots_even_with_same_node() {
    for node in ["demo-pkg", "dep"] {
        let mut fixture = Fixture::new();
        fixture.roots.push(RootInput {
            declaration_index: 0,
            node,
        });
        safe_error(
            fixture.model(LockModelLimits::default()).unwrap_err(),
            LockModelError::Duplicate(LockIdentity::RootReference),
        );
    }
}
#[test]
fn complete_inactive_inputs_are_bounded_before_owned_clone() {
    let declarations = ["inactive", "inactive"];
    for (limits, expected) in [
        (
            LockModelLimits {
                max_roots_per_group: 1,
                ..LockModelLimits::default()
            },
            LockLimit::Roots,
        ),
        (
            LockModelLimits {
                max_total_references: 1,
                ..LockModelLimits::default()
            },
            LockLimit::References,
        ),
        (
            LockModelLimits {
                max_declaration_bytes: 7,
                ..LockModelLimits::default()
            },
            LockLimit::DeclarationBytes,
        ),
        (
            LockModelLimits {
                max_total_utf8_bytes: 19,
                ..LockModelLimits::default()
            },
            LockLimit::TotalUtf8Bytes,
        ),
    ] {
        safe_error(
            inactive(&declarations, limits).unwrap_err(),
            LockModelError::Limit(expected),
        );
    }
}
#[test]
fn inactive_declarations_have_the_same_raw_source_and_control_guards() {
    for (raw, expected) in [
        (
            "inactive @ https://private-root-secret@host/a",
            LockModelError::UnsupportedSourceKind,
        ),
        (
            "inactive\nprivate-root-secret",
            LockModelError::InvalidField(LockField::Declaration),
        ),
        ("", LockModelError::InvalidField(LockField::Declaration)),
    ] {
        safe_error(
            inactive(&[raw], LockModelLimits::default()).unwrap_err(),
            expected,
        );
    }
}
#[test]
fn independent_schema_two_wire_binds_all_originals_and_active_indices() {
    let raw = golden();
    let model = PythonLockModel::decode_projection(&raw, LockModelLimits::default()).unwrap();
    assert_model(&model);
    let raw = inactive_wire("[\"first\", \"first\", \"inactive; os_name == 'other'\"]");
    let model = PythonLockModel::decode_projection(&raw, LockModelLimits::default()).unwrap();
    assert_eq!(
        model.graphs()[0].root_declarations(),
        ["first", "first", "inactive; os_name == 'other'"]
    );
    assert!(model.graphs()[0].roots().is_empty());
}
#[test]
fn schema_two_root_fields_are_required_closed_and_index_integers_are_strict() {
    let raw = inactive_wire("[\"inactive\"]");
    PythonLockModel::decode_projection(&raw, LockModelLimits::default()).unwrap();
    for changed in [
        raw.replace("root_declarations = [\"inactive\"]\n", ""),
        raw.replace(
            "root_declarations = [\"inactive\"]",
            "root_declarations = 'private-root-secret'",
        ),
        raw.replace(
            "root_declarations = [\"inactive\"]",
            "root_declarations = [1]",
        ),
        raw.replace(
            "root_declarations = [\"inactive\"]",
            "root_declarations = [{ secret = 'private-root-secret' }]",
        ),
        raw.replace(
            "root_declarations = [\"inactive\"]",
            "root_declarations = [\"inactive\"]\nroot_declarations = []",
        ),
    ] {
        safe_error(
            PythonLockModel::decode_projection(&changed, LockModelLimits::default()).unwrap_err(),
            LockModelError::InvalidToml,
        );
    }
    for schema in [0, 1, 3] {
        let changed = raw.replace("schema = 2", &format!("schema = {schema}"));
        safe_error(
            PythonLockModel::decode_projection(&changed, LockModelLimits::default()).unwrap_err(),
            LockModelError::UnsupportedSchema,
        );
    }
    let active = golden();
    PythonLockModel::decode_projection(&active, LockModelLimits::default()).unwrap();
    for value in [
        "-1",
        "0.5",
        "'private-root-secret'",
        "{secret='private-root-secret'}",
    ] {
        let changed = active.replace(
            "declaration_index = 0",
            &format!("declaration_index = {value}"),
        );
        safe_error(
            PythonLockModel::decode_projection(&changed, LockModelLimits::default()).unwrap_err(),
            LockModelError::InvalidToml,
        );
    }
    let legacy = active.replace("declaration_index = 0", "declaration = 'demo-pkg==1.0.0'");
    safe_error(
        PythonLockModel::decode_projection(&legacy, LockModelLimits::default()).unwrap_err(),
        LockModelError::InvalidToml,
    );
    let out_of_range = active.replace("declaration_index = 0", "declaration_index = 1");
    safe_error(
        PythonLockModel::decode_projection(&out_of_range, LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::RootDeclaration),
    );
    let duplicate = active.replace("[[python.groups.roots]]\ndeclaration_index = 0\nnode = \"demo-pkg\"", "[[python.groups.roots]]\ndeclaration_index = 0\nnode = \"demo-pkg\"\n[[python.groups.roots]]\ndeclaration_index = 0\nnode = \"demo-pkg\"");
    safe_error(
        PythonLockModel::decode_projection(&duplicate, LockModelLimits::default()).unwrap_err(),
        LockModelError::Duplicate(LockIdentity::RootReference),
    );
}
#[test]
fn full_original_wire_and_encoder_charge_exact_counts_utf8_refs_and_escaped_bytes() {
    let raw = inactive_wire("[\"inactive\", \"inactive\"]");
    let exact = LockModelLimits {
        max_roots_per_group: 2,
        max_total_references: 2,
        max_total_utf8_bytes: 20,
        max_declaration_bytes: 8,
        ..LockModelLimits::default()
    };
    let model = PythonLockModel::decode_projection(&raw, exact).unwrap();
    assert_eq!(
        model.graphs()[0].root_declarations(),
        ["inactive", "inactive"]
    );
    for (limits, expected) in [
        (
            LockModelLimits {
                max_roots_per_group: 1,
                ..exact
            },
            LockLimit::Roots,
        ),
        (
            LockModelLimits {
                max_total_references: 1,
                ..exact
            },
            LockLimit::References,
        ),
        (
            LockModelLimits {
                max_total_utf8_bytes: 19,
                ..exact
            },
            LockLimit::TotalUtf8Bytes,
        ),
        (
            LockModelLimits {
                max_declaration_bytes: 7,
                ..exact
            },
            LockLimit::DeclarationBytes,
        ),
    ] {
        safe_error(
            PythonLockModel::decode_projection(&raw, limits).unwrap_err(),
            LockModelError::Limit(expected),
        );
        safe_error(
            model.encode_projection(limits).unwrap_err(),
            LockModelError::Limit(expected),
        );
    }
    let escaped = inactive(
        &["inactive; os_name == '引号\t\"\\'"],
        LockModelLimits::default(),
    )
    .unwrap();
    let encoded = escaped
        .encode_projection(LockModelLimits::default())
        .unwrap();
    assert!(encoded.contains("schema = 2") && encoded.contains("root_declarations = ["));
    assert!(encoded.contains("\\t\\\"\\\\"));
    assert_eq!(
        PythonLockModel::decode_projection(&encoded, LockModelLimits::default()).unwrap(),
        escaped
    );
    assert_eq!(
        escaped
            .encode_projection(LockModelLimits {
                max_document_bytes: encoded.len(),
                ..LockModelLimits::default()
            })
            .unwrap(),
        encoded
    );
    safe_error(
        escaped
            .encode_projection(LockModelLimits {
                max_document_bytes: encoded.len() - 1,
                ..LockModelLimits::default()
            })
            .unwrap_err(),
        LockModelError::Limit(LockLimit::DocumentBytes),
    );
    assert_eq!(hex::encode(id(WHEEL_ID).as_bytes()), WHEEL_ID);
}
