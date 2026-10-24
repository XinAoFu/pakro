use super::*;
use std::io::Read;

#[tokio::test]
async fn s2_source_build_copy_retains_the_original_owned_source_and_deadline() {
    let fixture = project_fixture::ProjectRegistryFixture::sdist();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let context = fixture.context(&endpoints, &policy);
    let deadline = context.deadline;
    let RegistryPreparation::NeedsBuild(mut build) =
        ResolveRegistryDepot::new(&root, &["demo-pkg==1.0"], context)
            .unwrap()
            .prepare()
            .await
            .unwrap()
    else {
        panic!("missing source continuation")
    };
    let scratch = tempfile::tempdir().unwrap();
    let before = build.pin().clone();
    let mut copy = build.source_verified_copy(scratch.path()).unwrap();
    let mut bytes = Vec::new();
    copy.read_to_end(&mut bytes).unwrap();
    assert_eq!(
        bytes,
        include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz")
    );
    assert_eq!(copy.filename(), before.file().filename());
    assert_eq!(copy.digest(), before.digest());
    assert_eq!(copy.size(), before.size());
    assert_eq!(build.pin(), &before);
    assert_eq!(build.registry_context().deadline, deadline);
    assert!(build.usage().retained_bytes >= before.size());
    fixture.no_publish();
}

#[tokio::test]
async fn s2_source_build_copy_refuses_a_revoked_original_lock_before_writing() {
    const SDIST: &str = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz");
    let peer = Peer::new(move |path| {
        if path == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(SDIST)])
        } else {
            Reply::file(bytes)
        }
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let RegistryPreparation::NeedsBuild(mut build) = ResolveRegistryDepot::new(
        &root,
        &["demo-pkg==1.0"],
        fixture.context(&endpoints, &policy),
    )
    .unwrap()
    .prepare()
    .await
    .unwrap() else {
        panic!("missing source continuation")
    };
    let scratch = tempfile::tempdir().unwrap();
    drop(build.source_verified_copy(scratch.path()).unwrap());
    fs::write(
        fixture.root.path().join("pakro.lock"),
        "unknown replacement",
    )
    .unwrap();
    let absent = scratch.path().join("not-created");
    assert!(build.source_verified_copy(&absent).is_err());
    assert!(!absent.exists());
    assert_eq!(
        fs::read(fixture.root.path().join("pakro.lock")).unwrap(),
        b"unknown replacement"
    );
    for path in ["refs", "environment", "journal", "catalog"] {
        assert!(!fixture.root.path().join(path).exists());
    }
    assert_eq!(fixture.credentials.0.load(Ordering::SeqCst), 0);
}
