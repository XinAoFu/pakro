// Included by layouts/tests.rs after the publication IO RED is captured.

#[tokio::test]
async fn f3_valid_json_package_facts_must_match_the_published_tree() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(120),
    );
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let (facts, archive) = wheel(
        &context,
        &language,
        "factdep",
        [61; 32],
        vec![],
        vec![("factdep.py", b"VALUE=1\n")],
        None,
    );
    let dep =
        super::super::tree::prepare_package_tree(&context, &language, facts, archive).unwrap();
    let (facts, archive) = wheel(
        &context,
        &language,
        "factparent",
        [62; 32],
        vec![[61; 32]],
        vec![("factparent.py", b"import factdep\n")],
        None,
    );
    let parent =
        super::super::tree::prepare_package_tree(&context, &language, facts, archive).unwrap();
    let original = parent.manifest().clone();
    drop(parent);
    let path = super::super::manifest::store_path(&context)
        .join(".control/catalog")
        .join(super::super::manifest::package_name(&original.key()));
    let mut changed = original.clone();
    changed.facts.name = "plausible-other-name".into();
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        super::super::lease::acquire_package(&context, original.key()).is_err(),
        "valid JSON must not replace the tree's exact package name"
    );
    assert!(super::super::manifest::enumerate_published(&context).is_err());
    let mut changed = original.clone();
    changed.facts.dependencies.clear();
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        super::super::lease::acquire_package(&context, original.key()).is_err(),
        "valid JSON must not erase actual package dependency protection"
    );
    assert!(super::super::manifest::enumerate_published(&context).is_err());
    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    super::super::lease::acquire_package(&context, original.key())
        .unwrap()
        .unwrap()
        .check_binding()
        .unwrap();
    dep.check_binding().unwrap();
}

#[tokio::test]
async fn f3_wheel_payload_cannot_occupy_the_reserved_package_facts_file() {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(120),
    );
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let (facts, archive) = wheel(
        &context,
        &language,
        "factsclash",
        [63; 32],
        vec![],
        vec![(
            "factsclash-1.0.data/data/.pakro-package-facts.json",
            b"forged\n",
        )],
        None,
    );
    let key = facts.key;
    let result = super::super::tree::prepare_package_tree(&context, &language, facts, archive);
    let error = match result {
        Ok(_) => panic!("wheel payload occupied its publication facts slot"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("reserved"),
        "the complete original wheel plan must reject this reserved destination before writes: {error:#}"
    );
    assert!(
        super::super::manifest::read_package(&context, &key)
            .unwrap()
            .is_none()
    );
}
