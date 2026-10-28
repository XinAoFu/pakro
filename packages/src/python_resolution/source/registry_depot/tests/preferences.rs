//! Preferences come from real prior source observations and remain soft data.
use super::*;

const NEW: &str = "demo_pkg-2.0-py3-none-any.whl";
const ALTERNATE: &str = "demo_pkg-1.0-2-py3-none-any.whl";
const DEP_NEW: &str = "dep-2.0-py3-none-any.whl";

struct PreferenceFixture {
    peer: Peer,
    mode: Arc<AtomicUsize>,
}
impl PreferenceFixture {
    fn new() -> Self {
        let mode = Arc::new(AtomicUsize::new(0));
        let available = Arc::clone(&mode);
        let old = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
        let alternate = wheel_with_build("demo_pkg", "1.0", &["dep==1.0"], "", Some("2"));
        let new = wheel("demo_pkg", "2.0", &["dep==2.0"], "");
        let dep_old = wheel("dep", "1.0", &[], "");
        let dep_new = wheel("dep", "2.0", &[], "");
        let peer = Peer::new(move |path| match path {
            "/simple/demo-pkg/" => {
                let files = match available.load(Ordering::SeqCst) {
                    0 => vec![wire_file(DEMO)],
                    1 => vec![wire_file(NEW), wire_file(ALTERNATE), wire_file(DEMO)],
                    _ => vec![wire_file(NEW), wire_file(ALTERNATE)],
                };
                Reply::page("demo-pkg", files)
            }
            "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP_NEW), wire_file(DEP)]),
            value if value == format!("/files/{DEMO}") => Reply::file(&old),
            value if value == format!("/files/{ALTERNATE}") => Reply::file(&alternate),
            value if value == format!("/files/{NEW}") => Reply::file(&new),
            value if value == format!("/files/{DEP}") => Reply::file(&dep_old),
            value if value == format!("/files/{DEP_NEW}") => Reply::file(&dep_new),
            _ => Reply::missing(),
        });
        Self { peer, mode }
    }
}

fn selected_filename<'a>(plan: &'a RegistryPreparedPlan<'_>, name: &str) -> &'a str {
    let selected = plan
        .plan()
        .selected
        .iter()
        .find(|node| node.name == name)
        .unwrap();
    plan.owner(selected.key).unwrap().pin().file().filename()
}

