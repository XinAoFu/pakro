use super::*;
use crate::packages::python_resolution::lock_model::*;
use crate::packages::python_resolution::lock_session::{
    ExistingFileSlot, FilePinLookup, ResolutionIntent,
};
use crate::packages::python_resolution::source::replay::{
    LockedReplayContext, LockedReplayOrigin, acquire_locked_registry_file,
    metadata::{LockedMetadataState, route_locked_metadata},
};
use pakro_index::python_markers::{MarkerSemantics, PythonMarkerEnvironment, PythonMarkerScope};
use pakro_index::simple::SourceRequestRouter;
use pakro_index::{SupportedWheelTags, WheelTag};
use std::fs;
use std::time::Duration;

#[path = "../replay/tests/support.rs"]
mod replay_support;
use replay_support::*;
#[path = "tests/support.rs"]
mod support;
use support::*;

#[path = "tests/scoped.rs"]
mod scoped;

#[tokio::test]
async fn p2_locked_wheel_replay_reaches_shared_sources_without_new_selection() {
    use crate::packages::python_resolution::source::registry_depot::closed::close_registry_plan;
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = write_graph(
        &fixture,
        &target,
        ORIGINALS,
        &specs,
        &[(0, "demo-pkg"), (2, "demo-pkg")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    );
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let replay = LockedRegistryReplay::new(
        &root,
        &key,
        ORIGINALS,
        &target,
        context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
    )
    .unwrap();
    let strict = ready(replay.prepare().await);
    let prepared = strict.into_registry_plan().unwrap();
    let graph = close_registry_plan(prepared, Default::default()).unwrap();
    assert_eq!(graph.plan().selected.len(), 2);
    assert_eq!(graph.checked().roots.len(), 2);
    assert_eq!(graph.checked().edges.len(), 1);
    let (archives, evidence) = graph.into_shared_sources().unwrap();
    assert_eq!(archives.len(), 2);
    evidence
        .recheck(tokio::time::Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[test]
fn p2_source_output_digest_and_metadata_are_independent_locked_evidence() {
    use crate::packages::python_environment::model::SourceBuildRecord;
    use std::io::Cursor;
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let source = Spec {
        name: "demo-pkg".into(),
        version: "1.0".into(),
        filename: "demo-pkg-1.0.tar.gz".into(),
        bytes: b"original source evidence".to_vec(),
    };
    let output = Spec::wheel("demo-pkg", "1.0", &[], "");
    let artifact = spool_verified_reader(
        Cursor::new(&output.bytes),
        &output.filename,
        Digest::from_sha256_bytes(&output.bytes),
        1024 * 1024,
        fixture.root.path(),
    )
    .unwrap();
    let archive = crate::packages::python_wheel_source::inspect_owned_wheel(
        artifact,
        Digest::from_sha256_bytes(&output.bytes),
        &crate::packages::python_wheel::WheelExpectation {
            filename: &output.filename,
            name: &output.name,
            version: &output.version,
            supported: target.supported(),
        },
        Default::default(),
    )
    .unwrap();
    let record = SourceBuildRecord {
        source_digest: source.pin(&fixture).digest().to_string(),
        request_id: [7; 32],
        recipe: 1,
        backend: "demo_backend".into(),
        inputs: Vec::new(),
        output_filename: output.filename.clone(),
        output_digest: archive.digest().to_string(),
        output_size: archive.size(),
        metadata_sha256: Sha256::digest(archive.manifest().core_metadata().raw()).into(),
        observation: None,
    };
    for metadata_changed in [false, true] {
        let mut changed = record.clone();
        if metadata_changed {
            changed.metadata_sha256[0] ^= 1;
        } else {
            changed.output_digest = Digest::Sha256([8; 32]).to_string();
        }
        let artifact = spool_verified_reader(
            Cursor::new(&output.bytes),
            &output.filename,
            Digest::from_sha256_bytes(&output.bytes),
            1024 * 1024,
            fixture.root.path(),
        )
        .unwrap();
        assert!(matches!(
            build::inspect_output(
                artifact,
                &source.pin(&fixture),
                &changed,
                target.supported(),
                Default::default()
            ),
            Err(LockedGraphError::BuildMismatch)
        ));
    }
    assert!(peer.requests().is_empty());
}

#[tokio::test]
async fn p2_source_replay_retains_original_pin_and_exact_cached_output() {
    use crate::packages::python_environment::model::{SourceBuildObservation, SourceBuildRecord};
    use crate::packages::python_resolution::source::registry_depot::closed::close_registry_plan;
    use std::io::Cursor;
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let source = Spec {
        name: "demo-pkg".into(),
        version: "1.0".into(),
        filename: "demo_pkg-1.0.tar.gz".into(),
        bytes: include_bytes!("../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz").to_vec(),
    };
    let output = Spec::wheel("demo-pkg", "1.0", &[], "");
    let mut artifact = spool_verified_reader(
        Cursor::new(&output.bytes),
        &output.filename,
        Digest::from_sha256_bytes(&output.bytes),
        1024 * 1024,
        fixture.root.path(),
    )
    .unwrap();
    crate::artifact_snapshot::publish_verified_artifact(&mut artifact, &fixture.cache, 1024 * 1024)
        .unwrap();
    let archive = crate::packages::python_wheel_source::inspect_owned_wheel(
        artifact,
        Digest::from_sha256_bytes(&output.bytes),
        &crate::packages::python_wheel::WheelExpectation {
            filename: &output.filename,
            name: &output.name,
            version: &output.version,
            supported: target.supported(),
        },
        Default::default(),
    )
    .unwrap();
    let record = SourceBuildRecord {
        source_digest: source.pin(&fixture).digest().to_string(),
        request_id: [7; 32],
        recipe: 1,
        backend: "demo_backend".into(),
        inputs: Vec::new(),
        output_filename: output.filename.clone(),
        output_digest: archive.digest().to_string(),
        output_size: archive.size(),
        metadata_sha256: Sha256::digest(archive.manifest().core_metadata().raw()).into(),
        observation: Some(SourceBuildObservation {
            initial_requirements: Vec::new(),
            initial_inputs: Vec::new(),
            dynamic_requirements: Vec::new(),
        }),
    };
    let pin = source.pin(&fixture);
    let (root, key) = write_graph_with_builds(
        &fixture,
        &target,
        &["demo-pkg==1.0"],
        std::slice::from_ref(&source),
        &[(0, "demo-pkg")],
        &[],
        &[(pin.id(), record.clone())],
    );
    prelude(&fixture, &root, &target, std::slice::from_ref(&source)).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let replay = LockedRegistryReplay::new(
        &root,
        &key,
        &["demo-pkg==1.0"],
        &target,
        context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
    )
    .unwrap();
    let prepared = ready(replay.prepare().await).into_registry_plan().unwrap();
    let graph = close_registry_plan(prepared, Default::default()).unwrap();
    let selected = &graph.plan().selected[0];
    let owner = graph.owner(selected.key).unwrap();
    assert_eq!(owner.pin(), &pin);
    assert_eq!(owner.output_filename(), output.filename);
    assert_eq!(owner.build_record(), Some(&record));
    let (archives, evidence) = graph.into_shared_sources().unwrap();
    assert_eq!(archives[0].filename(), output.filename);
    assert_ne!(archives[0].digest(), pin.digest());
    evidence
        .recheck(tokio::time::Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(peer.requests().is_empty());
}

#[tokio::test]
async fn original_arbitrary_root_rhs_cannot_be_normalized_during_strict_preflight() {
    for (declaration, expected) in [("demo-pkg===1.0", true), ("demo-pkg===01.0", false)] {
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let target = target();
        let specs = [Spec::wheel("demo-pkg", "1.0", &[], "")];
        let originals = [declaration];
        let (root, key) = write_graph(
            &fixture,
            &target,
            &originals,
            &specs,
            &[(0, "demo-pkg")],
            &[],
        );
        prelude(&fixture, &root, &target, &specs).await;
        let endpoints = endpoints(&fixture, &NO_ROUTE);
        let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
        let result = LockedRegistryReplay::new(
            &root,
            &key,
            &originals,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        );
        if expected {
            ready(result.unwrap().prepare().await);
        } else {
            assert!(
                matches!(
                    result,
                    Err(LockedGraphError::Graph(GraphCheckError::Unsatisfied))
                ),
                "raw ===01.0 must not admit the actual 1.0 node"
            );
        }
        assert!(peer.requests().is_empty());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn all_warm_exact_wheels_replay_complete_original_roots_with_no_source_observation() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let before = fs::read(&fixture.lock_path).unwrap();
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    assert_eq!(
        plan.roots()
            .iter()
            .map(PythonRequirementSyntax::raw_for_semantic_resolution)
            .collect::<Vec<_>>(),
        ORIGINALS
    );
    assert_eq!(
        plan.graph()
            .roots()
            .iter()
            .map(|r| r.declaration_index())
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert_eq!(plan.target().fingerprint(), target.fingerprint());
    assert_eq!(plan.runtime_pin().content_tree(), &[9; 32]);
    assert_eq!(plan.derived().graph.roots.len(), 2);
    assert_eq!(plan.derived().graph.edges.len(), 1);
    for (name, extra) in [("demo-pkg", "fast"), ("dep", "slow")] {
        let owner = plan.owner(name).unwrap();
        assert_eq!(owner.origin(), LockedReplayOrigin::Cache);
        assert!(owner.file_facts().is_none() && owner.source_key().is_none());
        owner.recheck_for(&root).unwrap();
        assert_eq!(
            plan.derived().extras[&plan.node_key(name).unwrap()],
            [extra]
        );
    }
    assert!(std::ptr::eq(plan.root(), &root));
    plan.recheck_for(&root).unwrap();
    assert!(peer.requests().is_empty());
    assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn cold_exact_files_keep_old_pins_and_warm_the_next_offline_graph_without_reselection() {
    let specs = specs();
    let payloads = specs
        .iter()
        .map(|s| (s.filename.clone(), s.bytes.clone()))
        .collect::<Vec<_>>();
    let files = Peer::new(move |path| {
        let bytes = &payloads
            .iter()
            .find(|(name, _)| path.starts_with(&format!("/files/{name}?")))
            .expect("only the exact locked locator may be requested")
            .1;
        Reply::file(bytes)
    });
    let pages=specs.iter().map(|s| {
        let exact=file_json(&s.filename,&format!("{}/files/{}?ticket=new-private-locator",files.url,s.filename),serde_json::json!({}));
        let trap=file_json(&s.filename.replace("-py3-none-any.whl","-2-py3-none-any.whl"),"/forbidden-replacement",serde_json::json!({}));
        (format!("/simple/{}/",s.name),serde_json::to_vec(&serde_json::json!({"meta":{"api-version":"1.0"},"name":s.name,"files":[trap,exact]})).unwrap())
    }).collect::<Vec<_>>();
    let index = Peer::new(move |path| {
        Reply::page(
            pages
                .iter()
                .find(|(name, _)| name == path)
                .expect("only old locked projects may be fetched")
                .1
                .clone(),
        )
    });
    let fixture = Fixture::new(&index);
    let target = target();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    fs::remove_dir_all(&fixture.cache).unwrap();
    let endpoints = endpoints(&fixture, &ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed),
        )
        .unwrap()
        .prepare()
        .await,
    );
    for spec in &specs {
        let owner = plan.owner(&spec.name).unwrap();
        assert_eq!(owner.pin().id(), spec.pin(&fixture).id());
        assert_eq!(owner.origin(), LockedReplayOrigin::Registry);
        assert!(
            owner
                .file_facts()
                .unwrap()
                .locator()
                .url()
                .query()
                .unwrap()
                .contains("new-private-locator")
        );
    }
    assert_eq!(index.requests().len(), 2);
    assert_eq!(files.requests().len(), 2);
    drop(plan);
    let cached_endpoints = support::endpoints(&fixture, &NO_ROUTE);
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &cached_endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    assert_eq!(
        plan.owner("demo-pkg").unwrap().origin(),
        LockedReplayOrigin::Cache
    );
    assert_eq!(index.requests().len(), 2);
    assert_eq!(files.requests().len(), 2);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn complete_roots_and_frozen_context_must_match_before_any_router_provider_or_http() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    for changed in [
        vec![ORIGINALS[0], ORIGINALS[2]],
        vec![ORIGINALS[2], ORIGINALS[1], ORIGINALS[0]],
        vec![ORIGINALS[0], ORIGINALS[1]],
    ] {
        assert_eq!(
            LockedRegistryReplay::new(
                &root,
                &key,
                &changed,
                &target,
                context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
            )
            .unwrap_err(),
            LockedGraphError::RootMismatch
        );
    }
    let mut changed = environment();
    changed.platform_release = "a different explicit release";
    let changed = FrozenRegistryTarget::new(
        changed,
        &supported(),
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &changed,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
        )
        .unwrap_err(),
        LockedGraphError::TargetMismatch
    );
    let changed = FrozenRegistryTarget::new(
        environment(),
        &supported(),
        PythonMarkerScope::Dependency {
            requested_extras: &[],
        },
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &changed,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
        )
        .unwrap_err(),
        LockedGraphError::TargetMismatch
    );
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn ready_and_new_requests_remain_bound_to_the_actual_original_project_image() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    let other = tempfile::tempdir().unwrap();
    let other_path = other.path().join("pakro.lock");
    fs::copy(&fixture.lock_path, &other_path).unwrap();
    let other = BoundPythonLock::acquire(
        &other_path,
        Default::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        plan.recheck_for(&other).unwrap_err(),
        LockedGraphError::Revoked
    );
    fs::write(
        &fixture.lock_path,
        "lockfile_version=4\n# external actual image\n",
    )
    .unwrap();
    assert_eq!(
        plan.recheck_for(&root).unwrap_err(),
        LockedGraphError::Revoked
    );
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
        )
        .unwrap_err(),
        LockedGraphError::Revoked
    );
    assert!(peer.requests().is_empty());
    assert!(
        fs::read_to_string(&fixture.lock_path)
            .unwrap()
            .contains("external actual image")
    );
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn all_inactive_roots_are_a_real_empty_graph_but_absent_groups_and_schema_one_are_not() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let inactive = ["inactive; sys_platform == 'win32'"];
    let (root, key) = write_graph(&fixture, &target, &inactive, &[], &[], &[]);
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            &inactive,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    assert!(plan.graph().nodes().is_empty());
    assert_eq!(plan.roots().len(), 1);
    let missing = GroupKey::new("", "missing", *target.fingerprint(), Default::default()).unwrap();
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &missing,
            &inactive,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
        )
        .unwrap_err(),
        LockedGraphError::MissingGroup
    );
    for body in [
        "lockfile_version=4\n[python]\nschema=1\npins=[]\ngroups=[]\n",
        "lockfile_version=4\n[python]\nschema=2\npins=[]\ngroups=[]\n",
    ] {
        let other = tempfile::tempdir().unwrap();
        let path = other.path().join("pakro.lock");
        fs::write(&path, body).unwrap();
        let actual = BoundPythonLock::acquire(
            &path,
            Default::default(),
            Instant::now() + Duration::from_secs(5),
        );
        if body.contains("schema=1") {
            assert!(actual.is_err());
        } else {
            assert_eq!(
                LockedRegistryReplay::new(
                    &actual.unwrap(),
                    &key,
                    &inactive,
                    &target,
                    context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed)
                )
                .unwrap_err(),
                LockedGraphError::MissingGroup
            );
        }
    }
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn offline_missing_and_corrupt_expected_cache_do_not_fall_back_or_repair() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    let spec = &specs[1];
    let pin = spec.pin(&fixture);
    let path = fixture.seed_cache(&spec.filename, pin.digest(), &spec.bytes);
    fs::remove_file(&path).unwrap();
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline)
        )
        .unwrap()
        .prepare()
        .await
        .unwrap_err(),
        LockedGraphError::MissingInputs(vec![pin.id()])
    );
    fs::write(&path, b"corrupt expected bytes").unwrap();
    for network in [NetworkPolicy::Offline, NetworkPolicy::Allowed] {
        assert_eq!(
            LockedRegistryReplay::new(
                &root,
                &key,
                ORIGINALS,
                &target,
                context(&fixture, &endpoints, &policy, network)
            )
            .unwrap()
            .prepare()
            .await
            .unwrap_err(),
            LockedGraphError::Replay(LockedReplayError::Cache)
        );
        assert_eq!(fs::read(&path).unwrap(), b"corrupt expected bytes");
    }
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn actual_metadata_requires_python_and_inner_identity_are_checked_after_old_bytes() {
    for extra in ["Requires-Python: >=99\n", "Name: another-inner-identity\n"] {
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let target = target();
        let control = Spec::wheel("demo-pkg", "1.0", &[], "");
        let (root, key) = write_graph(
            &fixture,
            &target,
            &["demo-pkg"],
            std::slice::from_ref(&control),
            &[(0, "demo-pkg")],
            &[],
        );
        // The independent original expected-byte/C control passes before the
        // new strict entry is exercised. The malformed identity is only later.
        prelude(&fixture, &root, &target, std::slice::from_ref(&control)).await;
        let endpoints = endpoints(&fixture, &NO_ROUTE);
        let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
        ready(
            LockedRegistryReplay::new(
                &root,
                &key,
                &["demo-pkg"],
                &target,
                context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
            )
            .unwrap()
            .prepare()
            .await,
        );
        drop(root);
        let bad = Spec::wheel("demo-pkg", "1.0", &[], extra);
        let (root, key) = write_graph(
            &fixture,
            &target,
            &["demo-pkg"],
            std::slice::from_ref(&bad),
            &[(0, "demo-pkg")],
            &[],
        );
        fixture.seed_cache(&bad.filename, bad.pin(&fixture).digest(), &bad.bytes);
        let expected = acquire_locked_registry_file(
            root.existing_pin(bad.pin(&fixture).id()).unwrap().unwrap(),
            fixture.context(NetworkPolicy::Offline, &NO_ROUTE),
        )
        .await
        .unwrap();
        assert_eq!(expected.size(), bad.bytes.len() as u64);
        drop(expected);
        let error = LockedRegistryReplay::new(
            &root,
            &key,
            &["demo-pkg"],
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap_err();
        if extra.starts_with("Requires") {
            assert_eq!(
                error,
                LockedGraphError::Graph(GraphCheckError::RequiresPython)
            );
        } else {
            assert!(matches!(error, LockedGraphError::Metadata(_)));
        }
        assert!(peer.requests().is_empty());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn closure_checks_original_specs_and_exact_edge_multiplicity_without_relying_on_order() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    drop(root);
    for edges in [
        vec![("demo-pkg", "dep", "dep>=0")],
        vec![
            ("demo-pkg", "dep", DEPENDENCY),
            ("demo-pkg", "dep", DEPENDENCY),
        ],
    ] {
        let (root, key) = write_graph(
            &fixture,
            &target,
            ORIGINALS,
            &specs,
            &[(0, "demo-pkg"), (2, "demo-pkg")],
            &edges,
        );
        assert_eq!(
            LockedRegistryReplay::new(
                &root,
                &key,
                ORIGINALS,
                &target,
                context(&fixture, &endpoints, &policy, NetworkPolicy::Offline)
            )
            .unwrap()
            .prepare()
            .await
            .unwrap_err(),
            LockedGraphError::GraphMismatch
        );
        drop(root);
    }
    let (root, key) = write_graph(
        &fixture,
        &target,
        ORIGINALS,
        &specs[..1],
        &[(0, "demo-pkg"), (2, "demo-pkg")],
        &[],
    );
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline)
        )
        .unwrap()
        .prepare()
        .await
        .unwrap_err(),
        LockedGraphError::Graph(GraphCheckError::Unsatisfied)
    );
    drop(root);
    let repeated = vec![
        Spec::wheel("demo-pkg", "1.0", &[DEPENDENCY, "dep<2"], ""),
        Spec::wheel("dep", "1.0", &[], ""),
    ];
    let (root, key) = write_graph(
        &fixture,
        &target,
        ORIGINALS,
        &repeated,
        &[(0, "demo-pkg"), (2, "demo-pkg")],
        &[
            ("demo-pkg", "dep", "dep<2"),
            ("demo-pkg", "dep", DEPENDENCY),
        ],
    );
    prelude(&fixture, &root, &target, &repeated).await;
    assert_eq!(
        ready(
            LockedRegistryReplay::new(
                &root,
                &key,
                ORIGINALS,
                &target,
                context(&fixture, &endpoints, &policy, NetworkPolicy::Offline)
            )
            .unwrap()
            .prepare()
            .await
        )
        .derived()
        .graph
        .edges
        .len(),
        2
    );
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn extras_are_derived_from_real_roots_and_reject_self_supporting_conditional_cycles() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let cycle = vec![
        Spec::wheel("demo-pkg", "1.0", &[DEPENDENCY], ""),
        Spec::wheel("dep", "1.0", &["demo-pkg[fast]"], ""),
    ];
    let edges = [
        ("demo-pkg", "dep", DEPENDENCY),
        ("dep", "demo-pkg", "demo-pkg[fast]"),
    ];
    let (root, key) = write_graph(
        &fixture,
        &target,
        &["demo-pkg[fast]"],
        &cycle,
        &[(0, "demo-pkg")],
        &edges,
    );
    prelude(&fixture, &root, &target, &cycle).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let plan = ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            &["demo-pkg[fast]"],
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    assert_eq!(
        plan.derived().extras[&plan.node_key("demo-pkg").unwrap()],
        ["fast"]
    );
    assert_eq!(
        plan.derived().extras[&plan.node_key("dep").unwrap()],
        ["slow"]
    );
    drop(plan);
    drop(root);
    let (root, key) = write_graph(
        &fixture,
        &target,
        &["demo-pkg"],
        &cycle,
        &[(0, "demo-pkg")],
        &edges,
    );
    assert_eq!(
        LockedRegistryReplay::new(
            &root,
            &key,
            &["demo-pkg"],
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline)
        )
        .unwrap()
        .prepare()
        .await
        .unwrap_err(),
        LockedGraphError::Graph(GraphCheckError::NonMinimal)
    );
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn source_and_sdist_inputs_return_owned_typed_continuations_without_fake_metadata() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let sdist = Spec {
        name: "demo-pkg".into(),
        version: "1.0".into(),
        filename: "demo_pkg-1.0.tar.gz".into(),
        bytes: include_bytes!("../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz").to_vec(),
    };
    let (root, key) = write_graph(
        &fixture,
        &target,
        &["demo-pkg"],
        std::slice::from_ref(&sdist),
        &[(0, "demo-pkg")],
        &[],
    );
    prelude(&fixture, &root, &target, std::slice::from_ref(&sdist)).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    let state = LockedRegistryReplay::new(
        &root,
        &key,
        &["demo-pkg"],
        &target,
        context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
    );
    assert!(
        matches!(state, Err(LockedGraphError::MissingBuild)),
        "a historical source pin without exact build/output evidence cannot be replayed"
    );
    drop(root);
    let reference = "../${PRIVATE_TOKEN}/source";
    let spec = Spec::wheel(
        "demo-pkg",
        "1.0",
        &[
            "unused @ ../inactive ; sys_platform == 'win32'",
            "unbound @ ../${PRIVATE_TOKEN}/source ; extra == 'fast'",
        ],
        "",
    );
    let (root, key) = write_graph(
        &fixture,
        &target,
        &["demo-pkg[fast]"],
        std::slice::from_ref(&spec),
        &[(0, "demo-pkg")],
        &[],
    );
    prelude(&fixture, &root, &target, std::slice::from_ref(&spec)).await;
    let state = LockedRegistryReplay::new(
        &root,
        &key,
        &["demo-pkg[fast]"],
        &target,
        context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
    )
    .unwrap()
    .prepare()
    .await
    .unwrap();
    let LockedGraphPreparation::NeedsSourceAdapter(source) = state else {
        panic!("an active unresolved source token cannot become a version range")
    };
    assert!(matches!(
        source.location(),
        GraphSourceLocation::Dependency { declaration: 1, .. }
    ));
    assert_eq!(
        source
            .reference_for_source_binding()
            .unwrap()
            .raw_for_source_binding(),
        reference
    );
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn whole_attempt_limits_are_checked_instead_of_reset_for_each_cached_node() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let target = target();
    let specs = specs();
    let (root, key) = ordinary_graph(&fixture, &target, &specs);
    prelude(&fixture, &root, &target, &specs).await;
    let endpoints = endpoints(&fixture, &NO_ROUTE);
    let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
    ready(
        LockedRegistryReplay::new(
            &root,
            &key,
            ORIGINALS,
            &target,
            context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
        )
        .unwrap()
        .prepare()
        .await,
    );
    let total = specs.iter().map(|s| s.bytes.len() as u64).sum();
    let mut exact = context(&fixture, &endpoints, &policy, NetworkPolicy::Offline);
    exact.limits.max_nodes = 2;
    exact.limits.max_retained_archive_bytes = total;
    ready(
        LockedRegistryReplay::new(&root, &key, ORIGINALS, &target, exact)
            .unwrap()
            .prepare()
            .await,
    );
    for axis in 0..4 {
        let mut ctx = context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed);
        match axis {
            0 => ctx.limits.max_nodes = 1,
            1 => ctx.limits.max_retained_archive_bytes = total - 1,
            2 => ctx.limits.max_retained_utf8_bytes = 1,
            _ => ctx.limits.lock.max_document_bytes = 1,
        }
        assert_eq!(
            LockedRegistryReplay::new(&root, &key, ORIGINALS, &target, ctx).unwrap_err(),
            LockedGraphError::Limit
        );
    }
    let mut ctx = context(&fixture, &endpoints, &policy, NetworkPolicy::Allowed);
    ctx.limits.target.markers.max_nodes = 0;
    assert_eq!(
        LockedRegistryReplay::new(&root, &key, ORIGINALS, &target, ctx).unwrap_err(),
        LockedGraphError::InvalidLimits
    );
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn the_last_real_owner_gate_rejects_expired_deadlines_and_external_lock_edits() {
    for expire in [true, false] {
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let target = target();
        let specs = specs();
        let (root, key) = ordinary_graph(&fixture, &target, &specs);
        prelude(&fixture, &root, &target, &specs).await;
        let endpoints = endpoints(&fixture, &NO_ROUTE);
        let policy = PythonSourcePolicy::Explicit(fixture.registry.id().clone());
        ready(
            LockedRegistryReplay::new(
                &root,
                &key,
                ORIGINALS,
                &target,
                context(&fixture, &endpoints, &policy, NetworkPolicy::Offline),
            )
            .unwrap()
            .prepare()
            .await,
        );
        let mut ctx = context(&fixture, &endpoints, &policy, NetworkPolicy::Offline);
        ctx.deadline = Instant::now() + Duration::from_millis(200);
        let last = ctx.deadline;
        let reached = std::cell::Cell::new(false);
        let result = LockedRegistryReplay::new(&root, &key, ORIGINALS, &target, ctx)
            .unwrap()
            .prepare_with_finish(|| {
                reached.set(true);
                if expire {
                    while Instant::now() <= last + Duration::from_millis(5) {
                        std::hint::spin_loop();
                    }
                } else {
                    fs::write(
                        &fixture.lock_path,
                        "lockfile_version=4\n# changed at last actual owner gate\n",
                    )
                    .unwrap();
                }
            })
            .await;
        assert!(
            reached.get(),
            "the test must reach the real late gate, not an earlier setup failure"
        );
        assert_eq!(
            result.unwrap_err(),
            if expire {
                LockedGraphError::Deadline
            } else {
                LockedGraphError::Revoked
            }
        );
        assert!(peer.requests().is_empty());
        fixture.assert_no_authority_writes();
    }
}
