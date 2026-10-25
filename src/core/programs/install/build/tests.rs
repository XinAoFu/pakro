//! The dynamic solve must reuse the exact source-built input already observed
//! by the backend, including a legitimate non-reproducible generated wheel.
use super::*;
use crate::packages::python_resolution::candidate_policy::Sdists;
use crate::packages::python_resolution::source::registry_depot::ProjectRegistryFixture;
use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_coordinator_reuses_nonce_source_dependency_across_dynamic_solve() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let (fixture, counter_digest, helper_digest) =
        ProjectRegistryFixture::dynamic_source_dependency();
    let root = fixture.lock_at_deadline(context.policy().deadline);
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.candidate_policy.sdists = Sdists::Include;
    registry.deadline = context.policy().deadline;
    let mut budget = BuildBudget::new();
    let prepared = prepare_registry(
        &context,
        &language,
        &root,
        vec!["demo-pkg==1.0".into()],
        registry,
        &mut budget,
        0,
    )
    .await
    .expect("the final backend must see the exact nonce input from its requirements hook");
    let closed = close_registry_plan(prepared, Default::default()).unwrap();
    closed.recheck_for(&root).unwrap();
    assert_eq!(
        closed.plan().selected.len(),
        1,
        "build inputs are not invented runtime dependencies"
    );
    let node = &closed.plan().selected[0];
    assert_eq!(node.name, "demo-pkg");
    let source = closed.owner(node.key).unwrap();
    assert_eq!(source.pin().file().kind(), RegistryArtifactKind::Sdist);
    let record = source.build_record().unwrap().clone();
    record.validate().unwrap();
    assert_eq!(record.inputs.len(), 2);
    let counter = record
        .inputs
        .iter()
        .find(|input| input.name == "counter-dep")
        .unwrap();
    assert!(counter.source_is_sdist);
    assert_eq!(counter.source_filename, "counter_dep-1.0.tar.gz");
    assert_eq!(counter.source_digest, counter_digest.to_string());
    assert_eq!(counter.output_filename, "counter_dep-1.0-py3-none-any.whl");
    assert_ne!(counter.source_digest, counter.output_digest);
    counter.content_pin().unwrap();
    let cached = budget
        .completed
        .values()
        .find(|(build, _)| build.record().source_digest == counter.source_digest)
        .unwrap();
    assert_eq!(counter.output_digest, cached.0.record().output_digest);
    assert_eq!(counter.output_size, cached.0.record().output_size);
    let helper = record
        .inputs
        .iter()
        .find(|input| input.name == "helper")
        .unwrap();
    assert!(!helper.source_is_sdist);
    assert_eq!(helper.source_digest, helper_digest.to_string());
    assert_eq!(helper.output_digest, helper.source_digest);
    assert_eq!(
        budget.builds, 2,
        "demo and counter are each actually built once"
    );
    assert_eq!(budget.completed.len(), 2);
    assert!(budget.active.is_empty());
    assert!(budget.policy_work > 0 && budget.policy_work <= 16_777_216);
    assert!(budget.policy_utf8 > 0 && budget.policy_utf8 <= 64 * 1024 * 1024);
    assert_eq!(budget.workspace_reserved, 0);
    let (archives, evidence) = closed.into_shared_sources().unwrap();
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].manifest().name(), "demo-pkg");
    assert_eq!(archives[0].digest().to_string(), record.output_digest);
    evidence
        .recheck(tokio::time::Instant::from_std(context.policy().deadline))
        .unwrap();
    root.recheck().unwrap();
    language.check_binding().unwrap();
    fixture.no_publish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2_source_cold_and_warm_replay_keeps_recursive_inputs_and_rejects_changed_output() {
    use crate::programs::install::{lock, resolve};
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    for deterministic in [true, false] {
        let (fixture, _, _) = if deterministic {
            ProjectRegistryFixture::deterministic_source_dependency()
        } else {
            ProjectRegistryFixture::dynamic_source_dependency()
        };
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let endpoints = [fixture.endpoint()];
        let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
        let mut registry = fixture.context(&endpoints, &policy);
        registry.environment = language.target().environment();
        registry.supported = language.target().supported();
        registry.semantics = language.target().semantics();
        registry.candidate_policy.sdists = Sdists::Include;
        registry.deadline = context.policy().deadline;
        let roots = if deterministic {
            vec![("demo-pkg", "1.0"), ("other-pkg", "1.0")]
        } else {
            vec![("demo-pkg", "1.0")]
        };
        let declarations = roots
            .iter()
            .map(|(name, version)| format!("{name}=={version}"))
            .collect::<Vec<_>>();
        let graph =
            resolve::resolve_exact_roots(&context, &language, &root, &roots, registry.clone())
                .await
                .unwrap();
        let main = graph
            .graph()
            .plan()
            .selected
            .iter()
            .find(|node| node.name == "demo-pkg")
            .unwrap();
        let record = graph
            .graph()
            .owner(main.key)
            .unwrap()
            .build_record()
            .unwrap()
            .clone();
        assert_eq!(record.inputs.len(), 2);
        assert_eq!(record.observation.as_ref().unwrap().initial_inputs.len(), 1);
        let projection = graph
            .projection(&vec![[0; 32]; graph.graph().plan().selected.len()])
            .unwrap();
        let model = lock::project_lock_model(
            &root,
            &language,
            &projection,
            &roots
                .iter()
                .map(|(name, version)| ((*name).into(), (*version).into()))
                .collect::<Vec<_>>(),
            graph.builds(),
        )
        .unwrap();
        let bytes = root
            .prepare_document(model, Default::default())
            .unwrap()
            .bytes()
            .to_vec();
        drop(graph);
        drop(root);
        let lock_path = fixture.project_path().with_file_name("pakro.lock");
        std::fs::write(&lock_path, &bytes).unwrap();
        let root = fixture.lock_at_deadline(context.policy().deadline);
        let cache = stage.join(format!("strict-cache-{deterministic}"));
        let scratch = stage.join(format!("strict-scratch-{deterministic}"));
        std::fs::create_dir_all(&scratch).unwrap();
        registry.cache_root = &cache;
        registry.scratch_root = &scratch;
        let before = fixture.requests().len();
        let mut budget = BuildBudget::new();
        let replayed = replay_registry(
            &context,
            &language,
            &root,
            declarations.clone(),
            registry.clone(),
            &mut budget,
            0,
        )
        .await;
        assert_eq!(
            budget.policy_work, 0,
            "strict replay cannot enumerate solver candidates"
        );
        assert_eq!(budget.workspace_reserved, 0);
        assert!(budget.active.is_empty());
        if !deterministic {
            let error = match replayed {
                Ok(_) => {
                    panic!("a newly generated nonce output must not replace the locked output")
                }
                Err(error) => error,
            };
            assert!(
                error.chain().any(|cause| cause.to_string()
                    == "strict source rebuilt output differs from the locked artifact"),
                "nonce replay must reject its actual rebuilt output; another error is not evidence: {error:#}"
            );
            assert_eq!(std::fs::read(&lock_path).unwrap(), bytes);
            fixture.no_publish_with_existing_lock(&bytes);
            eprintln!(
                "P2_STRICT_NONCE_OUTPUT_REJECTED locked_bytes_preserved=true no_publication=true"
            );
            continue;
        }
        assert!(
            record
                .inputs
                .iter()
                .find(|input| input.name == "counter-dep")
                .unwrap()
                .output_size
                < 64 * 1024,
            "the backend's 64 KiB intermediate must exceed its compressed wheel size"
        );
        assert_eq!(
            budget.builds, 3,
            "two parents and their shared initial/final source dependency must each build once"
        );
        assert_eq!(budget.completed.len(), 3);
        let counter = record
            .inputs
            .iter()
            .find(|input| input.name == "counter-dep")
            .unwrap();
        let cached = budget
            .completed
            .values()
            .find(|(build, _)| build.record().source_digest == counter.source_digest)
            .unwrap();
        assert_eq!(cached.0.record().output_digest, counter.output_digest);
        let closed = close_registry_plan(replayed.unwrap(), Default::default()).unwrap();
        assert_eq!(closed.plan().selected.len(), 2);
        let main = closed
            .plan()
            .selected
            .iter()
            .find(|node| node.name == "demo-pkg")
            .unwrap();
        let output = closed.owner(main.key).unwrap();
        assert_eq!(
            output.build_record().unwrap().fingerprint().unwrap(),
            record.fingerprint().unwrap()
        );
        assert!(
            fixture.requests().len() > before,
            "cold replay must acquire actual original bytes"
        );
        drop(closed);
        let after_cold = fixture.requests().len();
        let mut warm = BuildBudget::new();
        let replayed = replay_registry(
            &context,
            &language,
            &root,
            declarations,
            registry,
            &mut warm,
            0,
        )
        .await
        .unwrap();
        close_registry_plan(replayed, Default::default()).unwrap();
        assert_eq!(
            fixture.requests().len(),
            after_cold,
            "warm exact inputs require no registry access"
        );
        assert_eq!(warm.policy_work, 0);
        assert_eq!(
            warm.builds, 3,
            "each source builds once in the new warm operation"
        );
        assert_eq!(warm.completed.len(), 3);
        assert_eq!(warm.workspace_reserved, 0);
        assert!(warm.active.is_empty());
        assert_eq!(std::fs::read(&lock_path).unwrap(), bytes);
        fixture.no_publish_with_existing_lock(&bytes);
    }
}
