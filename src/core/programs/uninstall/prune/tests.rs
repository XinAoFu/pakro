use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::{layouts::tests::wheel, tree::prepare_package_tree};
use crate::libraries::project::repository::ProjectRepository;
use crate::packages::python_environment::model::{
    GenerationRecord, InputKind, InputReference, SelectionKey,
};
use crate::programs::uninstall::reachability::{GroupReferenced, recheck_group};
use pakro_store::control::{
    ControlLimits, ControlNamespace, StoreWriter,
    lease::{LeaseMode, StableLock},
};
use std::time::Duration;

fn context(root: &std::path::Path) -> OperationContext {
    let initial = operation_context(root, true, Duration::from_secs(120));
    let mut policy = *initial.policy();
    policy.writes.project_selection = true;
    crate::programs::capture_operation_context(crate::programs::OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}
async fn language(context: &OperationContext) -> crate::libraries::language::lease::LanguageLease {
    let bytes = archive("3.15.0");
    prepare(&install_spec(&bytes), context, &bytes)
        .await
        .unwrap()
}
fn register_layout(
    context: &OperationContext,
    layout: &crate::libraries::package::lease::LayoutLease,
    group: &str,
) -> (SelectionKey, Uuid, String) {
    let mut repository = ProjectRepository::open_writer(context).unwrap();
    let project = Uuid::from_u128(41);
    repository
        .model_mut()
        .register_project(project, "/removed-fixture-project", &"a".repeat(64))
        .unwrap();
    let allocation = layout
        .prefix()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut inputs = vec![InputReference::new(
        InputKind::Language,
        pakro_store::Digest::Sha256(layout.language()),
    )];
    inputs.extend(layout.packages().iter().map(|package| {
        InputReference::new(
            InputKind::Package,
            pakro_store::Digest::Sha256(package.manifest().key()),
        )
    }));
    let record = GenerationRecord::new_shared(
        &allocation,
        layout.language_lease().key().target(),
        layout.graph(),
        [91; 32],
        layout.key(),
        inputs,
        repository.model().limits(),
    )
    .unwrap();
    repository
        .model_mut()
        .register_shared_generation(record)
        .unwrap();
    let key = SelectionKey::new(
        project,
        group,
        layout.language_lease().key().target(),
        repository.model().limits(),
    )
    .unwrap();
    let expected = repository.model().snapshot(&key);
    let transaction = Uuid::new_v4();
    repository
        .model_mut()
        .apply_selection(&key, &expected, Some(&allocation), transaction)
        .unwrap();
    repository.commit_model().unwrap();
    (key, transaction, allocation)
}

#[test]
fn p4_preview_empty_does_not_initialize_and_damaged_authority_is_not_zero() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let preview = run(&context, true, None).unwrap();
    assert!(preview.complete && preview.entries.is_empty());
    assert!(!context.requested_data_root().exists());
    assert!(!context.requested_cache_root().exists());
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    writer
        .control(ControlLimits::default())
        .unwrap()
        .replace(
            ControlNamespace::Journals,
            &format!("p2-lock-{}", Uuid::from_u128(1).simple()),
            None,
            b"damaged",
        )
        .unwrap();
    drop((writer, repository));
    let report = run(&context, false, None).unwrap();
    assert!(!report.complete);
    assert!(report.operation_id.is_none());
    assert!(report.entries.iter().any(|entry| {
        entry.status == ReclaimStatus::Unknown
            && entry
                .reasons
                .iter()
                .any(|reason| reason.contains("journal"))
    }));
}

