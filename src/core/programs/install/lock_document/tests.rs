use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::*;
use std::time::{Duration, Instant};

#[test]
fn p6_source_runtime_record_keeps_its_nested_tables_inside_the_unified_lock() {
    let (_, record, pin) = crate::libraries::language::lease::pin_tests::source_facts();
    let mut document: toml_edit::DocumentMut = "lockfile_version = 4\n[python]\nschema = 3\n[python.future]\nvalue = 'retained'\n"
        .parse()
        .unwrap();
    let mut platform = toml_edit::Table::new();
    platform.insert("install", toml_edit::value("source"));
    platform.insert(
        "source_python",
        toml_edit::Item::Table(source_python_table(&record).unwrap()),
    );
    platform.insert("url", toml_edit::value(pin.url.clone().unwrap()));
    platform.insert("checksum", toml_edit::value(pin.checksum.clone().unwrap()));
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let mut platforms = toml_edit::Table::new();
    platforms.insert(&target, toml_edit::Item::Table(platform));
    let mut entry = toml_edit::Table::new();
    entry.insert("version", toml_edit::value(record.version.clone()));
    entry.insert("backend", toml_edit::value("core:python"));
    entry.insert("platforms", toml_edit::Item::Table(platforms));
    let mut entries = toml_edit::ArrayOfTables::new();
    entries.push(entry);
    let mut tools = toml_edit::Table::new();
    tools.insert("python", toml_edit::Item::ArrayOfTables(entries));
    document.insert("tools", toml_edit::Item::Table(tools));
    let bytes = document.to_string().into_bytes();
    let parsed = crate::lockfile::Lockfile::parse_observed(&bytes).unwrap();
    assert_eq!(parsed.tools()["python"][0].platforms[&target], pin);
    let value: toml::Value = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(value["python"]["future"]["value"].as_str(), Some("retained"));
}

#[test]
fn p6_runtime_overlay_replaces_source_results_and_clears_them_for_prebuilt() {
    use crate::libraries::language::source::SourcePythonBuildInput;
    let (_, record, source) = crate::libraries::language::lease::pin_tests::source_facts();
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let prebuilt = crate::lockfile::PlatformInfo {
        url: Some(spec.plan().source().url().to_string()),
        checksum: Some(spec.plan().source().digest().to_string().replacen('-', ":", 1)),
        ..Default::default()
    };
    let entry = |pin: &crate::lockfile::PlatformInfo, source: bool| {
        let entry = crate::lockfile::LockfileTool {
            version: "3.15.0".into(), backend: Some("core:python".into()),
            specifiers: Default::default(),
            options: if source { std::collections::BTreeMap::from([("source".into(), "source".into())]) } else { Default::default() },
            platforms: std::collections::BTreeMap::from([(target.clone(), pin.clone())]),
        };
        toml::to_string(&entry).unwrap().parse::<toml_edit::DocumentMut>().unwrap().into_table()
    };
    let decode = |current: &toml_edit::Table| {
        // Table::Display prints its value body only; the production writer
        // serializes a DocumentMut, which also visits nested platform tables.
        let output = toml_edit::DocumentMut::from(current.clone()).to_string();
        let selected: crate::lockfile::LockfileTool = toml::from_str(&output).unwrap();
        eprintln!("P6_SOURCE_OVERLAY_TABLE_BODY\n{current}");
        eprintln!("P6_SOURCE_OVERLAY_DOCUMENT\n{output}");
        eprintln!("P6_SOURCE_OVERLAY_PLATFORM_KEYS={:?}", selected.platforms.keys().collect::<Vec<_>>());
        selected
    };
    let mut current = entry(&prebuilt, false);
    let mut options = toml_edit::InlineTable::new();
    options.insert("source", "prebuilt".into());
    current.insert("options", toml_edit::Item::Value(toml_edit::Value::InlineTable(options)));
    current.insert("future_runtime", toml_edit::value("retained runtime"));
    let platform = current["platforms"][&target].as_table_mut().unwrap();
    platform.insert("future_target", toml_edit::value("retained target"));
    platform["url"].as_value_mut().unwrap().decor_mut().set_suffix(" # retained URL comment");
    overlay_runtime_source(&mut current, &mut entry(&source, true), &target, true).unwrap();
    let selected = decode(&current);
    assert_eq!(selected.options.get("source").map(String::as_str), Some("source"));
    assert_eq!(selected.platforms[&target], source);

    // Same release/archive, but a different compiler and actual result: URL
    // and checksum alone must not leave the original source result in place.
    let original = record.input().unwrap();
    let mut toolchain = original.toolchain;
    toolchain.programs.iter_mut().find(|program| program.role == "cc").unwrap().sha256[0] ^= 1;
    let changed = SourcePythonBuildInput::new(&record.version, original.source, original.target, toolchain, None)
        .unwrap().record([74; 32], record.runtime_target).unwrap();
    assert_ne!(changed.request_id, record.request_id);
    let mut new_source = source.clone(); new_source.source_python = Some(changed.clone());
    overlay_runtime_source(&mut current, &mut entry(&new_source, true), &target, true).unwrap();
    let selected = decode(&current);
    assert_eq!(selected.platforms[&target].source_python.as_ref(), Some(&changed));
    assert_eq!(selected.platforms[&target].url, source.url);
    assert_eq!(selected.platforms[&target].checksum, source.checksum);

    overlay_runtime_source(&mut current, &mut entry(&prebuilt, false), &target, true).unwrap();
    let selected = decode(&current);
    assert!(selected.options.is_empty());
    assert!(current.get("options").is_none());
    assert_eq!(selected.platforms[&target], prebuilt);
    let output = toml_edit::DocumentMut::from(current).to_string();
    for retained in ["retained runtime", "retained target", "# retained URL comment"] {
        assert!(output.contains(retained), "runtime source switching lost {retained}");
    }
    assert!(!output.contains("source_python") && !output.contains("install ="));
}

