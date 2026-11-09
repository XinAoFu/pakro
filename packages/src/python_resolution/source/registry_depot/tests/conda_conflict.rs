use super::*;
use crate::libraries::package::conda::{prepare_conda_layout, publish_conda_package};
use crate::packages::conda_artifact;
use crate::programs::install::mixed::resolve_wheels;

/// Both versions contain executable Python and complete package metadata. The
/// conflict is only dep 2.0 fixed by Conda versus consumer 1.0 requiring dep<2.
#[tokio::test]
#[ignore = "requires the verified PAKRO_TEST_PYTHON_ARCHIVE"]
async fn mixed_conflict_uses_installable_packages_and_reports_requirements() {
    let consumer = wheel("consumer", "1.0", &["dep>=1,<2"], "");
    let compatible = wheel("compatible", "1.0", &["dep>=2,<3"], "");
    let dep = wheel("dep", "1.0", &[], "");
    let peer = Peer::new(move |target| match target {
        "/simple/consumer/" => Reply::page("consumer", vec![wire_file("consumer-1.0-py3-none-any.whl")]),
        "/simple/compatible/" => Reply::page("compatible", vec![wire_file("compatible-1.0-py3-none-any.whl")]),
        "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
        "/files/consumer-1.0-py3-none-any.whl" => Reply::file(&consumer),
        "/files/compatible-1.0-py3-none-any.whl" => Reply::file(&compatible),
        "/files/dep-1.0-py3-none-any.whl" => Reply::file(&dep),
        _ => Reply::missing(),
    });
    let fixture = Fixture::new(&peer);
    let context = crate::libraries::language::repository::tests::operation_context(
        &fixture.root.path().canonicalize().unwrap(), true, Duration::from_secs(180),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let filename = "dep-2.0-test_0.tar.bz2";
    let index = serde_json::json!({
        "name": "dep", "version": "2.0", "build": "test_0", "build_number": 0,
        "subdir": "noarch", "noarch": "python", "depends": [],
    });
    let mut files = vec![
        ("site-packages/dep/__init__.py", b"VALUE=84\n".to_vec()),
        ("site-packages/dep-2.0.dist-info/METADATA", b"Metadata-Version: 2.4\nName: dep\nVersion: 2.0\n".to_vec()),
    ];
    let paths = files.iter().map(|(path, bytes)| serde_json::json!({
        "_path": path, "path_type": "hardlink", "size_in_bytes": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes)),
    })).collect::<Vec<_>>();
    files.push(("info/index.json", serde_json::to_vec(&index).unwrap()));
    files.push(("info/paths.json", serde_json::to_vec(&serde_json::json!({
        "paths_version": 1, "paths": paths,
    })).unwrap()));
    let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice()).unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    let mut record = index;
    record["fn"] = filename.into();
    record["url"] = format!("https://example.invalid/noarch/{filename}").into();
    record["channel"] = "https://example.invalid/".into();
    record["size"] = bytes.len().into();
    record["sha256"] = hex::encode(Sha256::digest(&bytes)).into();
    let record: rattler_conda_types::RepoDataRecord = serde_json::from_value(record).unwrap();
    let mut artifact = crate::artifact_snapshot::spool_verified_reader(
        Cursor::new(&bytes), filename, Digest::from_sha256_bytes(&bytes),
        1024 * 1024, context.requested_cache_root(),
    ).unwrap();
    let tree = conda_artifact::extract_project(&context, &mut artifact, &record).unwrap();
    let package = publish_conda_package(
        &context, &mut artifact, &tree, std::slice::from_ref(&record), &[], None,
    ).unwrap();
    let root = BoundPythonLock::acquire(
        &fixture.root.path().join("pakro.lock"), Default::default(), context.policy().deadline,
    ).unwrap();
    let endpoints = [fixture.endpoint()];
    let policy = policy();

    // Positive controls exercise real publication, layout construction and imports.
    for (declaration, fixed, code) in [
        ("consumer==1.0", false, "import consumer, dep; assert consumer.VALUE == 42 and dep.VALUE == 42"),
        ("compatible==1.0", true, "import compatible, dep; assert compatible.VALUE == 42 and dep.VALUE == 84"),
    ] {
        let owners = if fixed {
            conda_metadata::inspect_conda_distributions(&context, &package, &tree).unwrap()
        } else { Vec::new() };
        let result = resolve_wheels(
            &context, &language, &root, &[declaration.into()],
            fixture.context(&endpoints, &policy), owners,
        ).await.unwrap();
        let mut packages = result.wheels.clone();
        let projected = if fixed {
            packages.push(package.clone());
            vec![(&package, &tree)]
        } else { Vec::new() };
        let layout = prepare_conda_layout(
            &context, &language, result.graph.fingerprint().unwrap(), &packages, &projected,
        ).await.unwrap();
        let output = std::process::Command::new(layout.bin().join("python"))
            .args(["-B", "-c", code]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    let before = peer.requests().len();
    let fixed = conda_metadata::inspect_conda_distributions(&context, &package, &tree).unwrap();
    let error = match resolve_wheels(
        &context, &language, &root, &["consumer==1.0".into()],
        fixture.context(&endpoints, &policy), fixed,
    ).await {
        Ok(_) => panic!("incompatible fixed Conda version must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("project requires consumer==1.0"), "{error}");
    assert!(error.contains("Conda package dep 2.0 build test_0 provides fixed dep==2.0"), "{error}");
    assert!(error.contains("consumer==1.0 requires dep>=1,<2"), "{error}");
    assert!(error.contains("cannot be replaced by PyPI"), "{error}");
    assert!(!error.contains("example.invalid") && !error.contains(&peer.url), "{error}");
    assert!(!peer.requests()[before..].iter().any(|path| path == "/simple/dep/"));
    assert!(!fixture.root.path().join("pakro.lock").exists());
    eprintln!("MIXED_CONFLICT_DIAGNOSTIC: {error}");
}
