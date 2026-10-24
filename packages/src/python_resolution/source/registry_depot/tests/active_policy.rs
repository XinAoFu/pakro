//! The old policy continuation is the first missing-functionality boundary.
//! All archive helpers validate real ZIP/METADATA before these source targets.
use super::*;

const PRE: &str = "demo_pkg-2.0rc1-py3-none-any.whl";
const FINAL: &str = "demo_pkg-1.0-py3-none-any.whl";

fn context<'a>(
    fixture: &'a Fixture,
    endpoints: &'a [RegistryEndpoint<'a>],
    source: &'a PythonSourcePolicy,
) -> RegistryDepotContext<'a> {
    let mut context = fixture.context(endpoints, source);
    context.candidate_policy.prereleases = Prereleases::IfNoMatchingFinal;
    context.candidate_policy.yanked = Yanked::ExactPinIfNoUnyanked;
    context
}
fn selected_file<'a>(plan: &'a RegistryPreparedPlan<'_>, name: &str) -> &'a str {
    let selected = plan
        .plan()
        .selected
        .iter()
        .find(|node| node.name == name)
        .unwrap();
    plan.owner(selected.key).unwrap().pin().file().filename()
}
fn pair(final_extra: &str) -> Peer {
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let final_bytes = wheel("demo_pkg", "1.0", &[], final_extra);
    Peer::new(move |path| match path {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(PRE), wire_file(FINAL)]),
        value if value == format!("/files/{PRE}") => Reply::file(&pre),
        value if value == format!("/files/{FINAL}") => Reply::file(&final_bytes),
        _ => Reply::missing(),
    })
}

#[tokio::test]
async fn default_active_policy_selects_the_actual_final_after_observing_the_alternative() {
    let peer = pair("");
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    // Real old explicit-policy source path, independent of the new policy mode.
    let control = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], f.context(&endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(selected_file(&control, "demo-pkg"), PRE);
    drop(control);
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(selected_file(&result, "demo-pkg"), FINAL);
    assert_eq!(result.owners.len(), 1);
    assert!(
        peer.requests()
            .iter()
            .any(|path| path == &format!("/files/{FINAL}"))
    );
    f.no_publish();
}

#[tokio::test]
async fn s4_reject_alternative_keeps_known_final_without_fetching_same_prerelease_source() {
    const PRE: &str = "demo_pkg-1.8.0.dev0-py3-none-any.whl";
    const SOURCE: &str = "demo_pkg-1.8.0.dev0.tar.gz";
    const FINAL: &str = "demo_pkg-1.7.0-py3-none-any.whl";
    let pre = wheel("demo_pkg", "1.8.0.dev0", &[], "");
    let final_bytes = wheel("demo_pkg", "1.7.0", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/demo-pkg/" => Reply::page(
            "demo-pkg",
            vec![wire_file(FINAL), wire_file(SOURCE), wire_file(PRE)],
        ),
        value if value == format!("/files/{PRE}") => Reply::file(&pre),
        value if value == format!("/files/{FINAL}") => Reply::file(&final_bytes),
        // Keep the actual source candidate visible, without supplying invented
        // source metadata or permitting an unnecessary build continuation.
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let mut registry = context(&f, &endpoints, &p);
    registry.candidate_policy.yanked = Yanked::Disallow;
    let preparation = ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], registry)
        .unwrap()
        .prepare()
        .await;
    let requests = peer.requests();
    assert!(
        !requests.iter().any(|path| path == &format!("/files/{SOURCE}")),
        "an already observed eligible final must precede unknown same-prerelease source metadata: {requests:?}"
    );
    let result = ready(preparation.unwrap());
    assert_eq!(selected_file(&result, "demo-pkg"), FINAL);
    assert_eq!(result.plan().selected.len(), 1);
    assert_eq!(result.owners.len(), 1);
    assert_eq!(requests.len(), 3);
    for filename in [PRE, FINAL] {
        assert_eq!(
            requests
                .iter()
                .filter(|path| *path == &format!("/files/{filename}"))
                .count(),
            1,
            "the policy must use each wheel's actual observed metadata: {filename}"
        );
    }
    f.no_publish();
}

