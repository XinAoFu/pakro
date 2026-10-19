//! These cases require the real depot loop to succeed first. A failure in
//! that prelude is never evidence about the closure gate.
use super::super::closed::{RegistryClosureError, close_registry_plan, close_with_finish};
use super::*;
use crate::packages::python_resolution::graph_check::{GraphCheckError, GraphCheckLimits};

fn graph_peer() -> Peer {
    let demo = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
    let dep = wheel("dep", "1.0", &[], "");
    Peer::new(move |target| match target {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
        "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
        value if value == format!("/files/{DEMO}") => Reply::file(&demo),
        value if value == format!("/files/{DEP}") => Reply::file(&dep),
        _ => Reply::missing(),
    })
}
async fn prepared<'root>(
    fixture: &Fixture,
    root: &'root BoundPythonLock,
) -> RegistryPreparedPlan<'root> {
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let observed = ready(
        ResolveRegistryDepot::new(
            root,
            &["demo-pkg==1.0", "inactive; python_version<'2'"],
            fixture.context(&endpoints, &policy),
        )
        .expect("real original-root/source constructor prelude")
        .prepare()
        .await
        .expect("real Simple/T/O2/C/M1 solving prelude"),
    );
    observed
        .recheck_for(root)
        .expect("actual selected owner scope prelude");
    assert_eq!(observed.plan().selected.len(), 2);
    observed
}

#[tokio::test]
async fn closure_consumes_real_selected_metadata_and_preserves_original_context() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let observed = prepared(&fixture, &root).await;
    let closed = close_registry_plan(observed, Default::default()).unwrap();
    assert_eq!(closed.checked().roots.len(), 1);
    assert_eq!(closed.checked().edges.len(), 1);
    assert_eq!(closed.plan().selected.len(), 2);
    for selected in &closed.plan().selected {
        let actual = closed.owner(selected.key).unwrap();
        assert_eq!(actual.pin().file().project(), selected.name);
        actual.recheck_for(&root).unwrap();
    }
    closed.recheck_for(&root).unwrap();
    assert_eq!(peer.requests().len(), 4);
    fixture.no_publish();
}

#[tokio::test]
async fn a_changed_local_data_key_cannot_select_an_unowned_file() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let mut observed = prepared(&fixture, &root).await;
    observed.plan.selected[0].key = CandidateKey([0xff; 32]);
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::IdentityMismatch
    );
    fixture.no_publish();
}

#[tokio::test]
async fn actual_pin_and_metadata_versions_cannot_be_replaced_by_solver_claims() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let mut observed = prepared(&fixture, &root).await;
    observed.plan.selected[0].version = "9.0".into();
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::IdentityMismatch
    );
    fixture.no_publish();
}

#[tokio::test]
async fn selected_owners_cannot_be_silently_discarded_after_preparation() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let mut observed = prepared(&fixture, &root).await;
    observed.plan.selected.pop();
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::IdentityMismatch
    );
    fixture.no_publish();
}

#[tokio::test]
async fn unrequested_extras_cannot_be_signed_by_a_real_metadata_owner() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let mut observed = prepared(&fixture, &root).await;
    observed.plan.selected[0]
        .requested_extras
        .push("ghost".into());
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::Graph(GraphCheckError::NonMinimal)
    );
    fixture.no_publish();
}

#[tokio::test]
async fn actual_wheel_tags_are_rechecked_against_the_frozen_target() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let mut observed = prepared(&fixture, &root).await;
    observed.supported = SupportedWheelTags::new(vec![
        WheelTag::new("cp311", "cp311", "linux_x86_64").unwrap(),
    ])
    .unwrap();
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::TargetMismatch
    );
    fixture.no_publish();
}

#[tokio::test]
async fn replacement_of_the_actual_project_image_revokes_a_prepared_graph() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let observed = prepared(&fixture, &root).await;
    let external = b"# private external writer\nlockfile_version=4\n";
    fs::write(fixture.root.path().join("pakro.lock"), external).unwrap();
    assert_eq!(
        close_registry_plan(observed, Default::default()).unwrap_err(),
        RegistryClosureError::Revoked
    );
    assert_eq!(
        fs::read(fixture.root.path().join("pakro.lock")).unwrap(),
        external
    );
    assert_eq!(peer.requests().len(), 4);
}

#[tokio::test]
async fn a_closed_graph_stays_bound_to_the_original_actual_project() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let other = tempfile::tempdir().unwrap();
    let other = BoundPythonLock::acquire(
        &other.path().join("pakro.lock"),
        Default::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    let observed = prepared(&fixture, &root).await;
    let closed = close_registry_plan(observed, GraphCheckLimits::default()).unwrap();
    closed.recheck_for(&root).unwrap();
    assert_eq!(
        closed.recheck_for(&other).unwrap_err(),
        RegistryClosureError::Revoked
    );
    fixture.no_publish();
}

#[tokio::test]
async fn an_external_image_change_after_semantic_checks_is_rejected_at_the_final_gate() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let observed = prepared(&fixture, &root).await;
    let path = fixture.root.path().join("pakro.lock");
    let external = b"# late private external writer\nlockfile_version=4\n";
    let error = close_with_finish(observed, Default::default(), || {
        fs::write(&path, external).unwrap();
    })
    .unwrap_err();
    assert_eq!(error, RegistryClosureError::Revoked);
    assert_eq!(fs::read(path).unwrap(), external);
    assert_eq!(peer.requests().len(), 4);
}

#[tokio::test]
async fn the_original_deadline_is_checked_before_closure_and_after_final_synchronous_work() {
    for expire_inside in [false, true] {
        let peer = graph_peer();
        let fixture = Fixture::new(&peer);
        let root = fixture.lock();
        let endpoints = [fixture.endpoint()];
        let policy = policy();
        let mut context = fixture.context(&endpoints, &policy);
        let end = Instant::now() + Duration::from_secs(1);
        context.deadline = end;
        let observed = ready(
            ResolveRegistryDepot::new(&root, &["demo-pkg==1.0"], context)
                .unwrap()
                .prepare()
                .await
                .expect("actual preparation must succeed inside the original deadline"),
        );
        assert_eq!(observed.deadline(), end);
        let expire = || {
            if let Some(left) = end.checked_duration_since(Instant::now()) {
                thread::sleep(left);
            }
            thread::sleep(Duration::from_millis(2));
        };
        let error = if expire_inside {
            close_with_finish(observed, Default::default(), expire).unwrap_err()
        } else {
            expire();
            close_registry_plan(observed, Default::default()).unwrap_err()
        };
        assert_eq!(error, RegistryClosureError::Deadline);
        assert_eq!(peer.requests().len(), 4);
        fixture.no_publish();
    }
}

#[tokio::test]
async fn closure_limits_reject_the_complete_batch_before_a_final_hook_or_publication() {
    let peer = graph_peer();
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let observed = prepared(&fixture, &root).await;
    assert_eq!(
        close_with_finish(
            observed,
            GraphCheckLimits {
                max_nodes: 1,
                ..Default::default()
            },
            || panic!("over-budget graph reached the final scope hook")
        )
        .unwrap_err(),
        RegistryClosureError::Graph(GraphCheckError::Limit)
    );
    fixture.no_publish();
}
