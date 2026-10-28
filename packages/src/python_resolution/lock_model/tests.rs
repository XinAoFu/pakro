use super::*;
use serde::de::{MapAccess, Visitor};

#[path = "tests/complete_roots.rs"]
mod complete_roots;
#[path = "tests/recheck_limits.rs"]
mod recheck_limits;
#[path = "tests/source_builds.rs"]
mod source_builds;
#[path = "tests/strict_matrix.rs"]
mod strict_matrix;

const WHEEL_ID: &str = "db7eeda292aaf412fb1502d5dbaa86d941d6b769e2b1f28cb711d648e7938e18";
const SDIST_ID: &str = "2a2316af267dbdc0d732dbd479e0b861c17cf6b9f2b7fc27ad6cba0c461d14a4";
fn id(value: &str) -> PinId {
    PinId(hex::decode(value).unwrap().try_into().unwrap())
}
fn file() -> RegistryFileRef {
    RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [0x11; 32],
            project: "demo-pkg",
            version: "1.0.0",
            filename: "demo_pkg-1.0.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        LockModelLimits::default(),
    )
    .unwrap()
}

struct Fixture {
    pins: Vec<ContentPin>,
    key: GroupKey,
    runtime: RuntimePin,
    root_declarations: Vec<&'static str>,
    roots: Vec<RootInput<'static>>,
    nodes: Vec<NodeInput<'static>>,
    edges: Vec<EdgeInput<'static>>,
}
impl Fixture {
    fn new() -> Self {
        // Independent pure-data oracle. These are not private source/verified
        // artifact capabilities; no production pin constructor is bypassed to
        // fake byte acquisition. IDs were separately computed with hashlib.
        let wheel = ContentPin {
            id: id(WHEEL_ID),
            file: file(),
            digest: Digest::Sha256([0x22; 32]),
            size: 123,
            evidence: PinEvidence::Advertised,
        };
        let sdist = ContentPin {
            id: id(SDIST_ID),
            file: RegistryFileRef {
                registry_id: "pypi".into(),
                canonical_base_fingerprint: [0x11; 32],
                project: "dep".into(),
                version: "2.0".into(),
                filename: "dep-2.0.tar.gz".into(),
                kind: RegistryArtifactKind::Sdist,
            },
            digest: Digest::Sha512([0x33; 64]),
            size: 456,
            evidence: PinEvidence::ObservedDuringFreshResolution,
        };
        Self {
            pins: vec![wheel, sdist],
            key: GroupKey::new("", "main", [0x44; 32], LockModelLimits::default()).unwrap(),
            runtime: RuntimePin::new([0x55; 32], 1, [0x44; 32]).unwrap(),
            root_declarations: vec!["demo-pkg==1.0.0"],
            roots: vec![RootInput {
                declaration_index: 0,
                node: "demo-pkg",
            }],
            nodes: vec![
                NodeInput {
                    name: "demo-pkg",
                    version: "1.0.0",
                    source_pin: id(WHEEL_ID),
                },
                NodeInput {
                    name: "dep",
                    version: "2.0",
                    source_pin: id(SDIST_ID),
                },
            ],
            edges: vec![EdgeInput {
                from: "demo-pkg",
                to: "dep",
                declaration: "dep>=2",
            }],
        }
    }
    fn input(&self) -> LockGraphInput<'_> {
        LockGraphInput {
            key: &self.key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: self.runtime,
            root_declarations: &self.root_declarations,
            roots: &self.roots,
            nodes: &self.nodes,
            edges: &self.edges,
        }
    }
    fn model(&self, limits: LockModelLimits) -> LockModelResult<PythonLockModel> {
        PythonLockModel::from_parts(&self.pins, &[self.input()], limits)
    }
}
fn golden() -> String {
    // Handwritten format/field oracle, independent of model encoder and pin ID
    // implementation. Substitutions are only the fixed externally computed IDs.
    r#"[python]
schema = 2
[[python.pins]]
id = "WHEEL_ID"
registry = "pypi"
canonical_base_fingerprint = "BASE"
project = "demo-pkg"
version = "1.0.0"
filename = "demo_pkg-1.0.0-py3-none-any.whl"
kind = "wheel"
digest = "sha256-D256"
size = 123
evidence = "advertised"
[[python.pins]]
id = "SDIST_ID"
registry = "pypi"
canonical_base_fingerprint = "BASE"
project = "dep"
version = "2.0"
filename = "dep-2.0.tar.gz"
kind = "sdist"
digest = "sha512-D512"
size = 456
evidence = "observed-during-fresh-resolution"
[[python.groups]]
config_scope = ""
group = "main"
target = "TARGET"
marker_semantics = "current2026"
root_declarations = ["demo-pkg==1.0.0"]
[python.groups.runtime]
content_tree = "TREE"
recipe = 1
target = "TARGET"
[[python.groups.roots]]
declaration_index = 0
node = "demo-pkg"
[[python.groups.nodes]]
name = "demo-pkg"
version = "1.0.0"
source_pin = "WHEEL_ID"
[[python.groups.nodes]]
name = "dep"
version = "2.0"
source_pin = "SDIST_ID"
[[python.groups.edges]]
from = "demo-pkg"
to = "dep"
declaration = "dep>=2"
"#
    .replace("WHEEL_ID", WHEEL_ID)
    .replace("SDIST_ID", SDIST_ID)
    .replace("BASE", &"11".repeat(32))
    .replace("D256", &"22".repeat(32))
    .replace("D512", &"33".repeat(64))
    .replace("TARGET", &"44".repeat(32))
    .replace("TREE", &"55".repeat(32))
}
fn assert_model(model: &PythonLockModel) {
    assert_eq!(model.pins().len(), 2);
    assert_eq!(model.graphs().len(), 1);
    let graph = &model.graphs()[0];
    assert_eq!(graph.key().config_scope(), "");
    assert_eq!(graph.key().group(), "main");
    assert_eq!(graph.key().target(), &[0x44; 32]);
    assert_eq!(graph.semantics(), LockMarkerSemantics::Current2026);
    assert_eq!(graph.runtime().content_tree(), &[0x55; 32]);
    assert_eq!(graph.runtime().recipe(), 1);
    assert_eq!(graph.runtime().target(), &[0x44; 32]);
    assert_eq!(graph.root_declarations(), ["demo-pkg==1.0.0"]);
    assert_eq!(graph.roots()[0].declaration_index(), 0);
    assert_eq!(graph.roots()[0].node(), "demo-pkg");
    assert_eq!(graph.nodes().len(), 2);
    assert_eq!(graph.edges()[0].from(), "demo-pkg");
    assert_eq!(graph.edges()[0].to(), "dep");
    assert_eq!(graph.edges()[0].declaration(), "dep>=2");
    let pin = model
        .pins()
        .iter()
        .find(|pin| pin.id() == id(WHEEL_ID))
        .unwrap();
    assert_eq!(pin.file().registry_id(), "pypi");
    assert_eq!(pin.file().canonical_base_fingerprint(), &[0x11; 32]);
    assert_eq!(pin.file().project(), "demo-pkg");
    assert_eq!(pin.file().version(), "1.0.0");
    assert_eq!(pin.file().filename(), "demo_pkg-1.0.0-py3-none-any.whl");
    assert_eq!(pin.file().kind(), RegistryArtifactKind::Wheel);
    assert_eq!(pin.digest(), Digest::Sha256([0x22; 32]));
    assert_eq!(pin.size(), 123);
    assert_eq!(pin.evidence(), PinEvidence::Advertised);
    let node = graph
        .nodes()
        .iter()
        .find(|node| node.name() == "demo-pkg")
        .unwrap();
    assert_eq!(node.version(), "1.0.0");
    assert_eq!(node.source_pin(), id(WHEEL_ID));
    assert!(model.graph(graph.key()).is_some());
}