#[test]
fn p4_every_group_and_target_is_a_root_but_tombstones_and_retired_records_are_not() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let project = Uuid::from_u128(42);
    repository
        .model_mut()
        .register_project(project, "/missing-catalog-only-fixture", &"b".repeat(64))
        .unwrap();
    let mut keys = Vec::new();
    for (index, group) in ["auxiliary", "auxiliary", "default"]
        .into_iter()
        .enumerate()
    {
        let key = SelectionKey::new(
            project,
            group,
            [index as u8 + 1; 32],
            repository.model().limits(),
        )
        .unwrap();
        let expected = repository.model().snapshot(&key);
        repository
            .model_mut()
            .apply_language_selection(
                &key,
                &expected,
                &pakro_store::Digest::Sha256([index as u8 + 11; 32]).to_string(),
                Uuid::new_v4(),
            )
            .unwrap();
        keys.push(key);
    }
    // Retired metadata is intentionally descriptive: it is never admitted as
    // a real allocation or a deletion permit by this projection test.
    let retired = GenerationRecord::new_shared(
        "g-00000000000000000000000000000042",
        [4; 32],
        [5; 32],
        [6; 32],
        [7; 32],
        vec![InputReference::new(
            InputKind::Language,
            pakro_store::Digest::Sha256([15; 32]),
        )],
        repository.model().limits(),
    )
    .unwrap();
    repository
        .model_mut()
        .register_shared_generation(retired)
        .unwrap();
    repository.commit_model().unwrap();
    let roots = repository.read_views().reclamation_roots().unwrap();
    assert_eq!(roots.len(), 3);
    assert_eq!(
        roots
            .iter()
            .map(|root| root.target)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([[1; 32], [2; 32], [3; 32]])
    );
    assert!(!roots.iter().any(|root| root.language == [15; 32]));
    let before = repository.model().snapshot(&keys[0]);
    repository
        .model_mut()
        .apply_selection(&keys[0], &before, None, Uuid::new_v4())
        .unwrap();
    repository.commit_model().unwrap();
    assert_eq!(
        repository.read_views().reclamation_roots().unwrap().len(),
        2
    );
    assert_eq!(repository.model().snapshot(&keys[0]).revision(), 2);
}

#[test]
fn p4_unknown_operation_journal_is_not_silently_ignored() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    writer
        .control(ControlLimits::default())
        .unwrap()
        .replace(
            ControlNamespace::Journals,
            "foreign-operation",
            None,
            b"unrecognized protected state",
        )
        .unwrap();
    drop((writer, repository));
    let report = run(&context, false, None).unwrap();
    assert!(!report.complete && report.operation_id.is_none());
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.status == ReclaimStatus::Unknown
                && entry.label.contains("foreign-operation"))
    );
    assert_eq!(
        std::fs::read(
            context
                .requested_data_root()
                .join("projects/.control/journals/foreign-operation")
        )
        .unwrap(),
        b"unrecognized protected state"
    );
}