#[tokio::test]
async fn s4_reject_alternative_preserves_two_transitive_final_advice_without_source_fetch() {
    const APP: &str = "app-1.0-py3-none-any.whl";
    const A_PRE: &str = "a_policy-1.8.0.dev0-py3-none-any.whl";
    const A_SOURCE: &str = "a_policy-1.8.0.dev0.tar.gz";
    const A_FINAL: &str = "a_policy-1.7.0-py3-none-any.whl";
    const Z_PRE: &str = "z_policy-1.8.0.dev0-py3-none-any.whl";
    const Z_SOURCE: &str = "z_policy-1.8.0.dev0.tar.gz";
    const Z_FINAL: &str = "z_policy-1.7.0-py3-none-any.whl";
    let app = wheel("app", "1.0", &["a-policy>=1", "z-policy>=1"], "");
    let a_pre = wheel("a_policy", "1.8.0.dev0", &[], "");
    let a_final = wheel("a_policy", "1.7.0", &[], "");
    let z_pre = wheel("z_policy", "1.8.0.dev0", &[], "");
    let z_final = wheel("z_policy", "1.7.0", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/app/" => Reply::page("app", vec![wire_file(APP)]),
        "/simple/a-policy/" => Reply::page(
            "a-policy",
            vec![wire_file(A_PRE), wire_file(A_SOURCE), wire_file(A_FINAL)],
        ),
        "/simple/z-policy/" => Reply::page(
            "z-policy",
            vec![wire_file(Z_PRE), wire_file(Z_SOURCE), wire_file(Z_FINAL)],
        ),
        value if value == format!("/files/{APP}") => Reply::file(&app),
        value if value == format!("/files/{A_PRE}") => Reply::file(&a_pre),
        value if value == format!("/files/{A_FINAL}") => Reply::file(&a_final),
        value if value == format!("/files/{Z_PRE}") => Reply::file(&z_pre),
        value if value == format!("/files/{Z_FINAL}") => Reply::file(&z_final),
        // Both source candidates remain advertised and unknown. Neither may
        // be fetched while repairing the other project's prerelease choice.
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let mut registry = context(&f, &endpoints, &p);
    registry.candidate_policy.yanked = Yanked::Disallow;
    // Actual final metadata is acquired independently across solver reentry.
    // Retrying z-policy must preserve a-policy's already observed advice.
    let preparation = ResolveRegistryDepot::new(&root, &["app==1.0"], registry)
        .unwrap()
        .prepare()
        .await;
    let requests = peer.requests();
    for filename in [A_SOURCE, Z_SOURCE] {
        assert!(
            !requests
                .iter()
                .any(|path| path == &format!("/files/{filename}")),
            "both projects' observed eligible finals must precede source builds: {requests:?}"
        );
    }
    let result = ready(preparation.unwrap());
    assert_eq!(selected_file(&result, "app"), APP);
    assert_eq!(selected_file(&result, "a-policy"), A_FINAL);
    assert_eq!(selected_file(&result, "z-policy"), Z_FINAL);
    assert_eq!(result.plan().selected.len(), 3);
    assert_eq!(result.owners.len(), 3);
    assert_eq!(requests.len(), 8);
    for filename in [APP, A_PRE, A_FINAL, Z_PRE, Z_FINAL] {
        assert_eq!(
            requests
                .iter()
                .filter(|path| *path == &format!("/files/{filename}"))
                .count(),
            1,
            "each original wheel must supply its own actual metadata once: {filename}"
        );
    }
    f.no_publish();
}

#[tokio::test]
async fn s4_policy_rejection_keeps_unrelated_transitive_wheel_without_source_fetch() {
    const APP: &str = "app-1.0-py3-none-any.whl";
    const ANYIO_WHEEL: &str = "anyio-4.15.1-py3-none-any.whl";
    const ANYIO_SOURCE: &str = "anyio-4.15.1.tar.gz";
    const POLICY_PRE: &str = "z_policy-2.0rc1-py3-none-any.whl";
    const POLICY_FINAL: &str = "z_policy-1.0-py3-none-any.whl";
    let app = wheel("app", "1.0", &["anyio==4.15.1", "z-policy>=1"], "");
    let anyio = wheel("anyio", "4.15.1", &[], "");
    let pre = wheel("z_policy", "2.0rc1", &[], "");
    let final_bytes = wheel("z_policy", "1.0", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/app/" => Reply::page("app", vec![wire_file(APP)]),
        "/simple/anyio/" => Reply::page(
            "anyio",
            vec![wire_file(ANYIO_WHEEL), wire_file(ANYIO_SOURCE)],
        ),
        "/simple/z-policy/" => Reply::page(
            "z-policy",
            vec![wire_file(POLICY_PRE), wire_file(POLICY_FINAL)],
        ),
        value if value == format!("/files/{APP}") => Reply::file(&app),
        value if value == format!("/files/{ANYIO_WHEEL}") => Reply::file(&anyio),
        value if value == format!("/files/{POLICY_PRE}") => Reply::file(&pre),
        value if value == format!("/files/{POLICY_FINAL}") => Reply::file(&final_bytes),
        // The unrelated source is advertised, but its bytes are never needed.
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    // Transitive nodes are discovered after the policy retry's synthetic
    // witness package, unlike distributions declared directly at the root.
    let preparation = ResolveRegistryDepot::new(&root, &["app==1.0"], context(&f, &endpoints, &p))
        .unwrap()
        .prepare()
        .await;
    let requests = peer.requests();
    assert!(
        !requests
            .iter()
            .any(|path| path == &format!("/files/{ANYIO_SOURCE}")),
        "repairing the prerelease policy must not fetch the unrelated source: {requests:?}"
    );
    let result = ready(preparation.unwrap());
    assert_eq!(selected_file(&result, "app"), APP);
    assert_eq!(selected_file(&result, "anyio"), ANYIO_WHEEL);
    assert_eq!(selected_file(&result, "z-policy"), POLICY_FINAL);
    assert_eq!(result.owners.len(), 3);
    for filename in [APP, ANYIO_WHEEL, POLICY_PRE, POLICY_FINAL] {
        assert_eq!(
            requests
                .iter()
                .filter(|path| *path == &format!("/files/{filename}"))
                .count(),
            1,
            "each actual wheel must be observed once: {filename}"
        );
    }
    f.no_publish();
}

#[tokio::test]
async fn only_matching_pre_is_retained_in_the_real_universe_until_full_constraints_are_known() {
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let peer = Peer::new(move |path| {
        if path == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(PRE)])
        } else {
            Reply::file(&pre)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg>=1", "demo-pkg<3"],
            context(&f, &endpoints, &p),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    assert_eq!(selected_file(&result, "demo-pkg"), PRE);
    assert_eq!(result.roots().len(), 2);
    assert_eq!(
        result.policy_notices(result.plan().selected[0].key),
        [crate::packages::python_resolution::candidate_policy::PolicyNotice::PrereleaseFallback]
    );
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}

#[tokio::test]
async fn unknown_final_actual_requires_python_is_fetched_before_legal_pre_fallback() {
    let peer = pair("Requires-Python: >=99\n");
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(selected_file(&result, "demo-pkg"), PRE);
    assert_eq!(result.owners.len(), 1);
    for filename in [PRE, FINAL] {
        assert!(
            peer.requests()
                .iter()
                .any(|path| path == &format!("/files/{filename}"))
        );
    }
    f.no_publish();
}

#[tokio::test]
async fn actual_parent_backtracking_changes_the_active_policy_without_global_pre_bans() {
    const APP2: &str = "app-2.0-py3-none-any.whl";
    const APP1: &str = "app-1.0-py3-none-any.whl";
    let app2 = wheel("app", "2.0", &["demo-pkg>=1"], "");
    let app1 = wheel("app", "1.0", &["demo-pkg>=2.0rc1"], "");
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let final_bytes = wheel("demo_pkg", "1.0", &["absent>=1"], "");
    let peer = Peer::new(move |path| match path {
        "/simple/app/" => Reply::page("app", vec![wire_file(APP2), wire_file(APP1)]),
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(PRE), wire_file(FINAL)]),
        value if value == format!("/files/{APP2}") => Reply::file(&app2),
        value if value == format!("/files/{APP1}") => Reply::file(&app1),
        value if value == format!("/files/{PRE}") => Reply::file(&pre),
        value if value == format!("/files/{FINAL}") => Reply::file(&final_bytes),
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["app>=1"], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(selected_file(&result, "app"), APP1);
    assert_eq!(selected_file(&result, "demo-pkg"), PRE);
    assert_eq!(result.owners.len(), 2);
    for filename in [APP2, APP1, FINAL] {
        assert!(
            peer.requests()
                .iter()
                .any(|path| path == &format!("/files/{filename}"))
        );
    }
    f.no_publish();
}

#[tokio::test]
async fn false_extra_does_not_supply_a_constraint_or_trigger_a_project_lookup() {
    let app = wheel(
        "app",
        "1.0",
        &["demo-pkg>=2.0rc1; extra == 'fast'"],
        "Provides-Extra: fast\n",
    );
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/app/" => Reply::page("app", vec![wire_file("app-1.0-py3-none-any.whl")]),
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(PRE)]),
        "/files/app-1.0-py3-none-any.whl" => Reply::file(&app),
        value if value == format!("/files/{PRE}") => Reply::file(&pre),
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["app"], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(result.owners.len(), 1);
    assert!(
        peer.requests()
            .iter()
            .all(|path| !path.contains("demo-pkg") && !path.contains("demo_pkg"))
    );
    drop(result);
    let result = ready(
        ResolveRegistryDepot::new(&root, &["app[fast]"], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(result.owners.len(), 2);
    assert_eq!(selected_file(&result, "demo-pkg"), PRE);
    f.no_publish();
}

#[tokio::test]
async fn exact_yanked_uses_actual_non_yanked_m1_eligibility_and_keeps_the_file_identity() {
    const YANK: &str = "demo_pkg-1.0-2-py3-none-any.whl";
    for (other_rejected, exact) in [(false, true), (true, true), (true, false)] {
        let yanked = wheel_with_build("demo_pkg", "1.0", &[], "", Some("2"));
        let other = wheel(
            "demo_pkg",
            "1.0",
            &[],
            if other_rejected {
                "Requires-Python: >=99\n"
            } else {
                ""
            },
        );
        let peer = Peer::new(move |path| {
            if path == "/simple/demo-pkg/" {
                let mut file = wire_file(YANK);
                file["yanked"] = serde_json::json!("retired release");
                Reply::page("demo-pkg", vec![file, wire_file(FINAL)])
            } else if path == format!("/files/{YANK}") {
                Reply::file(&yanked)
            } else if path == format!("/files/{FINAL}") {
                Reply::file(&other)
            } else {
                Reply::missing()
            }
        });
        let f = Fixture::new(&peer);
        let root = f.lock();
        let endpoints = [f.endpoint()];
        let p = policy();
        let raw = if exact {
            "demo-pkg==1.0"
        } else {
            "demo-pkg>=1"
        };
        let result = ResolveRegistryDepot::new(&root, &[raw], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await;
        if exact {
            let result = ready(result.unwrap());
            assert_eq!(
                selected_file(&result, "demo-pkg"),
                if other_rejected { YANK } else { FINAL }
            );
            assert_eq!(result.owners.len(), 1);
            let notices = result.policy_notices(result.plan().selected[0].key);
            if other_rejected {
                assert_eq!(notices, [crate::packages::python_resolution::candidate_policy::PolicyNotice::YankedExactPin]);
            } else {
                assert!(notices.is_empty());
            }
            assert!(result.policy_notices.keys().all(|key| {
                result
                    .plan()
                    .selected
                    .iter()
                    .any(|selected| selected.key == *key)
            }));
        } else {
            assert!(matches!(result, Err(RegistryDepotError::NoSolution)));
        }
        f.no_publish();
    }
}

#[tokio::test]
async fn an_unknown_final_sdist_produces_the_real_build_continuation() {
    const SDIST: &str = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz");
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(PRE), wire_file(SDIST)]),
        value if value == format!("/files/{PRE}") => Reply::file(&pre),
        value if value == format!("/files/{SDIST}") => Reply::file(bytes),
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let state = ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], context(&f, &endpoints, &p))
        .unwrap()
        .prepare()
        .await
        .unwrap();
    let RegistryPreparation::NeedsBuild(source) = state else {
        panic!("unknown actual sdist may not be treated as an absent final")
    };
    assert_eq!(source.pin().file().filename(), SDIST);
    assert!(
        peer.requests()
            .iter()
            .any(|path| path == &format!("/files/{SDIST}"))
    );
    f.no_publish();
}