#[test]
fn model_valid_registry_graph_and_shared_pin_preserve_all_readonly_facts() {
    let fixture = Fixture::new();
    assert_model(&fixture.model(LockModelLimits::default()).unwrap());
    let key = GroupKey::new("tools/项目", "开发", [0x44; 32], LockModelLimits::default()).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "dep",
    }];
    let nodes = [fixture.nodes[1]];
    let second = LockGraphInput {
        key: &key,
        semantics: LockMarkerSemantics::Current2026,
        runtime: fixture.runtime,
        root_declarations: &["dep==2.0"],
        roots: &roots,
        nodes: &nodes,
        edges: &[],
    };
    let model = PythonLockModel::from_parts(
        &fixture.pins,
        &[second, fixture.input()],
        LockModelLimits::default(),
    )
    .unwrap();
    assert_eq!(model.pins().len(), 2);
    assert_eq!(model.graphs().len(), 2);
}
#[test]
fn model_empty_data_and_explicit_empty_group_do_not_require_a_fake_resolved_graph() {
    let model = PythonLockModel::from_parts(&[], &[], LockModelLimits::default()).unwrap();
    assert!(model.pins().is_empty() && model.graphs().is_empty());
    let fixture = Fixture::new();
    let graph = LockGraphInput {
        roots: &[],
        nodes: &[],
        edges: &[],
        ..fixture.input()
    };
    let model = PythonLockModel::from_parts(&[], &[graph], LockModelLimits::default()).unwrap();
    assert_eq!(model.graphs().len(), 1);
    assert!(model.graphs()[0].nodes().is_empty());
}
#[test]
fn model_portable_group_key_rejects_unsafe_scope_and_controls_without_local_uuid() {
    let limits = LockModelLimits::default();
    assert_eq!(
        GroupKey::new("tools/项目", "开发 main", [4; 32], limits)
            .unwrap()
            .config_scope(),
        "tools/项目"
    );
    for scope in [
        "/tmp/project",
        ".",
        "..",
        "a/../b",
        "a/./b",
        "a//b",
        "a/",
        "C:/project",
        "a\\b",
        "bad\0scope",
    ] {
        assert_eq!(
            GroupKey::new(scope, "main", [4; 32], limits).unwrap_err(),
            LockModelError::InvalidField(LockField::Scope),
            "{scope:?}"
        );
    }
    assert_eq!(
        GroupKey::new("", "bad\ngroup", [4; 32], limits).unwrap_err(),
        LockModelError::InvalidField(LockField::Group)
    );
}
#[test]
fn model_file_ref_normalizes_identity_and_rejects_unsafe_or_conflicting_filename() {
    let limits = LockModelLimits::default();
    let input = RegistryFileRefInput {
        registry_id: "pypi",
        canonical_base_fingerprint: [0x11; 32],
        project: "Demo.Pkg",
        version: "1.0.0",
        filename: "demo_pkg-1.0.0-py3-none-any.whl",
        kind: RegistryArtifactKind::Wheel,
    };
    assert_eq!(
        RegistryFileRef::new(input, limits).unwrap().project(),
        "demo-pkg"
    );
    for version in ["", ">=1", "1.*", "==1"] {
        assert_eq!(
            RegistryFileRef::new(RegistryFileRefInput { version, ..input }, limits).unwrap_err(),
            LockModelError::InvalidField(LockField::Version)
        );
    }
    for filename in [
        "../demo.whl",
        "/demo.whl",
        "a\\b.whl",
        "demo\0.whl",
        "bad filename.whl",
    ] {
        assert_eq!(
            RegistryFileRef::new(RegistryFileRefInput { filename, ..input }, limits).unwrap_err(),
            LockModelError::InvalidField(LockField::Filename)
        );
    }
    assert_eq!(
        RegistryFileRef::new(
            RegistryFileRefInput {
                filename: "other-1.0.0-py3-none-any.whl",
                ..input
            },
            limits
        )
        .unwrap_err(),
        LockModelError::IdentityMismatch
    );
}