#[tokio::test]
async fn p4_real_cycle_all_slot_roots_busy_closure_and_retired_cleanup() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let language = language(&context).await;
    let language_id = ReclaimId::Language(language.key().fingerprint());
    let first = wheel(
        &context,
        &language,
        "cyclea",
        [81; 32],
        vec![[82; 32]],
        vec![("cycle_a.py", b"VALUE=1\n")],
        None,
    );
    let second = wheel(
        &context,
        &language,
        "cycleb",
        [82; 32],
        vec![[81; 32]],
        vec![("cycle_b.py", b"VALUE=2\n")],
        None,
    );
    let layout = crate::libraries::package::layouts::prepare_layout(
        &context,
        &language,
        [83; 32],
        vec![first, second],
    )
    .unwrap();
    let layout_id = ReclaimId::Layout(layout.key());
    let layout_manifest: crate::libraries::package::manifest::LayoutManifest =
        crate::libraries::package::manifest::read_json(
            &context,
            &crate::libraries::package::manifest::layout_name(&layout.key()),
        )
        .unwrap()
        .unwrap();
    layout_manifest.validate_metadata().unwrap();
    let mut bad_layout = layout_manifest.clone();
    bad_layout.packages.clear();
    assert!(bad_layout.validate_metadata().is_err());
    let mut bad_layout = layout_manifest.clone();
    bad_layout.packages.push(bad_layout.packages[0]);
    assert!(bad_layout.validate_metadata().is_err());
    let mut bad_layout = layout_manifest.clone();
    bad_layout.packages.reverse();
    assert!(bad_layout.validate_metadata().is_err());
    let mut bad_layout = layout_manifest.clone();
    bad_layout.storage.key = [0; 32];
    assert!(bad_layout.validate_metadata().is_err());
    let layout_lock = layout
        .prefix()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let (key, old_transaction, allocation) = register_layout(&context, &layout, "auxiliary");
    let active = ProjectRepository::open_readonly(&context)
        .unwrap()
        .read_views()
        .reclamation_roots()
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].group, "auxiliary");
    assert_eq!(active[0].target, *key.target());
    assert_eq!(active[0].packages.len(), 2);
    let rooted = view::read(&context, None, &[], false).unwrap();
    assert!(rooted.complete);
    assert!(
        rooted
            .graph()
            .unwrap()
            .groups(
                &rooted
                    .graph()
                    .unwrap()
                    .closure(rooted.roots.keys().copied())
                    .unwrap()
            )
            .is_empty()
    );
    assert!(
        plan_exact_target(&context, ReclaimId::Package([81; 32]))
            .unwrap_err()
            .to_string()
            .contains("object is still referenced")
    );
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let expected = repository.model().snapshot(&key);
    repository
        .model_mut()
        .apply_selection(&key, &expected, None, Uuid::new_v4())
        .unwrap();
    repository.commit_model().unwrap();
    drop(repository);
    let held = layout.packages()[0].clone();
    drop(layout);
    drop(language);
    let busy = view::read(&context, None, &[], true).unwrap();
    assert!(busy.complete);
    assert_eq!(
        busy.entries[&ReclaimId::Package([81; 32])].status,
        ReclaimStatus::Busy
    );
    assert!(matches!(
        busy.entries[&ReclaimId::Package([82; 32])].status,
        ReclaimStatus::Protected | ReclaimStatus::Busy
    ));
    assert_eq!(busy.entries[&language_id].status, ReclaimStatus::Protected);
    drop(held);
    let free = view::read(&context, None, &[], true).unwrap();
    assert!(free.complete);
    let groups = free.graph().unwrap().groups(&BTreeSet::new());
    assert_eq!(groups[0], vec![layout_id]);
    assert!(groups.iter().any(|group|group==&vec![ReclaimId::Package([81;32]),ReclaimId::Package([82;32])]));
    assert_eq!(groups.last().unwrap(), &vec![language_id]);
    // Exact package removal cannot widen into the unrooted cycle.
    assert!(plan_exact_target(&context, ReclaimId::Package([81; 32])).is_err());
    let result = run(&context, false, None).unwrap();
    assert!(result.complete, "{result:?}");
    for id in [
        layout_id,
        ReclaimId::Package([81; 32]),
        ReclaimId::Package([82; 32]),
        language_id,
    ] {
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.object == Some(id) && entry.status == ReclaimStatus::Removed)
        );
    }
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    assert!(repository.model().generation(&allocation).is_none());
    assert_eq!(repository.model().snapshot(&key).revision(), 2);
    assert!(
        repository
            .model()
            .applied_transaction(old_transaction)
            .is_some()
    );
    assert!(
        context
            .requested_data_root()
            .join("packages/.leases/build")
            .join(format!("{layout_lock}.lock"))
            .is_file()
    );
}

