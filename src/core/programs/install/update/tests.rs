use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_environment::model::{
    GenerationRecord, InputKind, InputReference, PackageGraphNode, PackageSourcePin, SelectionKey,
};
use crate::packages::python_resolution::lock_model::{
    GroupKey, LockGraphInput, LockMarkerSemantics, NodeInput, PinEvidence, RegistryArtifactKind,
    RegistryFileRef, RegistryFileRefInput, RootInput,
};
use crate::programs::*;
use pakro_store::Digest;
use std::{
    path::Path,
    time::{Duration, Instant},
};

const SOURCE: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.15.0+20261013-aarch64-apple-darwin-install_only.tar.gz";

fn context(root: &Path, write: bool) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(90),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: write,
                project_selection: write,
                lockfile: write,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}

fn observe(project: &Path, context: &OperationContext) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(
        project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap()
}

fn graph(digest: u8) -> (ContentPin, PackageGraphRecord) {
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "pypi",
            canonical_base_fingerprint: [11; 32],
            project: "demo",
            version: "1.0",
            filename: "demo-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        Default::default(),
    )
    .unwrap();
    let digest = Digest::Sha256([digest; 32]);
    let pin = ContentPin::new(
        &file,
        digest,
        123,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap();
    let graph = PackageGraphRecord {
        roots: vec![0],
        nodes: vec![PackageGraphNode {
            name: "demo".into(),
            version: "1.0".into(),
            instance: [13; 32],
            source_pin: hex::encode(pin.id().as_bytes()),
            artifact: digest.to_string(),
            pin: PackageSourcePin {
                registry: "pypi".into(),
                base: [11; 32],
                filename: file.filename().into(),
                size: 123,
                advertised: true,
                build: None,
                conda: None,
            },
        }],
        edges: vec![],
    };
    (pin, graph)
}

fn model(language: [u8; 32], targets: &[[u8; 32]], pin: &ContentPin) -> PythonLockModel {
    model_without_fingerprint(language, targets, pin).with_declaration_fingerprint([1; 32])
}

fn model_without_fingerprint(
    language: [u8; 32],
    targets: &[[u8; 32]],
    pin: &ContentPin,
) -> PythonLockModel {
    let keys = targets
        .iter()
        .map(|target| GroupKey::new("", "shared-runtime", *target, Default::default()).unwrap())
        .collect::<Vec<_>>();
    let nodes = [NodeInput {
        name: "demo",
        version: "1.0",
        source_pin: pin.id(),
    }];
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo",
    }];
    let groups = keys
        .iter()
        .map(|key| LockGraphInput {
            key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: RuntimePin::new(language, 1, *key.target()).unwrap(),
            root_declarations: &["demo==1.0"],
            roots: &roots,
            nodes: &nodes,
            edges: &[],
        })
        .collect::<Vec<_>>();
    PythonLockModel::from_parts(&[pin.clone()], &groups, Default::default()).unwrap()
}

fn native(source: &str, checksum: &str) -> String {
    format!(
        "lockfile_version=4\n[[tools.python]]\nversion='3.15.0+20261013'\nbackend='core:python'\nspecifiers=['3.15.0']\n[tools.python.platforms.macos-arm64]\nurl='{source}'\nchecksum='{checksum}'\n"
    )
}

#[test]
fn p3_update_single_old_graph_survives_a_new_interpreter_without_an_old_payload() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = root.join("moved-project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.16.0'\n[packages.\"python@3.16.0\"]\ndemo='1.0'\n",
    )
    .unwrap();
    let context = context(&root, false);
    let info = crate::lockfile::PlatformInfo {
        url: Some(SOURCE.into()),
        checksum: Some(format!("sha256:{}", "00".repeat(32))),
        ..Default::default()
    };
    let target = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_locked(
        "3.15.0+20261013",
        &target,
        crate::plugins::core::python::native::RuntimeFlavor::InstallOnly,
        &info,
    )
    .unwrap();
    let key = LanguageInstallSpec::from_plan(&plan, false).unwrap().key();
    let (pin, _) = graph(12);
    let old = model(key.fingerprint(), &[[3; 32]], &pin);
    let bytes = format!(
        "{}\n{}",
        native(SOURCE, info.checksum.as_deref().unwrap()),
        old.encode_projection(Default::default()).unwrap()
    );
    let path = project.join("pakro.lock");
    std::fs::write(&path, &bytes).unwrap();
    let observed = observe(&project, &context);
    let entries_before = std::fs::read_dir(&project)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<BTreeSet<_>>();
    let preview_pins = preferred_runtime_pins_from_observation(&context, &observed, &old).unwrap();
    assert_eq!(preview_pins[0].id(), pin.id());
    let entries_after = std::fs::read_dir(&project)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entries_before, entries_after,
        "readonly update preflight must not create a lock guard"
    );
    let (unrelated_pin, _) = graph(19);
    let unrelated = model(key.fingerprint(), &[[3; 32]], &unrelated_pin);
    assert!(preferred_runtime_pins_from_observation(&context, &observed, &unrelated).is_err());
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let preferred = preferred_runtime_pins(&context, &observed, &bound).unwrap();
    assert_eq!(preferred.len(), 1);
    assert_eq!(preferred[0].id(), pin.id());
    assert!(!root.join("data").exists());
    assert!(!root.join("cache").exists());
    assert_eq!(std::fs::read_to_string(path).unwrap(), bytes);
}