#[test]
fn s3_legacy_source_filename_original_hyphenated_project_pin_is_preserved() {
    let input = RegistryFileRefInput {
        registry_id: "pypi",
        canonical_base_fingerprint: [7; 32],
        project: "antlr4-python3-runtime",
        version: "4.9.3",
        filename: "antlr4-python3-runtime-4.9.3.tar.gz",
        kind: RegistryArtifactKind::Sdist,
    };
    let mut ids = Vec::new();
    for filename in [input.filename, "antlr4_python3_runtime-4.9.3.tar.gz"] {
        let file = RegistryFileRef::new(
            RegistryFileRefInput { filename, ..input },
            Default::default(),
        )
        .unwrap();
        assert_eq!(file.filename(), filename);
        assert_eq!(file.project(), "antlr4-python3-runtime");
        assert_eq!(file.version(), "4.9.3");
        let pin = ContentPin::new(
            &file,
            Digest::Sha256([31; 32]),
            100,
            PinEvidence::Advertised,
            Default::default(),
        )
        .unwrap();
        ids.push(pin.id());
    }
    assert_ne!(
        ids[0], ids[1],
        "actual source basenames must remain bound by their pins"
    );
    for filename in [
        "foreign-4.9.3.tar.gz",
        "antlr4-python3-runtime-4.9.3-1.tar.gz",
        "../antlr4-python3-runtime-4.9.3.tar.gz",
    ] {
        assert!(
            RegistryFileRef::new(
                RegistryFileRefInput { filename, ..input },
                Default::default()
            )
            .is_err(),
            "{filename}"
        );
    }
}

