use super::*;
use pakro_index::python_requirements::parse_python_requirement;

const BASIC: &str = "[project]\nname='app'\nversion='1.0'\n";
fn parse(raw: &str) -> ParsedPythonProject {
    parse_pyproject(raw, Default::default()).unwrap()
}
fn selection<'a>(
    project: bool,
    extras: &'a [&'a str],
    groups: &'a [&'a str],
) -> PythonProjectSelection<'a> {
    PythonProjectSelection {
        include_project_dependencies: project,
        extras,
        groups,
    }
}
fn select(
    project: &ParsedPythonProject,
    base: bool,
    extras: &[&str],
    groups: &[&str],
) -> PythonDeclarationSelection {
    project
        .select(selection(base, extras, groups), Default::default())
        .unwrap()
}
fn raws(value: &PythonDeclarationSelection) -> Vec<&str> {
    value
        .known()
        .requirements()
        .iter()
        .map(|requirement| requirement.syntax().raw_for_semantic_resolution())
        .collect()
}
fn requirement_fixture(raw: &str) {
    parse_python_requirement(raw,Default::default()).expect("the existing exact requirement parser must accept this fixture before new declaration behavior");
}
fn safe(error: &PythonDeclarationError) {
    assert!(!format!("{error:?} {error}").contains("PRIVATE_SENTINEL"));
}

#[test]
fn static_project_requirements_keep_exact_ast_markers_extras_uri_and_python_constraint() {
    let requirements = [
        "base[Fast_X]>=1; python_version < '3.14'",
        "direct @ https://private.invalid/file?ticket=PRIVATE_SENTINEL ; os_name == 'posix'",
    ];
    for raw in requirements {
        requirement_fixture(raw);
    }
    let raw = format!(
        "{BASIC}requires-python='>=3.10,<4'\ndependencies=[\"{}\",\"{}\"]\n",
        requirements[0], requirements[1]
    );
    let project = parse(&raw);
    assert_eq!(project.project().unwrap().name().as_str(), "app");
    assert_eq!(project.project().unwrap().version(), Some("1.0"));
    assert_eq!(
        project.project().unwrap().requires_python(),
        Some(">=3.10,<4")
    );
    let result = select(&project, true, &[], &[]);
    assert!(result.metadata_need().is_none());
    assert_eq!(raws(&result), requirements);
    let syntax = result.known().requirements()[1].syntax();
    assert_eq!(
        syntax
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "https://private.invalid/file?ticket=PRIVATE_SENTINEL"
    );
    assert!(syntax.marker().is_some());
    assert!(
        !format!("{project:?} {result:?} {:?}", result.known().requirements())
            .contains("PRIVATE_SENTINEL")
    );
}

#[test]
fn optional_extras_and_groups_are_independent_normalized_selection_namespaces() {
    let project = parse(&format!(
        "{BASIC}dependencies=['base']\n[project.optional-dependencies]\nWeb_UI=['ui-one',\"ui-two; extra == 'Web.UI'\"]\n[dependency-groups]\nweb-ui=['group-one']\n"
    ));
    assert_eq!(
        project.optional_extras().collect::<Vec<_>>(),
        [("Web_UI", "web-ui")]
    );
    assert_eq!(
        project.dependency_groups().collect::<Vec<_>>(),
        [("web-ui", "web-ui")]
    );
    let result = select(&project, true, &["web.UI"], &["WEB_UI"]);
    assert_eq!(
        raws(&result),
        ["base", "ui-one", "ui-two; extra == 'Web.UI'", "group-one"]
    );
    assert_eq!(result.known().extras(), ["web-ui"]);
    assert_eq!(result.known().groups(), ["web-ui"]);
    assert_eq!(
        result.known().requirements()[1].origin(),
        &PythonDeclarationOrigin::Extra {
            name: "web-ui".into(),
            index: 0
        }
    );
    assert!(matches!(
        project.select(selection(false, &["missing"], &[]), Default::default()),
        Err(PythonDeclarationError::MissingSelection(
            PythonDeclarationNamespace::Extra
        ))
    ));
}

#[test]
fn nested_group_includes_expand_in_place_and_keep_every_duplicate_requirement() {
    let project = parse(
        "[dependency-groups]\nBase_Group=['repeat>=1','repeat<2']\ninner=['middle',{include-group='BASE.GROUP'}]\ndev=['first',{include-group='inner'},'last',{include-group='base-group'}]\n",
    );
    let result = select(&project, false, &[], &["DEV"]);
    assert!(result.metadata_need().is_none());
    assert_eq!(
        raws(&result),
        [
            "first",
            "middle",
            "repeat>=1",
            "repeat<2",
            "last",
            "repeat>=1",
            "repeat<2"
        ]
    );
    assert_eq!(
        result.known().requirements()[2].origin(),
        &PythonDeclarationOrigin::Group {
            name: "base-group".into(),
            index: 0
        }
    );
}