#[tokio::test]
async fn p4_fresh_gate_rejects_new_parent_without_reacquiring_target_sh() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let language = language(&context).await;
    let (facts, archive) = wheel(
        &context,
        &language,
        "dep",
        [84; 32],
        vec![],
        vec![("dep.py", b"VALUE=1\n")],
        None,
    );
    let dependency = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let manifest = dependency.manifest().clone();
    manifest.validate_metadata().unwrap();
    let mut bad_package = manifest.clone();
    bad_package.facts.source.clear();
    assert!(bad_package.validate_metadata().is_err());
    let mut bad_package = manifest.clone();
    bad_package.facts.source = "x".repeat(65537);
    assert!(bad_package.validate_metadata().is_err());
    let mut bad_package = manifest.clone();
    bad_package.facts.artifact.push(' ');
    assert!(bad_package.validate_metadata().is_err());
    let mut bad_package = manifest.clone();
    bad_package.facts.dependencies = vec![[85; 32], [85; 32]];
    assert!(bad_package.validate_metadata().is_err());
    let mut bad_package = manifest.clone();
    bad_package.facts.dependencies = vec![[85; 32], [84; 32]];
    assert!(bad_package.validate_metadata().is_err());
    let id = ReclaimId::Package([84; 32]);
    assert!(plan_exact_target(&context, id).is_ok());
    let (facts, archive) = wheel(
        &context,
        &language,
        "newparent",
        [85; 32],
        vec![[84; 32]],
        vec![("new_parent.py", b"import dep\n")],
        None,
    );
    let parent = prepare_package_tree(&context, &language, facts, archive).unwrap();
    drop(parent);
    drop(dependency);
    let writer = StoreWriter::open_writer(
        &context.requested_data_root().join("packages"),
        nix::unistd::geteuid().as_raw(),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    let lock = StableLock::open_existing(
        writer.build_handle(),
        &format!("{}.lock", manifest.storage.allocation),
        nix::unistd::geteuid().as_raw(),
        crate::libraries::package::lease::identity(manifest.storage.lock),
    )
    .unwrap();
    let held = lock
        .try_acquire(LeaseMode::Exclusive, context.policy().deadline)
        .unwrap()
        .unwrap();
    let error = recheck_group(
        &context,
        ReclaimMode::Prune,
        &[(id, vec![ReclaimId::Language(manifest.language.unwrap())])],
        None,
    )
    .unwrap_err();
    assert!(
        error.downcast_ref::<GroupReferenced>().is_some(),
        "{error:#}"
    );
    held.check_binding().unwrap();
    assert!(
        context
            .requested_data_root()
            .join("packages")
            .join(&manifest.storage.allocation)
            .join("site-packages/dep.py")
            .is_file()
    );
}

#[tokio::test]
async fn p4_old_unmanifested_directory_is_unknown_and_never_deleted() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let language = language(&context).await;
    let language_path = language.root().to_owned();
    drop(language);
    let writer = StoreWriter::open_writer(
        &context.requested_data_root().join("packages"),
        nix::unistd::geteuid().as_raw(),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    drop(writer);
    let orphan = context
        .requested_data_root()
        .join("packages/g-00000000000000000000000000000001");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::write(orphan.join("unknown"), b"preserve").unwrap();
    let report = run(&context, false, None).unwrap();
    assert!(!report.complete && report.operation_id.is_none());
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.status == ReclaimStatus::Unknown
                && entry.label.contains("g-00000000000000000000000000000001"))
    );
    assert_eq!(std::fs::read(orphan.join("unknown")).unwrap(), b"preserve");
    assert!(language_path.exists());
}

