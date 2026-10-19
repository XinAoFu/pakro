use super::*;

fn selected_target() -> FrozenRegistryTarget {
    FrozenRegistryTarget::new(
        environment(),
        &supported(),
        PythonMarkerScope::Lock {
            selected_extras: &["ui"],
            dependency_groups: &["dev"],
        },
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap()
}
fn scopes() -> Vec<OwnedRootMarkerScope> {
    vec![
        OwnedRootMarkerScope::Environment,
        OwnedRootMarkerScope::ProjectExtras(vec!["ui".into()]),
        OwnedRootMarkerScope::Environment,
    ]
}
const RAW: &[&str] = &[
    "inactive; sys_platform=='win32'",
    "demo-pkg[fast]; extra=='ui'",
    "dep; python_version>='3'",
];

#[tokio::test]
async fn actual_expected_wheels_keep_project_and_group_scopes_through_locked_closure() {
    let peer = Peer::new(|_| Reply::denied());
    let f = Fixture::new(&peer);
    let target = selected_target();
    let specs = specs();
    let (root, key) = write_graph(
        &f,
        &target,
        RAW,
        &specs,
        &[(1, "demo-pkg"), (2, "dep")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&f, &root, &target, &specs).await;
    let endpoints = endpoints(&f, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(f.registry.id().clone());
    let expected = scopes();
    let plan = ready(
        LockedRegistryReplay::new_scoped(
            &root,
            &key,
            RAW,
            &expected,
            &target,
            context(&f, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    assert_eq!(plan.root_scopes(), Some(expected.as_slice()));
    assert_eq!(
        plan.derived()
            .graph
            .roots
            .iter()
            .map(|root| root.declaration)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        plan.derived().extras[&plan.node_key("demo-pkg").unwrap()],
        ["fast"]
    );
    assert_eq!(
        plan.derived().extras[&plan.node_key("dep").unwrap()],
        ["slow"]
    );
    assert_eq!(plan.derived().graph.edges.len(), 1);
    plan.recheck_for(&root).unwrap();
    assert!(peer.requests().is_empty());
    f.assert_no_authority_writes();
}

#[tokio::test]
async fn a_locked_group_cannot_read_an_extra_defined_only_for_the_project() {
    let peer = Peer::new(|_| Reply::denied());
    let f = Fixture::new(&peer);
    let target = selected_target();
    let specs = specs();
    let raw = [RAW[0], RAW[1], "dep; extra=='ui'"];
    let (root, key) = write_graph(
        &f,
        &target,
        &raw,
        &specs,
        &[(1, "demo-pkg"), (2, "dep")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&f, &root, &target, &specs).await;
    let endpoints = endpoints(&f, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(f.registry.id().clone());
    assert!(matches!(
        LockedRegistryReplay::new_scoped(
            &root,
            &key,
            &raw,
            &scopes(),
            &target,
            context(&f, &endpoints, &policy, NetworkPolicy::Offline)
        ),
        Err(LockedGraphError::Graph(GraphCheckError::Marker))
    ));
    assert!(peer.requests().is_empty());
    f.assert_no_authority_writes();
}

#[tokio::test]
async fn the_attempt_owns_its_original_scopes_before_the_caller_changes_selection_data() {
    let peer = Peer::new(|_| Reply::denied());
    let f = Fixture::new(&peer);
    let target = selected_target();
    let specs = specs();
    let (root, key) = write_graph(
        &f,
        &target,
        RAW,
        &specs,
        &[(1, "demo-pkg"), (2, "dep")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&f, &root, &target, &specs).await;
    let endpoints = endpoints(&f, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(f.registry.id().clone());
    let mut original = scopes();
    let replay = LockedRegistryReplay::new_scoped(
        &root,
        &key,
        RAW,
        &original,
        &target,
        context(&f, &endpoints, &policy, NetworkPolicy::Offline),
    )
    .unwrap();
    original[1] = OwnedRootMarkerScope::Environment;
    let plan = ready(replay.prepare().await);
    assert_eq!(plan.root_scopes(), Some(scopes().as_slice()));
    assert!(peer.requests().is_empty());
    f.assert_no_authority_writes();
}

#[tokio::test]
async fn locked_scope_cardinality_matches_all_original_inactive_and_repeated_roots() {
    let peer = Peer::new(|_| Reply::denied());
    let f = Fixture::new(&peer);
    let target = selected_target();
    let specs = specs();
    let (root, key) = write_graph(
        &f,
        &target,
        RAW,
        &specs,
        &[(1, "demo-pkg"), (2, "dep")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&f, &root, &target, &specs).await;
    let endpoints = endpoints(&f, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(f.registry.id().clone());
    for count in [2, 4] {
        let input = vec![OwnedRootMarkerScope::Environment; count];
        assert!(matches!(
            LockedRegistryReplay::new_scoped(
                &root,
                &key,
                RAW,
                &input,
                &target,
                context(&f, &endpoints, &policy, NetworkPolicy::Offline)
            ),
            Err(LockedGraphError::RootMismatch)
        ));
    }
    assert!(peer.requests().is_empty());
    f.assert_no_authority_writes();
}

#[tokio::test]
async fn original_scope_bytes_are_bounded_before_the_attempt_clones_them() {
    let peer = Peer::new(|_| Reply::denied());
    let f = Fixture::new(&peer);
    let target = selected_target();
    let specs = specs();
    let (root, key) = write_graph(
        &f,
        &target,
        RAW,
        &specs,
        &[(1, "demo-pkg"), (2, "dep")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&f, &root, &target, &specs).await;
    let endpoints = endpoints(&f, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(f.registry.id().clone());
    let mut valid = context(&f, &endpoints, &policy, NetworkPolicy::Offline);
    valid.limits.max_retained_utf8_bytes = 4096;
    LockedRegistryReplay::new_scoped(&root, &key, RAW, &scopes(), &target, valid).unwrap();
    let mut oversized = scopes();
    oversized[0] = OwnedRootMarkerScope::ProjectExtras(vec!["x".repeat(4097)]);
    let mut invalid = context(&f, &endpoints, &policy, NetworkPolicy::Offline);
    invalid.limits.max_retained_utf8_bytes = 4096;
    assert!(matches!(
        LockedRegistryReplay::new_scoped(&root, &key, RAW, &oversized, &target, invalid),
        Err(LockedGraphError::Limit)
    ));
    assert!(peer.requests().is_empty());
    f.assert_no_authority_writes();
}