#[test]
fn p6_zip_source_pin_roundtrips_without_changing_existing_tar_pin_identity() {
    let mut fixture = Fixture::new();
    let old = fixture.pins[1].clone();
    assert_eq!(
        ContentPin::new(
            old.file(),
            old.digest(),
            old.size(),
            old.evidence(),
            Default::default()
        )
        .unwrap()
        .id(),
        id(SDIST_ID)
    );
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: old.file().registry_id(),
            canonical_base_fingerprint: *old.file().canonical_base_fingerprint(),
            project: old.file().project(),
            version: old.file().version(),
            filename: "dep-2.0.zip",
            kind: RegistryArtifactKind::Sdist,
        },
        Default::default(),
    )
    .unwrap();
    let zip = ContentPin::new(
        &file,
        old.digest(),
        old.size(),
        old.evidence(),
        Default::default(),
    )
    .unwrap();
    assert_ne!(zip.id(), old.id());
    assert_eq!(old.id(), id(SDIST_ID));
    let zip_id = zip.id();
    fixture.nodes[1].source_pin = zip_id;
    fixture.pins[1] = zip;
    let model = fixture.model(Default::default()).unwrap();
    let text = model.encode_projection(Default::default()).unwrap();
    let decoded = PythonLockModel::decode_projection(&text, Default::default()).unwrap();
    let actual = decoded
        .pins()
        .iter()
        .find(|pin| pin.id() == zip_id)
        .unwrap();
    assert_eq!(actual.file().filename(), "dep-2.0.zip");
    assert_eq!(actual.file().kind(), RegistryArtifactKind::Sdist);
    for filename in [
        "foreign-2.0.zip",
        "../dep-2.0.zip",
        "dep-2.0-1.zip",
        "dep-2.0.tar.xz",
    ] {
        assert!(
            RegistryFileRef::new(
                RegistryFileRefInput {
                    filename,
                    registry_id: "pypi",
                    canonical_base_fingerprint: [0x11; 32],
                    project: "dep",
                    version: "2.0",
                    kind: RegistryArtifactKind::Sdist
                },
                Default::default()
            )
            .is_err(),
            "{filename}"
        );
    }
}
#[test]
fn model_pin_and_canonical_base_hashes_match_independent_versioned_golden() {
    let limits = LockModelLimits::default();
    let file = file();
    let digest = Digest::Sha256([0x22; 32]);
    assert_eq!(
        hex::encode(
            ContentPin::new(&file, digest, 123, PinEvidence::Advertised, limits)
                .unwrap()
                .id()
                .as_bytes()
        ),
        WHEEL_ID
    );
    let original = pin_id(&file, digest);
    for axis in 0..6 {
        let mut changed = file.clone();
        let mut changed_digest = digest;
        match axis {
            0 => changed.registry_id = "another".into(),
            1 => changed.canonical_base_fingerprint = [0x12; 32],
            2 => changed.project = "other".into(),
            3 => changed.version = "1.1".into(),
            4 => changed.filename = "different.whl".into(),
            _ => changed.kind = RegistryArtifactKind::Sdist,
        }
        assert_ne!(pin_id(&changed, changed_digest), original);
        changed_digest = Digest::Sha512([0x22; 64]);
        assert_ne!(pin_id(&file, changed_digest), original);
    }
    let base = CanonicalRegistryUrl::new("https://pypi.org/simple/").unwrap();
    assert_eq!(
        hex::encode(canonical_registry_base_fingerprint(&base)),
        "43f1b31ef218f402ea856a3d84ed9fb0904d25f62700f2c0fb2b425b040626ca"
    );
    assert_eq!(PinId::parse_hex(WHEEL_ID).unwrap(), id(WHEEL_ID));
    assert_eq!(
        PinId::parse_hex("md5-secret").unwrap_err(),
        LockModelError::InvalidField(LockField::PinId)
    );
}
#[test]
fn model_duplicate_or_conflicting_pin_data_is_never_overwritten() {
    let mut fixture = Fixture::new();
    fixture.pins.push(fixture.pins[0].clone());
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Duplicate(LockIdentity::Pin)
    );
    fixture.pins.last_mut().unwrap().size += 1;
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Duplicate(LockIdentity::Pin)
    );
    fixture.pins.pop();
    fixture.pins[0].id = PinId::from_bytes([9; 32]);
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::IdentityMismatch
    );
}
#[test]
fn model_node_alias_duplicates_and_source_pin_identity_conflicts_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.nodes.push(NodeInput {
        name: "Demo.Pkg",
        ..fixture.nodes[0]
    });
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Duplicate(LockIdentity::Node)
    );
    fixture.nodes.pop();
    fixture.nodes[0].source_pin = PinId::from_bytes([9; 32]);
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::NodePin)
    );
    fixture.nodes[0].source_pin = id(SDIST_ID);
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::IdentityMismatch
    );
}
#[test]
fn model_group_runtime_and_semantics_have_no_implicit_host_or_recipe_fallback() {
    let fixture = Fixture::new();
    let graphs = [fixture.input(), fixture.input()];
    assert_eq!(
        PythonLockModel::from_parts(&fixture.pins, &graphs, LockModelLimits::default())
            .unwrap_err(),
        LockModelError::Duplicate(LockIdentity::Group)
    );
    for recipe in [0, 2] {
        assert_eq!(
            RuntimePin::new([5; 32], recipe, [4; 32]).unwrap_err(),
            LockModelError::UnsupportedRuntimeRecipe
        );
    }
    let graph = LockGraphInput {
        runtime: RuntimePin::new([5; 32], 1, [9; 32]).unwrap(),
        ..fixture.input()
    };
    assert_eq!(
        PythonLockModel::from_parts(&fixture.pins, &[graph], LockModelLimits::default())
            .unwrap_err(),
        LockModelError::IdentityMismatch
    );
}
#[test]
fn model_all_root_edge_and_pin_references_are_closed_but_runtime_cycles_are_legal() {
    let mut fixture = Fixture::new();
    fixture.edges.push(EdgeInput {
        from: "dep",
        to: "demo-pkg",
        declaration: "demo-pkg>=1",
    });
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap().graphs()[0]
            .edges()
            .len(),
        2
    );
    fixture.roots[0].node = "absent";
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::Root)
    );
    fixture.roots[0].node = "demo-pkg";
    fixture.edges[0].to = "absent";
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::Edge)
    );
    fixture.edges.clear();
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::UnreachableNode)
    );
    fixture.nodes.pop();
    assert_eq!(
        fixture.model(LockModelLimits::default()).unwrap_err(),
        LockModelError::Reference(LockReference::UnusedPin)
    );
}
#[test]
fn model_raw_budgets_and_debug_never_turn_secret_declarations_into_authority() {
    let fixture = Fixture::new();
    let mut limits = LockModelLimits::default();
    limits.max_pins = 1;
    assert_eq!(
        fixture.model(limits).unwrap_err(),
        LockModelError::Limit(LockLimit::Pins)
    );
    let mut limits = LockModelLimits::default();
    limits.max_total_references = 1;
    assert_eq!(
        fixture.model(limits).unwrap_err(),
        LockModelError::Limit(LockLimit::References)
    );
    let mut limits = LockModelLimits::default();
    limits.max_total_utf8_bytes = 1;
    assert_eq!(
        fixture.model(limits).unwrap_err(),
        LockModelError::Limit(LockLimit::TotalUtf8Bytes)
    );
    let mut direct = Fixture::new();
    direct.root_declarations[0] = "demo-pkg @ https://user:secret-token@example.invalid/pkg.whl";
    let error = direct.model(LockModelLimits::default()).unwrap_err();
    assert_eq!(error, LockModelError::UnsupportedSourceKind);
    assert!(!format!("{error:?} {error}").contains("secret-token"));
    assert!(
        !format!("{:?}", fixture.model(LockModelLimits::default()).unwrap()).contains("demo-pkg==")
    );
}