#[tokio::test]
async fn policy_budget_is_shared_across_metadata_reentry_and_exhaustion_never_publishes() {
    let supported = tags();
    let wheel_tags = pakro_index::WheelTags {
        python: vec!["py3".into()],
        abi: vec!["none".into()],
        platform: vec!["any".into()],
    };
    let rank = supported.plan_rank(&wheel_tags).unwrap().work_bound();
    // Two static files (sort height 1), two owned cache facts, followed by
    // the two actual active assessments around the final metadata rejection.
    let initial = supported.ordered().len() + 2 * (1 + 1 + rank + 1);
    let active = 2 * (1 + 8) + 1;
    let complete = initial + 2 * active;
    for (budget, success) in [(complete, true), (complete - 1, false)] {
        let peer = pair("Requires-Python: >=99\n");
        let f = Fixture::new(&peer);
        let root = f.lock();
        let endpoints = [f.endpoint()];
        let p = policy();
        let mut configured = context(&f, &endpoints, &p);
        configured.limits.candidates.max_work_units = budget;
        let result = ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], configured)
            .unwrap()
            .prepare()
            .await;
        if success {
            assert_eq!(selected_file(&ready(result.unwrap()), "demo-pkg"), PRE);
        } else {
            assert!(matches!(result, Err(RegistryDepotError::Limit)));
        }
        for filename in [PRE, FINAL] {
            assert!(
                peer.requests()
                    .iter()
                    .any(|path| path == &format!("/files/{filename}"))
            );
        }
        f.no_publish();
    }
}

#[tokio::test]
async fn every_new_policy_limit_is_checked_for_an_empty_graph_before_any_http() {
    use pakro_resolver::python_native::policy::NativePolicyLimits;
    let peer = Peer::new(|_| Reply::missing());
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let control = ready(
        ResolveRegistryDepot::new(&root, &[], context(&f, &endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert!(control.plan().selected.is_empty());
    let default = NativePolicyLimits::default();
    for limits in [
        NativePolicyLimits {
            max_rounds: 0,
            ..default
        },
        NativePolicyLimits {
            max_nogood_terms: 0,
            ..default
        },
        NativePolicyLimits {
            max_active_references: 0,
            ..default
        },
        NativePolicyLimits {
            max_propagation_steps: 0,
            ..default
        },
    ] {
        let mut context = context(&f, &endpoints, &p);
        context.limits.policy = limits;
        assert!(matches!(
            ResolveRegistryDepot::new(&root, &[], context),
            Err(RegistryDepotError::InvalidLimits)
        ));
    }
    assert!(peer.requests().is_empty());
    f.no_publish();
}
