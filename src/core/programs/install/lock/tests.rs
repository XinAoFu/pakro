use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::packages::python_resolution::lock_model::{
    RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
};
use pakro_store::Digest;
use std::time::Duration;

fn graph() -> PackageGraphRecord {
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
    let digest = Digest::Sha256([12; 32]);
    let pin = ContentPin::new(
        &file,
        digest,
        123,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap();
    PackageGraphRecord {
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
    }
}

fn empty() -> PackageGraphRecord {
    PackageGraphRecord {
        roots: vec![],
        nodes: vec![],
        edges: vec![],
    }
}

#[tokio::test]
async fn p3_lock_preserves_inactive_declarations_and_original_active_indices() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(60));
    let archive = archive("3.15.0");
    let language = prepare(&install_spec(&archive), &context, &archive)
        .await
        .unwrap();
    let bound = BoundPythonLock::acquire(
        &stage.join("pakro.lock"),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    let inactive = PythonRoot {
        name: "inactive".into(),
        version: "1.0".into(),
        declaration: "inactive[feature]==1.0; python_version < '1'".into(),
    };
    let active = PythonRoot {
        name: "demo".into(),
        version: "1.0".into(),
        declaration: "demo[feature]==1.0; python_version >= '3'".into(),
    };
    let roots = vec![inactive.clone(), active.clone()];
    let model = project_lock_model_roots(
        &bound,
        &language,
        &graph(),
        &roots,
        &[],
        PythonLockIntent::Update,
    )
    .unwrap();
    let locked = validate_project_lock_model_roots(&model, &language, &roots).unwrap();
    assert_eq!(
        locked.root_declarations(),
        crate::programs::shared::python_roots::declarations(&roots)
    );
    assert_eq!(locked.roots().len(), 1);
    assert_eq!(locked.roots()[0].declaration_index(), 1);
    assert_eq!(locked.roots()[0].node(), "demo");
    assert_eq!(model.pins().len(), 1);
    assert!(validate_project_lock_model_roots(&model, &language, &[active.clone()]).is_err());
    let mut changed = roots.clone();
    changed[0].declaration = "inactive[feature]==1.0; python_version < '2'".into();
    assert!(validate_project_lock_model_roots(&model, &language, &changed).is_err());
    for roots in [vec![inactive], vec![]] {
        let model = project_lock_model_roots(
            &bound,
            &language,
            &empty(),
            &roots,
            &[],
            PythonLockIntent::Update,
        )
        .unwrap();
        let locked = validate_project_lock_model_roots(&model, &language, &roots).unwrap();
        assert!(
            locked.roots().is_empty() && locked.nodes().is_empty() && locked.edges().is_empty()
        );
        assert_eq!(
            locked.root_declarations(),
            crate::programs::shared::python_roots::declarations(&roots)
        );
        assert!(model.pins().is_empty() && model.builds().is_empty());
    }
    assert!(
        project_lock_model_roots(
            &bound,
            &language,
            &empty(),
            &[active],
            &[],
            PythonLockIntent::Update
        )
        .is_err()
    );
}

#[tokio::test]
async fn p3_update_discards_other_target_graphs_and_unused_pins_while_preserve_keeps_them() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(60));
    let archive = archive("3.15.0");
    let language = prepare(&install_spec(&archive), &context, &archive)
        .await
        .unwrap();
    let path = stage.join("pakro.lock");
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let initial = project_lock_model_roots(
        &bound,
        &language,
        &graph(),
        &[PythonRoot::plain("demo", "1.0").unwrap()],
        &[],
        PythonLockIntent::Update,
    )
    .unwrap();
    let old_key = GroupKey::new("", "shared-runtime", [99; 32], Default::default()).unwrap();
    let old = GraphView::new(&initial.graphs()[0]);
    let old_runtime = RuntimePin::new([88; 32], 1, [99; 32]).unwrap();
    let previous = PythonLockModel::from_parts(
        initial.pins(),
        &[
            old.input(),
            LockGraphInput {
                key: &old_key,
                semantics: LockMarkerSemantics::Current2026,
                runtime: old_runtime,
                root_declarations: &[],
                roots: &[],
                nodes: &[],
                edges: &[],
            },
        ],
        Default::default(),
    )
    .unwrap();
    drop(bound);
    let original = format!(
        "lockfile_version=4\n{}",
        previous.encode_projection(Default::default()).unwrap()
    );
    std::fs::write(&path, &original).unwrap();
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let preserved = project_lock_model_roots(
        &bound,
        &language,
        &empty(),
        &[],
        &[],
        PythonLockIntent::Preserve,
    )
    .unwrap();
    assert_eq!(preserved.graphs().len(), 2);
    let updated = project_lock_model_roots(
        &bound,
        &language,
        &empty(),
        &[],
        &[],
        PythonLockIntent::Update,
    )
    .unwrap();
    assert_eq!(updated.graphs().len(), 1);
    assert_eq!(updated.graphs()[0].key(), &group_key(&language).unwrap());
    assert!(updated.pins().is_empty() && updated.builds().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}