#[test]
fn only_selected_group_contents_are_validated_and_missing_includes_or_cycles_fail() {
    let project = parse(
        "[dependency-groups]\ngood=['valid']\nfuture=[{future-source={token='PRIVATE_SENTINEL'}}]\nbadreq=['not a valid requirement ???']\nwrong=42\nmissing=[{include-group='absent'}]\na=[{include-group='B'}]\nb=[{include-group='a'}]\nextra-key=[{include-group='good',extra=true}]\n",
    );
    assert_eq!(raws(&select(&project, false, &[], &["good"])), ["valid"]);
    for (group, expected) in [
        ("future", PythonDeclarationError::InvalidGroupItem),
        ("badreq", PythonDeclarationError::InvalidRequirement),
        ("wrong", PythonDeclarationError::InvalidGroupItem),
        (
            "missing",
            PythonDeclarationError::MissingSelection(PythonDeclarationNamespace::Group),
        ),
        ("a", PythonDeclarationError::GroupCycle),
        ("extra-key", PythonDeclarationError::InvalidGroupItem),
    ] {
        let error = project
            .select(selection(false, &[], &[group]), Default::default())
            .unwrap_err();
        safe(&error);
        assert_eq!(error, expected);
    }
}

#[test]
fn normalized_declaration_collisions_and_repeated_selection_are_not_overwritten() {
    let valid = parse(&format!(
        "{BASIC}[project.optional-dependencies]\ndev=['one']\n[dependency-groups]\ndev=['two']\n"
    ));
    assert_eq!(
        raws(&select(&valid, false, &["dev"], &["dev"])),
        ["one", "two"]
    );
    for (raw, namespace) in [
        (
            "[dependency-groups]\nDev_Test=['a']\ndev-test=['b']\n".to_owned(),
            PythonDeclarationNamespace::Group,
        ),
        (
            format!("{BASIC}[project.optional-dependencies]\n'Web.UI'=['a']\nweb_ui=['b']\n"),
            PythonDeclarationNamespace::Extra,
        ),
    ] {
        assert_eq!(
            parse_pyproject(&raw, Default::default()).unwrap_err(),
            PythonDeclarationError::DuplicateName(namespace)
        );
    }
    assert_eq!(
        valid
            .select(selection(false, &[], &["dev", "DEV"]), Default::default())
            .unwrap_err(),
        PythonDeclarationError::DuplicateSelection
    );
}

#[test]
fn malformed_toml_types_and_required_project_build_fields_have_safe_diagnostics() {
    parse(BASIC);
    for raw in [
        "[project\nname='PRIVATE_SENTINEL'",
        "[project]\nname='app'\nname='PRIVATE_SENTINEL'\n",
    ] {
        let error = parse_pyproject(raw, Default::default()).unwrap_err();
        safe(&error);
        assert_eq!(error, PythonDeclarationError::InvalidToml);
    }
    for (raw, field) in [
        ("[project]\nversion='1.0'", PythonDeclarationField::Name),
        ("[project]\nname='app'", PythonDeclarationField::Version),
        (
            "[project]\nname='app'\nversion=1",
            PythonDeclarationField::Version,
        ),
        (
            "[project]\nname='app'\nversion='not-version'",
            PythonDeclarationField::Version,
        ),
        (
            "[project]\nname='app'\nversion='1'\ndependencies='PRIVATE_SENTINEL'",
            PythonDeclarationField::Dependencies,
        ),
        (
            "[build-system]\nbuild-backend='backend'",
            PythonDeclarationField::BuildRequires,
        ),
        (
            "[build-system]\nrequires='PRIVATE_SENTINEL'",
            PythonDeclarationField::BuildRequires,
        ),
        (
            "[build-system]\nrequires=[]\nconfig-settings={token='PRIVATE_SENTINEL'}",
            PythonDeclarationField::BuildSystem,
        ),
        (
            "[project]\nname='app'\nversion='1'\nrequires-python='not-python'",
            PythonDeclarationField::RequiresPython,
        ),
    ] {
        let error = parse_pyproject(raw, Default::default()).unwrap_err();
        safe(&error);
        assert_eq!(error, PythonDeclarationError::InvalidField(field));
    }
}