#[test]
fn p3_update_ambiguous_or_source_conflicting_old_graphs_are_not_guessed() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let info = crate::lockfile::PlatformInfo {
        url: Some(SOURCE.into()),
        checksum: Some(format!("sha256:{}", "00".repeat(32))),
        ..Default::default()
    };
    let target = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_locked(
        "3.15.0+20261013",
        &target,
        crate::plugins::core::python::native::RuntimeFlavor::InstallOnly,
        &info,
    )
    .unwrap();
    let key = LanguageInstallSpec::from_plan(&plan, false).unwrap().key();
    let (pin, _) = graph(12);
    let cases = [
        model(key.fingerprint(), &[[3; 32], [4; 32]], &pin),
        model([55; 32], &[[3; 32]], &pin),
    ];
    for (index, model) in cases.into_iter().enumerate() {
        let project = root.join(format!("project-{index}"));
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.16.0'\n").unwrap();
        let original = format!(
            "{}\n{}",
            native(SOURCE, info.checksum.as_deref().unwrap()),
            model.encode_projection(Default::default()).unwrap()
        );
        let path = project.join("pakro.lock");
        std::fs::write(&path, &original).unwrap();
        let observed = observe(&project, &context);
        let bound =
            BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
        assert!(preferred_runtime_pins(&context, &observed, &bound).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn p3_update_incomplete_python_results_are_rejected_but_no_model_keeps_first_lock_rules() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, false);
    let (pin, _) = graph(12);
    let cases = [
        format!(
            "lockfile_version=4\n{}",
            model([55; 32], &[[3; 32]], &pin)
                .encode_projection(Default::default())
                .unwrap()
        ),
        format!(
            "lockfile_version=4\n{}",
            model_without_fingerprint([55; 32], &[[3; 32]], &pin)
                .encode_projection(Default::default())
                .unwrap()
        ),
        format!(
            "lockfile_version=4\n{}",
            PythonLockModel::from_parts(&[], &[], Default::default())
                .unwrap()
                .with_declaration_fingerprint([1; 32])
                .encode_projection(Default::default())
                .unwrap()
        ),
    ];
    for (index, original) in cases.into_iter().enumerate() {
        let project = root.join(format!("incomplete-{index}"));
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.16.0'\n").unwrap();
        let path = project.join("pakro.lock");
        std::fs::write(&path, &original).unwrap();
        let observed = observe(&project, &context);
        let bound =
            BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
        assert!(preferred_runtime_pins(&context, &observed, &bound).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
    let project = root.join("first-lock");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.16.0'\n").unwrap();
    let path = project.join("pakro.lock");
    std::fs::write(&path, "lockfile_version=3\n").unwrap();
    let observed = observe(&project, &context);
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    assert!(
        preferred_runtime_pins(&context, &observed, &bound)
            .unwrap()
            .is_empty()
    );
    assert!(!root.join("data").exists());
}

#[tokio::test]
async fn p3_update_registered_old_graph_matches_catalog_and_recorded_target_without_payload() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root, true);
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.16.0'\n[packages.\"python@3.16.0\"]\ndemo='1.0'\n",
    )
    .unwrap();
    let archive = archive("3.15.0");
    let language = prepare(&install_spec(&archive), &context, &archive)
        .await
        .unwrap();
    let source = language.source_pin().unwrap();
    let key = language.key();
    let old_target = *language.target().fingerprint();
    let old_payload = language.root().to_owned();
    let (pin, graph_record) = graph(12);
    let old = model(key.fingerprint(), &[old_target], &pin);
    let native = native(
        source.url.as_deref().unwrap(),
        source.checksum.as_deref().unwrap(),
    );
    let path = project.join("pakro.lock");
    std::fs::write(
        &path,
        format!(
            "{native}\n{}",
            old.encode_projection(Default::default()).unwrap()
        ),
    )
    .unwrap();
    let observed = observe(&project, &context);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let limits = repository.model().limits();
    let record = GenerationRecord::new_shared(
        "g-00000000000000000000000000000001",
        key.target(),
        graph_record.fingerprint().unwrap(),
        [5; 32],
        [7; 32],
        vec![
            InputReference::new(InputKind::Language, Digest::Sha256(key.fingerprint())),
            InputReference::new(
                InputKind::Package,
                Digest::Sha256(graph_record.nodes[0].instance),
            ),
            InputReference::new(
                InputKind::Artifact,
                graph_record.nodes[0].artifact.parse().unwrap(),
            ),
        ],
        limits,
    )
    .unwrap()
    .with_package_graph(graph_record, limits)
    .unwrap();
    let allocation = repository
        .model_mut()
        .register_shared_generation(record)
        .unwrap();
    repository
        .model_mut()
        .register_project(
            identity.as_uuid(),
            project.to_str().unwrap(),
            &"a".repeat(64),
        )
        .unwrap();
    let slot = SelectionKey::new(identity.as_uuid(), DEFAULT_GROUP, key.target(), limits).unwrap();
    let stamp = repository.model().snapshot(&slot);
    repository
        .model_mut()
        .apply_selection(&slot, &stamp, Some(&allocation), uuid::Uuid::now_v7())
        .unwrap();
    repository.commit_model().unwrap();
    drop(repository);
    drop(language);
    std::fs::rename(
        &old_payload,
        root.join("owned-old-payload-removed-from-library"),
    )
    .unwrap();
    {
        let bound =
            BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
        let preferred = preferred_runtime_pins(&context, &observed, &bound).unwrap();
        assert_eq!(preferred[0].id(), pin.id());
    }
    let (changed_pin, _) = graph(19);
    for changed in [
        model(key.fingerprint(), &[old_target], &changed_pin),
        model(key.fingerprint(), &[[99; 32]], &pin),
    ] {
        let original = format!(
            "{native}\n{}",
            changed.encode_projection(Default::default()).unwrap()
        );
        std::fs::write(&path, &original).unwrap();
        let observed = observe(&project, &context);
        let bound =
            BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
        assert!(preferred_runtime_pins(&context, &observed, &bound).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}