#[tokio::test]
async fn p3_old_file_then_old_version_precedes_new_candidates_without_hard_constraints() {
    let server = PreferenceFixture::new();
    let fixture = Fixture::new(&server.peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let source_policy = policy();
    let first = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg>=1"],
            fixture.context(&endpoints, &source_policy),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    let preferred = first
        .plan()
        .selected
        .iter()
        .map(|node| first.owner(node.key).unwrap().pin().clone())
        .collect::<Vec<_>>();
    assert_eq!(selected_filename(&first, "demo-pkg"), DEMO);
    drop(first);
    server.mode.store(1, Ordering::SeqCst);
    for (mode, declarations, expected_demo, expected_dep, use_preferences) in [
        (1, vec!["demo-pkg>=1"], DEMO, DEP, true),
        (2, vec!["demo-pkg>=1"], ALTERNATE, DEP, true),
        // The old candidate's actual metadata conflicts with a new root.
        // The solver must be able to replace that candidate and its dep.
        (1, vec!["demo-pkg>=1", "dep==2.0"], NEW, DEP_NEW, true),
        (1, vec!["demo-pkg>=1"], NEW, DEP_NEW, false),
    ] {
        server.mode.store(mode, Ordering::SeqCst);
        let mut context = fixture.context(&endpoints, &source_policy);
        context.candidate_policy.prereleases = Prereleases::IfNoMatchingFinal;
        if use_preferences {
            context.preferred_pins = preferred.clone();
        }
        let prepared = ready(
            ResolveRegistryDepot::new(&root, &declarations, context)
                .unwrap()
                .prepare()
                .await
                .unwrap(),
        );
        assert_eq!(selected_filename(&prepared, "demo-pkg"), expected_demo);
        assert_eq!(selected_filename(&prepared, "dep"), expected_dep);
        let closed =
            super::super::closed::close_registry_plan(prepared, Default::default()).unwrap();
        closed.recheck_for(&root).unwrap();
        assert_eq!(closed.plan().selected.len(), 2);
    }
    fixture.no_publish();
}

#[tokio::test]
async fn p3_preference_descriptors_do_not_grant_installed_prerelease_eligibility() {
    let pre = wheel("demo_pkg", "2.0rc1", &[], "");
    let final_bytes = wheel("demo_pkg", "1.0", &[], "");
    let peer = Peer::new(move |path| match path {
        "/simple/demo-pkg/" => Reply::page(
            "demo-pkg",
            vec![
                wire_file("demo_pkg-2.0rc1-py3-none-any.whl"),
                wire_file(DEMO),
            ],
        ),
        "/files/demo_pkg-2.0rc1-py3-none-any.whl" => Reply::file(&pre),
        value if value == format!("/files/{DEMO}") => Reply::file(&final_bytes),
        _ => Reply::missing(),
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let source_policy = policy();
    let first = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg>=1"],
            fixture.context(&endpoints, &source_policy),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    let preferred = first
        .owner(first.plan().selected[0].key)
        .unwrap()
        .pin()
        .clone();
    assert_eq!(preferred.file().version(), "2.0rc1");
    drop(first);
    let mut context = fixture.context(&endpoints, &source_policy);
    context.candidate_policy.prereleases = Prereleases::IfNoMatchingFinal;
    context.preferred_pins = vec![preferred];
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], context)
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(selected_filename(&result, "demo-pkg"), DEMO);
    fixture.no_publish();
}

#[tokio::test]
async fn p3_same_filename_from_another_registry_or_base_is_only_a_version_preference() {
    use crate::packages::python_resolution::lock_model::{RegistryFileRef, RegistryFileRefInput};
    let server = PreferenceFixture::new();
    let fixture = Fixture::new(&server.peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let source_policy = policy();
    let first = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg>=1"],
            fixture.context(&endpoints, &source_policy),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    let selected = first
        .plan()
        .selected
        .iter()
        .find(|node| node.name == "demo-pkg")
        .unwrap();
    let preferred = first.owner(selected.key).unwrap().pin().clone();
    drop(first);
    server.mode.store(1, Ordering::SeqCst);
    for change_registry in [false, true] {
        let old = preferred.file();
        let mut base = *old.canonical_base_fingerprint();
        if !change_registry {
            base[0] ^= 1;
        }
        let file = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: if change_registry {
                    "another"
                } else {
                    old.registry_id()
                },
                canonical_base_fingerprint: base,
                project: old.project(),
                version: old.version(),
                filename: old.filename(),
                kind: old.kind(),
            },
            Default::default(),
        )
        .unwrap();
        let changed = ContentPin::new(
            &file,
            preferred.digest(),
            preferred.size(),
            preferred.evidence(),
            Default::default(),
        )
        .unwrap();
        let mut context = fixture.context(&endpoints, &source_policy);
        context.preferred_pins = vec![changed];
        let plan = ready(
            ResolveRegistryDepot::new(&root, &["demo-pkg>=1"], context)
                .unwrap()
                .prepare()
                .await
                .unwrap(),
        );
        assert_eq!(selected_filename(&plan, "demo-pkg"), ALTERNATE);
    }
    fixture.no_publish();
}

#[tokio::test]
async fn p3_preference_inputs_are_bounded_before_registry_requests() {
    let server = PreferenceFixture::new();
    let fixture = Fixture::new(&server.peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let source_policy = policy();
    let first = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg>=1"],
            fixture.context(&endpoints, &source_policy),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    let preferred = first
        .plan()
        .selected
        .iter()
        .map(|node| first.owner(node.key).unwrap().pin().clone())
        .collect::<Vec<_>>();
    drop(first);
    let requests = server.peer.requests();
    for axis in 0..6 {
        let mut context = fixture.context(&endpoints, &source_policy);
        context.preferred_pins = preferred.clone();
        let expected = match axis {
            0 => {
                context.preferred_pins.push(preferred[0].clone());
                RegistryDepotError::InvalidConfiguration
            }
            1 => {
                context.limits.lock.max_pins = 1;
                RegistryDepotError::Limit
            }
            2 => {
                context.limits.max_steps = 1;
                RegistryDepotError::Limit
            }
            3 => {
                context.limits.max_utf8_bytes = 1;
                RegistryDepotError::Limit
            }
            4 => {
                context.limits.lock.max_field_bytes = 1;
                RegistryDepotError::Limit
            }
            _ => {
                context.limits.lock.max_artifact_bytes = 1;
                RegistryDepotError::Limit
            }
        };
        assert_eq!(
            ResolveRegistryDepot::new(&root, &[], context).unwrap_err(),
            expected
        );
        assert_eq!(server.peer.requests(), requests);
    }
    fixture.no_publish();
}