#[test]
fn current_dynamic_lists_keep_static_prefixes_and_missing_dynamic_extras_wait_for_worker() {
    let project = parse(
        "[project]\nname='app'\nversion='1'\ndynamic=['dependencies','optional-dependencies']\ndependencies=['static-base']\n[project.optional-dependencies]\nui=['static-ui']\n",
    );
    let selected = select(&project, true, &["ui", "future-extra"], &[]);
    assert_eq!(raws(&selected), ["static-base", "static-ui"]);
    assert_eq!(
        selected.metadata_need().unwrap().reason(),
        PythonMetadataWorkerReason::DynamicMetadata
    );
    assert_eq!(
        selected.metadata_need().unwrap().fields(),
        ["dependencies", "optional-dependencies"]
    );
    for raw in [
        "[project]\nname='app'\nversion='1'\ndynamic=['version']",
        "[project]\nname='app'\nversion='1'\ndynamic=['name']",
    ] {
        assert_eq!(
            parse_pyproject(raw, Default::default()).unwrap_err(),
            PythonDeclarationError::DynamicConflict
        );
    }
    assert_eq!(
        parse_pyproject(
            "[project]\nname='app'\ndynamic=['version','PRIVATE_SENTINEL']",
            Default::default()
        )
        .unwrap_err(),
        PythonDeclarationError::InvalidField(PythonDeclarationField::Dynamic)
    );
}

#[test]
fn missing_project_needs_metadata_only_when_project_dependencies_are_requested() {
    let project = parse("[tool.any]\nvalue='opaque'\n[dependency-groups]\nlint=['ruff']\n");
    assert!(project.project().is_none());
    let groups = select(&project, false, &[], &["lint"]);
    assert_eq!(raws(&groups), ["ruff"]);
    assert!(groups.metadata_need().is_none());
    let included = select(&project, true, &[], &["lint"]);
    assert_eq!(raws(&included), ["ruff"]);
    assert_eq!(
        included.metadata_need().unwrap().reason(),
        PythonMetadataWorkerReason::MissingProject
    );
    let empty = select(&project, true, &[], &[]);
    assert!(empty.known().requirements().is_empty());
    assert!(empty.metadata_need().is_some());
}

#[test]
fn build_system_requires_backend_and_paths_are_separate_unexecuted_data() {
    requirement_fixture("backend-lib>=1");
    let project = parse(
        "[build-system]\nrequires=['backend-lib>=1']\nbuild-backend='local_backend.factory:Builder.make'\nbackend-path=['.','backend dir','${HOME}/backend']\n",
    );
    let build = project.build_system();
    assert_eq!(build.origin(), PythonBuildSystemOrigin::Declared);
    assert_eq!(
        build
            .requires()
            .map(PythonRequirementSyntax::raw_for_semantic_resolution)
            .collect::<Vec<_>>(),
        ["backend-lib>=1"]
    );
    assert_eq!(build.backend().module(), Some("local_backend.factory"));
    assert_eq!(build.backend().object(), Some("Builder.make"));
    assert_eq!(
        build
            .backend_paths()
            .iter()
            .map(PythonBackendPath::raw_for_source_binding)
            .collect::<Vec<_>>(),
        [".", "backend dir", "${HOME}/backend"]
    );
    let default = parse("[tool.other]\nx=true\n");
    assert_eq!(
        default.build_system().origin(),
        PythonBuildSystemOrigin::DefaultAbsentTable
    );
    assert_eq!(
        default
            .build_system()
            .requires()
            .map(PythonRequirementSyntax::raw_for_semantic_resolution)
            .collect::<Vec<_>>(),
        ["setuptools"]
    );
    assert!(matches!(
        default.build_system().backend(),
        PythonBuildBackend::LegacyUnspecified
    ));
    let explicit = parse("[build-system]\nrequires=[]\n");
    assert_eq!(explicit.build_system().requires().count(), 0);
    assert_eq!(
        explicit.build_system().origin(),
        PythonBuildSystemOrigin::Declared
    );
    for (raw, field) in [
        (
            "[build-system]\nrequires=[]\nbuild-backend='invalid..module'",
            PythonDeclarationField::Backend,
        ),
        (
            "[build-system]\nrequires=[]\nbackend-path=['/outside']",
            PythonDeclarationField::BackendPath,
        ),
    ] {
        assert_eq!(
            parse_pyproject(raw, Default::default()).unwrap_err(),
            PythonDeclarationError::InvalidField(field)
        );
    }
}

