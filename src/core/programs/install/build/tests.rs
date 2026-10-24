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