#[test]
fn model_quoted_marker_uri_and_at_sign_are_preserved_without_becoming_a_source_reference() {
    let mut fixture = Fixture::new();
    let raw =
        "demo-pkg==1.0.0; os_name == 'https://user:literal-token@example.invalid/a?value=@keep'";
    fixture.root_declarations[0] = raw;
    let model = fixture.model(LockModelLimits::default()).unwrap();
    let graph = model
        .graph(&fixture.key)
        .expect("registry graph with opaque marker literal was discarded");
    assert_eq!(graph.root_declarations(), [raw]);
    assert_eq!(graph.roots()[0].node(), "demo-pkg");
    assert!(!format!("{model:?} {graph:?}").contains("literal-token"));
}

#[test]
fn codec_handwritten_toml_oracle_decodes_every_schema_two_field() {
    assert_model(
        &PythonLockModel::decode_projection(&golden(), LockModelLimits::default()).unwrap(),
    );
}
#[test]
fn codec_encoder_uses_stable_maps_and_preserves_unicode_tab_quote_declarations() {
    let mut fixture = Fixture::new();
    fixture.root_declarations[0] = "demo-pkg==1.0.0; os_name == 'a\t\"b'";
    fixture.key = GroupKey::new(
        "tools/项目",
        "开发 group",
        [0x44; 32],
        LockModelLimits::default(),
    )
    .unwrap();
    let model = fixture.model(LockModelLimits::default()).unwrap();
    let encoded = model.encode_projection(LockModelLimits::default()).unwrap();
    assert!(encoded.contains("[python]"));
    assert!(!encoded.contains("project_uuid"));
    assert!(!encoded.contains("prefix"));
    assert_eq!(
        PythonLockModel::decode_projection(&encoded, LockModelLimits::default()).unwrap(),
        model
    );
    fixture.pins.reverse();
    fixture.nodes.reverse();
    assert_eq!(
        fixture
            .model(LockModelLimits::default())
            .unwrap()
            .encode_projection(LockModelLimits::default())
            .unwrap(),
        encoded
    );
}
#[test]
fn codec_closed_fields_versions_and_primitives_are_checked_after_valid_decode() {
    let valid = golden();
    assert_model(&PythonLockModel::decode_projection(&valid, LockModelLimits::default()).unwrap());
    for bad in [
        valid.replacen("schema = 2", "schema = 3", 1),
        valid.replacen("schema = 2", "schema = 2\nunknown = 1", 1),
        valid.replacen("recipe = 1", "recipe = 2", 1),
        valid.replacen("current2026", "legacy", 1),
        valid.replacen("size = 123", "size = -1", 1),
        valid.replacen("size = 123", "size = 1.5", 1),
        valid.replacen("group = \"main\"", "group = \"main\"\ngroup = \"other\"", 1),
    ] {
        assert!(PythonLockModel::decode_projection(&bad, LockModelLimits::default()).is_err());
    }
}
#[test]
fn codec_duplicate_and_dangling_wire_identities_are_not_hidden_by_order() {
    let valid = golden();
    assert_model(&PythonLockModel::decode_projection(&valid, LockModelLimits::default()).unwrap());
    for bad in [
        valid.replace(SDIST_ID, WHEEL_ID),
        valid.replacen("name = \"dep\"", "name = \"Demo.Pkg\"", 1),
        valid.replacen("node = \"demo-pkg\"", "node = \"missing\"", 1),
        valid.replacen("to = \"dep\"", "to = \"missing\"", 1),
        valid.replacen("sha256-", "md5-", 1),
        valid.replacen(WHEEL_ID, &"09".repeat(32), 1),
    ] {
        assert!(PythonLockModel::decode_projection(&bad, LockModelLimits::default()).is_err());
    }
}
#[test]
fn codec_malformed_or_secret_toml_errors_never_echo_input_values() {
    assert_model(
        &PythonLockModel::decode_projection(&golden(), LockModelLimits::default()).unwrap(),
    );
    for raw in [
        "[python]\nschema = \"secret-token",
        "[python]\nschema=2\npins=[{evil=\"secret-token\"}]\ngroups=[]",
    ] {
        let error =
            PythonLockModel::decode_projection(raw, LockModelLimits::default()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret-token"));
    }
}
#[test]
fn codec_raw_count_utf8_and_reference_budgets_are_independently_tightened() {
    let raw = golden();
    assert_model(&PythonLockModel::decode_projection(&raw, LockModelLimits::default()).unwrap());
    let exact = LockModelLimits {
        max_document_bytes: raw.len(),
        ..LockModelLimits::default()
    };
    assert_model(&PythonLockModel::decode_projection(&raw, exact).unwrap());
    for limits in [
        LockModelLimits {
            max_document_bytes: raw.len() - 1,
            ..exact
        },
        LockModelLimits {
            max_pins: 1,
            ..exact
        },
        LockModelLimits {
            max_nodes_per_group: 1,
            ..exact
        },
        LockModelLimits {
            max_total_references: 1,
            ..exact
        },
        LockModelLimits {
            max_total_utf8_bytes: 1,
            ..exact
        },
        LockModelLimits {
            max_declaration_bytes: 1,
            ..exact
        },
    ] {
        assert!(matches!(
            PythonLockModel::decode_projection(&raw, limits),
            Err(LockModelError::Limit(_))
        ));
    }
}
#[test]
fn codec_encoder_revalidates_tighter_model_and_actual_escaped_output_limits() {
    let model = Fixture::new().model(LockModelLimits::default()).unwrap();
    let encoded = model.encode_projection(LockModelLimits::default()).unwrap();
    let exact = LockModelLimits {
        max_document_bytes: encoded.len(),
        ..LockModelLimits::default()
    };
    assert_eq!(model.encode_projection(exact).unwrap(), encoded);
    assert_eq!(
        model
            .encode_projection(LockModelLimits {
                max_document_bytes: encoded.len() - 1,
                ..exact
            })
            .unwrap_err(),
        LockModelError::Limit(LockLimit::DocumentBytes)
    );
    assert_eq!(
        model
            .encode_projection(LockModelLimits {
                max_pins: 1,
                ..exact
            })
            .unwrap_err(),
        LockModelError::Limit(LockLimit::Pins)
    );
}
#[test]
fn codec_python_table_seed_embeds_without_claiming_the_whole_root_lock() {
    struct RootSeed;
    impl<'de> DeserializeSeed<'de> for RootSeed {
        type Value = PythonLockModel;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            struct RootVisitor;
            impl<'de> Visitor<'de> for RootVisitor {
                type Value = PythonLockModel;
                fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                    out.write_str("a python-only table")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                    assert_eq!(map.next_key::<String>()?.as_deref(), Some("python"));
                    let model =
                        map.next_value_seed(PythonLockSeed::new(LockModelLimits::default()))?;
                    assert!(map.next_key::<String>()?.is_none());
                    Ok(model)
                }
            }
            deserializer.deserialize_map(RootVisitor)
        }
    }
    let raw = golden();
    let deserializer = toml::de::Deserializer::parse(&raw).unwrap();
    assert_model(&RootSeed.deserialize(deserializer).unwrap());
}