#[test]
fn frontend_build_config_preserves_scalar_list_shapes_and_merges_duplicates_safely() {
    let config = PythonBuildConfigData::from_entries(
        &[
            ("CC", PythonBuildConfigValue::String("clang")),
            ("option", PythonBuildConfigValue::String("one")),
            (
                "option",
                PythonBuildConfigValue::Strings(&["two", "PRIVATE_SENTINEL"]),
            ),
            ("empty", PythonBuildConfigValue::Strings(&[])),
        ],
        Default::default(),
    )
    .unwrap();
    assert!(matches!(
        config.get("CC"),
        Some(PythonBuildConfigValueRef::String("clang"))
    ));
    match config.get("option").unwrap() {
        PythonBuildConfigValueRef::Strings(values) => {
            assert_eq!(values, ["one", "two", "PRIVATE_SENTINEL"])
        }
        _ => panic!("duplicate values were overwritten"),
    }
    assert!(
        matches!(config.get("empty"),Some(PythonBuildConfigValueRef::Strings(values)) if values.is_empty())
    );
    assert!(!format!("{config:?}").contains("PRIVATE_SENTINEL"));
    let limits = PythonProjectLimits {
        max_config_entries: 1,
        ..Default::default()
    };
    assert_eq!(
        PythonBuildConfigData::from_entries(
            &[
                ("same", PythonBuildConfigValue::String("one")),
                ("same", PythonBuildConfigValue::String("two"))
            ],
            limits
        )
        .unwrap_err(),
        PythonDeclarationError::Limit(PythonDeclarationLimit::ConfigEntries)
    );
}

#[test]
fn raw_structure_collect_and_expansion_budgets_are_independent_and_checked() {
    let valid = parse(BASIC);
    assert!(select(&valid, true, &[], &[]).metadata_need().is_none());
    for (raw, limits, expected) in [
        (
            BASIC,
            PythonProjectLimits {
                max_document_bytes: 1,
                ..Default::default()
            },
            PythonDeclarationLimit::DocumentBytes,
        ),
        (
            "[tool.future]\nvalues=[1,2,3,4,5]\n",
            PythonProjectLimits {
                max_values: 2,
                ..Default::default()
            },
            PythonDeclarationLimit::Values,
        ),
        (
            "[tool.future.deep]\nx=true\n",
            PythonProjectLimits {
                max_depth: 2,
                ..Default::default()
            },
            PythonDeclarationLimit::Depth,
        ),
        (
            "[tool]\nvalue='a large PRIVATE_SENTINEL value'",
            PythonProjectLimits {
                max_field_bytes: 8,
                ..Default::default()
            },
            PythonDeclarationLimit::FieldBytes,
        ),
    ] {
        assert_eq!(
            parse_pyproject(raw, limits).unwrap_err(),
            PythonDeclarationError::Limit(expected)
        );
    }
    let groups = parse(
        "[dependency-groups]\nleaf=['a','b']\ninner=[{include-group='leaf'},{include-group='leaf'}]\nouter=[{include-group='inner'},{include-group='inner'}]\n",
    );
    assert_eq!(raws(&select(&groups, false, &[], &["outer"])).len(), 8);
    for (limits, expected) in [
        (
            PythonExpansionLimits {
                max_expanded_requirements: 7,
                ..Default::default()
            },
            PythonDeclarationLimit::ExpandedRequirements,
        ),
        (
            PythonExpansionLimits {
                max_depth: 1,
                ..Default::default()
            },
            PythonDeclarationLimit::Depth,
        ),
        (
            PythonExpansionLimits {
                max_work: 1,
                ..Default::default()
            },
            PythonDeclarationLimit::ExpansionWork,
        ),
    ] {
        assert_eq!(
            groups
                .select(selection(false, &[], &["outer"]), limits)
                .unwrap_err(),
            PythonDeclarationError::Limit(expected)
        );
    }
    assert_eq!(
        parse_pyproject(
            BASIC,
            PythonProjectLimits {
                max_groups: 0,
                ..Default::default()
            }
        )
        .unwrap_err(),
        PythonDeclarationError::InvalidLimits
    );
}

#[test]
fn raw_uri_backend_paths_self_extras_and_markers_are_not_expanded_or_evaluated() {
    let raw_requirement = "local @ ../${PRIVATE_SENTINEL}/source ; python_version < '1'";
    requirement_fixture(raw_requirement);
    let raw = format!(
        "{BASIC}dependencies=[\"{raw_requirement}\"]\n[project.optional-dependencies]\nall=['app[gui,cli]']\ngui=['gui-lib']\ncli=['cli-lib']\n[build-system]\nrequires=[]\nbackend-path=['../unresolved/${{PRIVATE_SENTINEL}}']\n"
    );
    let project = parse(&raw);
    let selected = select(&project, true, &["all"], &[]);
    assert_eq!(raws(&selected), [raw_requirement, "app[gui,cli]"]);
    assert_eq!(
        selected.known().requirements()[0]
            .syntax()
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "../${PRIVATE_SENTINEL}/source"
    );
    assert_eq!(
        project.build_system().backend_paths()[0].raw_for_source_binding(),
        "../unresolved/${PRIVATE_SENTINEL}"
    );
    assert!(!format!("{project:?} {selected:?}").contains("PRIVATE_SENTINEL"));
}
