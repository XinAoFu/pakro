use super::*;
use crate::libraries::language::repository::tests::operation_context;
use crate::libraries::package::layouts::tests::wheel;
use crate::libraries::package::tree::prepare_package_tree;
use std::time::Duration;

// These cases exercise real locks, manifests and wheel allocations. They do
// not execute the interpreter; the small protocol fixture avoids repeatedly
// extracting a full distribution already covered by F3 import/CLI tests.
async fn removal_language(
    context: &OperationContext,
) -> crate::libraries::language::lease::LanguageLease {
    use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
    let bytes = archive("3.15.0");
    prepare(&install_spec(&bytes), context, &bytes)
        .await
        .unwrap()
}

#[tokio::test]
async fn f4_references_report_chain_and_block() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(300));
    let language = removal_language(&context).await;
    let (facts, archive) = wheel(
        &context,
        &language,
        "dep",
        [41; 32],
        vec![],
        vec![("dep.py", b"VALUE=1\n")],
        None,
    );
    let dependency = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let dependency_manifest = dependency.manifest().clone();
    let (facts, archive) = wheel(
        &context,
        &language,
        "parent",
        [42; 32],
        vec![[41; 32]],
        vec![("parent.py", b"import dep\n")],
        None,
    );
    let parent = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let parent_manifest = parent.manifest().clone();
    let parent_path = parent.root().to_owned();
    let (facts, archive) = wheel(
        &context,
        &language,
        "unrelated",
        [43; 32],
        vec![],
        vec![("unrelated.py", b"VALUE=2\n")],
        None,
    );
    let unrelated = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let unrelated_path = unrelated.root().to_owned();
    drop(parent);
    drop(dependency);
    let error = remove(&context, &dependency_manifest)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("parent==1.0") && error.contains("dep==1.0"),
        "{error}"
    );
    remove(&context, &parent_manifest).unwrap();
    assert!(!parent_path.exists());
    remove(&context, &dependency_manifest).unwrap();
    unrelated.check_binding().unwrap();
    assert!(unrelated_path.join("site-packages/unrelated.py").is_file());
    assert!(
        manifest::store_path(&context)
            .join(".leases/build")
            .join(format!("{}.lock", dependency_manifest.storage.allocation))
            .is_file()
    );
}

#[tokio::test]
async fn f4_package_live_lease_and_unknown_block_removal() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(300));
    let language = removal_language(&context).await;
    let (facts, archive) = wheel(
        &context,
        &language,
        "demo",
        [44; 32],
        vec![],
        vec![("demo.py", b"VALUE=3\n")],
        None,
    );
    let lease = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let target = lease.manifest().clone();
    let path = lease.root().to_owned();
    assert!(
        remove(&context, &target)
            .unwrap_err()
            .to_string()
            .contains("in use")
    );
    lease.check_binding().unwrap();
    drop(lease);
    let catalog = manifest::store_path(&context).join(".control/catalog");
    std::fs::write(
        catalog.join(manifest::package_name(&target.key())),
        b"{broken",
    )
    .unwrap();
    assert!(remove(&context, &target).is_err());
    assert!(path.join("site-packages/demo.py").is_file());
    assert!(manifest::enumerate_published(&context).is_err());
}

#[tokio::test]
async fn f4_unselected_adapter_is_invalidated_without_expanding_deletion() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(300));
    let language = removal_language(&context).await;
    let source = wheel(
        &context,
        &language,
        "unused",
        [45; 32],
        vec![],
        vec![("unused.py", b"VALUE=4\n")],
        None,
    );
    let layout = crate::libraries::package::layouts::prepare_layout(
        &context,
        &language,
        [46; 32],
        vec![source],
    )
    .unwrap();
    let key = layout.key();
    let adapter_path = layout.prefix().to_owned();
    let target = layout.packages()[0].manifest().clone();
    let package_path = layout.packages()[0].root().to_owned();
    drop(layout);
    drop(language);
    remove(&context, &target).unwrap();
    assert!(!package_path.exists());
    assert!(
        adapter_path.is_dir(),
        "only the explicitly targeted package payload is deleted"
    );
    assert!(
        crate::libraries::package::lease::acquire_layout(&context, key)
            .unwrap()
            .is_none(),
        "derived adapter must no longer claim Ready"
    );
    assert!(
        manifest::store_path(&context)
            .join(".leases/build")
            .join(format!(
                "{}.lock",
                adapter_path.file_name().unwrap().to_str().unwrap()
            ))
            .is_file()
    );
}
