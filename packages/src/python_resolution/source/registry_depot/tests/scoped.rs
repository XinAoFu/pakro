use super::*;
use crate::packages::python_resolution::source::registry_depot::closed::close_registry_plan;

#[tokio::test]
async fn different_root_contexts_survive_actual_http_metadata_and_final_closure() {
    let demo = wheel("demo_pkg", "1.0", &["ghost; extra=='ui'"], "");
    let dep = wheel("dep", "1.0", &[], "");
    let peer = Peer::new(move |target| match target {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
        "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
        value if value == format!("/files/{DEMO}") => Reply::file(&demo),
        value if value == format!("/files/{DEP}") => Reply::file(&dep),
        _ => Reply::missing(),
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let context = fixture.context(&endpoints, &policy);
    let scopes = [
        RegistryRootScope::ProjectExtras(vec!["ui".into()]),
        RegistryRootScope::Environment,
    ];
    let prepared = ready(
        ResolveRegistryDepot::new_scoped(
            &root,
            &["demo-pkg; extra=='ui'", "dep; python_version>='3'"],
            &scopes,
            context,
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    let closed = close_registry_plan(prepared, Default::default()).unwrap();
    assert_eq!(
        closed
            .checked()
            .roots
            .iter()
            .map(|row| row.declaration)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(closed.checked().edges.is_empty());
    assert_eq!(closed.plan().selected.len(), 2);
    assert_eq!(peer.requests().len(), 4);
    fixture.no_publish();
}

#[test]
fn group_extra_markers_fail_before_observing_registry_availability() {
    let peer = Peer::new(|_| panic!("marker failure precedes HTTP"));
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let scopes = [
        RegistryRootScope::ProjectExtras(vec!["ui".into()]),
        RegistryRootScope::Environment,
    ];
    assert_eq!(
        ResolveRegistryDepot::new_scoped(
            &root,
            &["demo-pkg; extra=='ui'", "dep; extra=='ui'"],
            &scopes,
            fixture.context(&endpoints, &policy)
        )
        .unwrap_err(),
        RegistryDepotError::RootMarker
    );
    assert!(peer.requests().is_empty());
    fixture.no_publish();
}

#[test]
fn every_original_root_requires_exactly_one_scope() {
    let peer = Peer::new(|_| panic!("invalid scope cardinality precedes HTTP"));
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    for scopes in [vec![], vec![RegistryRootScope::Environment; 2]] {
        assert_eq!(
            ResolveRegistryDepot::new_scoped(
                &root,
                &["demo-pkg; python_version<'2'"],
                &scopes,
                fixture.context(&endpoints, &policy)
            )
            .unwrap_err(),
            RegistryDepotError::InvalidConfiguration
        );
    }
    assert!(peer.requests().is_empty());
}

#[test]
fn unused_scope_names_are_bounded_before_owned_copies_or_http() {
    let peer = Peer::new(|_| panic!("scope limits precede HTTP"));
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let mut context = fixture.context(&endpoints, &policy);
    context.limits.max_utf8_bytes = 256;
    let scopes = [RegistryRootScope::ProjectExtras(vec!["x".repeat(257)])];
    assert_eq!(
        ResolveRegistryDepot::new_scoped(
            &root,
            &["demo-pkg; python_version<'2'"],
            &scopes,
            context
        )
        .unwrap_err(),
        RegistryDepotError::Limit
    );
    assert!(peer.requests().is_empty());
}

#[tokio::test]
async fn an_explicit_empty_scoped_request_is_a_real_empty_plan() {
    let peer = Peer::new(|_| panic!("empty plan has no availability input"));
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let prepared = ready(
        ResolveRegistryDepot::new_scoped(&root, &[], &[], fixture.context(&endpoints, &policy))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    let closed = close_registry_plan(prepared, Default::default()).unwrap();
    assert!(closed.plan().selected.is_empty());
    assert!(closed.checked().roots.is_empty());
    assert!(peer.requests().is_empty());
    fixture.no_publish();
}