async fn selected_language_metadata_damage_blocks_reclaim(key_only: bool) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = context(&root);
    let language = language(&context).await;
    let language_id = ReclaimId::Language(language.key().fingerprint());
    let language_root = language.root().to_owned();
    let (facts, archive) = wheel(
        &context,
        &language,
        "unused",
        [88; 32],
        vec![],
        vec![("unused.py", b"VALUE=3\n")],
        None,
    );
    let unused = prepare_package_tree(&context, &language, facts, archive).unwrap();
    let unused_root = unused.root().to_owned();
    let unused_id = ReclaimId::Package(unused.manifest().key());
    let package_manifest = context
        .requested_data_root()
        .join("packages/.control/catalog")
        .join(crate::libraries::package::manifest::package_name(
            &unused.manifest().key(),
        ));
    let package_before = std::fs::read(&package_manifest).unwrap();
    let project = root.join("selected-project");
    std::fs::create_dir(&project).unwrap();
    let project_source = project.join("pakro.toml");
    let project_bytes = b"[tools]\npython='3.15.0+20261013'\n";
    std::fs::write(&project_source, project_bytes).unwrap();
    let mut repository = ProjectRepository::open_writer(&context).unwrap();
    let project_id = Uuid::from_u128(88);
    repository
        .model_mut()
        .register_project(project_id, project.to_str().unwrap(), &"c".repeat(64))
        .unwrap();
    let selection = SelectionKey::new(
        project_id,
        "default",
        language.key().target(),
        repository.model().limits(),
    )
    .unwrap();
    let expected = repository.model().snapshot(&selection);
    repository
        .model_mut()
        .apply_language_selection(
            &selection,
            &expected,
            &pakro_store::Digest::Sha256(language_id.key()).to_string(),
            Uuid::new_v4(),
        )
        .unwrap();
    repository.commit_model().unwrap();
    drop((repository, unused, language));
    let catalog = context
        .requested_data_root()
        .join("projects/.control/catalog/projects");
    let catalog_before = std::fs::read(&catalog).unwrap();
    let checks = crate::libraries::language::lease::full_inventory_checks();
    let baseline = run(&context, true, None).unwrap();
    assert!(baseline.complete, "{baseline:?}");
    assert!(
        baseline
            .entries
            .iter()
            .any(|entry| entry.object == Some(language_id)
                && entry.status == ReclaimStatus::Protected)
    );
    assert!(baseline.entries.iter().any(|entry| entry.object == Some(unused_id) && entry.status == ReclaimStatus::Eligible));
    assert_eq!(
        crate::libraries::language::lease::full_inventory_checks(),
        checks,
        "protected-language metadata validation must not hash the payload again"
    );
    let manifest = context
        .requested_data_root()
        .join("languages/.control/catalog")
        .join(format!("{}.json", language_id.label()));
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let damaged = if key_only {
        serde_json::json!({"key": original["key"]})
    } else {
        let mut value = original;
        value["schema"] = serde_json::json!(99);
        value
    };
    let damaged_bytes = serde_json::to_vec(&damaged).unwrap();
    std::fs::write(&manifest, &damaged_bytes).unwrap();
    for dry_run in [true, false] {
        let report = run(&context, dry_run, None).unwrap();
        assert!(
            !report.complete && report.operation_id.is_none(),
            "{report:?}"
        );
        assert!(
            report.entries.iter().any(|entry| matches!(
                entry.status,
                ReclaimStatus::Unknown | ReclaimStatus::Damaged
            ) && entry.label.contains("language")),
            "{report:?}"
        );
        assert!(
            !report
                .entries
                .iter()
                .any(|entry| entry.status == ReclaimStatus::Removed)
        );
        assert_eq!(
            std::fs::read(unused_root.join("site-packages/unused.py")).unwrap(),
            b"VALUE=3\n"
        );
        assert_eq!(std::fs::read(&package_manifest).unwrap(), package_before);
        assert_eq!(std::fs::read(&catalog).unwrap(), catalog_before);
        assert_eq!(std::fs::read(&manifest).unwrap(), damaged_bytes);
        assert_eq!(std::fs::read(&project_source).unwrap(), project_bytes);
        assert!(language_root.exists());
    }
}

#[tokio::test]
async fn p4_selected_language_key_only_manifest_blocks_all_reclaim() {
    selected_language_metadata_damage_blocks_reclaim(true).await;
}

#[tokio::test]
async fn p4_selected_language_invalid_schema_blocks_all_reclaim() {
    selected_language_metadata_damage_blocks_reclaim(false).await;
}