#[tokio::test]
async fn p2_runtime_overlay_preserves_native_comments_unknown_fields_and_provenance() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 64,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: true,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let archive = archive("3.15.0");
    let spec = install_spec(&archive);
    let language = prepare(&spec, &context, &archive).await.unwrap();
    let inventories_before_mapping = crate::libraries::language::lease::full_inventory_checks();
    let source = language.source_pin().unwrap();
    assert_eq!(language.lock_version().unwrap(), "3.15.0+20261013");
    crate::programs::install::lock::group_key(&language).unwrap();
    crate::programs::install::lock::runtime_pin(&language).unwrap();
    assert_eq!(
        crate::libraries::language::lease::full_inventory_checks(),
        inventories_before_mapping,
        "portable lock descriptors must not repeat a complete runtime hash walk"
    );
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let original = format!(
        "# native header\nlockfile_version=4 # version comment\n\n[[tools.python]]\nversion='3.15.0' # runtime comment\nbackend='core:python' # backend comment\nspecifiers=['3.15.0'] # specifier comment\nfuture_runtime='runtime stays'\n\n[tools.python.platforms.{target}]\nurl='{}' # URL comment\nchecksum='{}' # checksum comment\nprovenance='github-attestations'\nfuture_target='target stays'\n\n[future]\nwhen=2026-10-25T00:00:00Z # future comment\n",
        source.url.unwrap(),
        source.checksum.unwrap(),
    );
    let path = project.join("pakro.lock");
    std::fs::write(&path, &original).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let model = PythonLockModel::from_parts(&[], &[], Default::default()).unwrap();
    let bytes = prepare_document(&bound, &observed, &language, model).unwrap();
    assert_eq!(
        crate::libraries::language::lease::full_inventory_checks(),
        inventories_before_mapping,
        "preparing document data must not re-run the surrounding lease boundary checks"
    );
    let output = std::str::from_utf8(&bytes).unwrap();
    let parsed = crate::lockfile::Lockfile::parse_observed(&bytes).unwrap();
    assert_eq!(parsed.tools()["python"][0].version, "3.15.0+20261013");
    assert!(output.contains("# runtime comment"));
    for retained in [
        "# native header",
        "# version comment",
        "# backend comment",
        "# specifier comment",
        "# URL comment",
        "# checksum comment",
        "provenance='github-attestations'",
        "future_runtime='runtime stays'",
        "future_target='target stays'",
        "when=2026-10-25T00:00:00Z # future comment",
    ] {
        assert!(
            output.contains(retained),
            "missing retained field/comment {retained}"
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let model =
        crate::packages::python_resolution::lock_session::decode_root(&bytes, Default::default())
            .unwrap()
            .unwrap();
    validate_declarations(&observed, &model).unwrap();
}

#[tokio::test]
async fn p3_update_overlay_replaces_old_python_results_and_retains_foreign_native_data() {
    use crate::packages::python_environment::model::PackageGraphRecord;
    use crate::packages::python_resolution::lock_model::{
        GroupKey, LockGraphInput, LockMarkerSemantics, RuntimePin,
    };
    use crate::programs::shared::python_roots::PythonLockIntent;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(60),
    );
    let archive = archive("3.15.0");
    let language = prepare(&install_spec(&archive), &context, &archive)
        .await
        .unwrap();
    let source = language.source_pin().unwrap();
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let stale_platform = if target == "linux-x64" {
        "macos-arm64"
    } else {
        "linux-x64"
    };
    let foreign = "\n[[tools.node]]\nversion='22.0.0' # other ecosystem comment\nbackend='core:node'\nspecifiers=['22.0.0']\nfuture_node='retained'\n\n[future]\nvalue='retained' # future comment\n";
    let native = format!(
        "# unified header\nlockfile_version=4 # revision comment\n\n[[tools.python]]\nversion='3.14.0+20261013' # obsolete Python result\nbackend='core:python'\nspecifiers=['3.14.0']\n\n[[tools.python]]\nversion='3.15.0+20261013' # current runtime comment\nbackend='core:python' # backend remains\nspecifiers=['3.14.0','3.15.0'] # declarations comment\nfuture_runtime='retained runtime field'\n\n[tools.python.platforms.{target}]\nurl='{}' # source URL comment\nchecksum='{}' # source checksum comment\nprovenance='github-attestations'\nfuture_target='retained target field'\n\n[tools.python.platforms.{stale_platform}]\nurl='https://example.invalid/old-other-platform'\nchecksum='sha256-{}'\n{foreign}",
        source.url.unwrap(),
        source.checksum.unwrap(),
        "03".repeat(32)
    );
    let current_key = crate::programs::install::lock::group_key(&language).unwrap();
    let stale_key = GroupKey::new("", "shared-runtime", [99; 32], Default::default()).unwrap();
    let old = PythonLockModel::from_parts(
        &[],
        &[
            LockGraphInput {
                key: &current_key,
                semantics: LockMarkerSemantics::Current2026,
                runtime: crate::programs::install::lock::runtime_pin(&language).unwrap(),
                root_declarations: &[],
                roots: &[],
                nodes: &[],
                edges: &[],
            },
            LockGraphInput {
                key: &stale_key,
                semantics: LockMarkerSemantics::Current2026,
                runtime: RuntimePin::new([88; 32], 1, [99; 32]).unwrap(),
                root_declarations: &[],
                roots: &[],
                nodes: &[],
                edges: &[],
            },
        ],
        Default::default(),
    )
    .unwrap()
    .with_declaration_fingerprint([1; 32]);
    let python = old.encode_projection(Default::default()).unwrap().replacen(
        "schema = 3",
        "schema = 3 # Python schema comment",
        1,
    );
    let original = format!("{native}\n{python}");
    let path = project.join("pakro.lock");
    std::fs::write(&path, &original).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let model = crate::programs::install::lock::project_lock_model_roots(
        &bound,
        &language,
        &PackageGraphRecord {
            roots: vec![],
            nodes: vec![],
            edges: vec![],
        },
        &[],
        &[],
        PythonLockIntent::Update,
    )
    .unwrap();
    let bytes = prepare_document_for_update(&bound, &observed, &language, model).unwrap();
    let output = std::str::from_utf8(&bytes).unwrap();
    let parsed = crate::lockfile::Lockfile::parse_observed(&bytes).unwrap();
    assert_eq!(parsed.tools()["python"].len(), 1);
    assert_eq!(parsed.tools()["python"][0].version, "3.15.0+20261013");
    assert_eq!(
        parsed.tools()["python"][0].specifiers,
        std::collections::BTreeSet::from(["3.15.0".to_owned()])
    );
    assert_eq!(parsed.tools()["python"][0].platforms.len(), 1);
    assert_eq!(parsed.tools()["node"][0].version, "22.0.0");
    assert!(output.contains(foreign));
    for expected in [
        "# unified header",
        "# revision comment",
        "# current runtime comment",
        "# backend remains",
        "# declarations comment",
        "# source URL comment",
        "# source checksum comment",
        "future_runtime='retained runtime field'",
        "future_target='retained target field'",
        "# Python schema comment",
    ] {
        assert!(
            output.contains(expected),
            "lost retained syntax: {expected}"
        );
    }
    assert!(!output.contains("# obsolete Python result"));
    assert!(!output.contains("https://example.invalid/old-other-platform"));
    let model =
        crate::packages::python_resolution::lock_session::decode_root(&bytes, Default::default())
            .unwrap()
            .unwrap();
    assert_eq!(model.graphs().len(), 1);
    assert_eq!(model.graphs()[0].key(), &current_key);
    validate_declarations(&observed, &model).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);

    let invalid_project = root.join("invalid-native-project");
    std::fs::create_dir(&invalid_project).unwrap();
    std::fs::write(
        invalid_project.join("pakro.toml"),
        "[tools]\npython='3.15.0'\n",
    )
    .unwrap();
    let invalid = format!("lockfile_version=4\n[tools]\npython=17\n\n{python}");
    let invalid_path = invalid_project.join("pakro.lock");
    std::fs::write(&invalid_path, &invalid).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root];
    let invalid_observed = observe_project(
        &invalid_project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let invalid_bound =
        BoundPythonLock::acquire(&invalid_path, Default::default(), context.policy().deadline)
            .unwrap();
    assert!(
        prepare_document_for_update(&invalid_bound, &invalid_observed, &language, model).is_err()
    );
    assert_eq!(std::fs::read_to_string(invalid_path).unwrap(), invalid);
}
