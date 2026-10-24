use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use std::io::{Read, Write};
use std::time::Duration;

fn bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, path, *body).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn source(bytes: &[u8], root: &Path) -> VerifiedArtifact {
    source_named(bytes, "demo-1.0.tar.gz", root)
}

fn source_named(bytes: &[u8], filename: &str, root: &Path) -> VerifiedArtifact {
    spool_verified_reader(
        bytes,
        filename,
        pakro_store::Digest::from_sha256_bytes(bytes),
        1024 * 1024,
        root,
    )
    .unwrap()
}

#[test]
fn s1_source_build_counts_implicit_directories_in_extraction_budget() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let bytes = bundle(&[
        (
            "demo-1.0/pyproject.toml",
            b"[build-system]\nrequires=[]\nbuild-backend='fixture'\n",
        ),
        ("demo-1.0/a/b/c/payload.py", b"pass\n"),
    ]);
    assert!(
        extract_source(
            &mut source(&bytes, temp.path()),
            &scratch,
            Instant::now() + Duration::from_secs(10),
            SourceBuildLimits {
                max_entries: 2,
                ..Default::default()
            }
        )
        .is_err(),
        "implicit parent directories exceeded the real entry budget"
    );
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn s1_source_build_extracts_legacy_sdist_with_original_default_build_system() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let bytes = bundle(&[(
        "demo-1.0/setup.py",
        b"from setuptools import setup\nsetup(name='demo', version='1.0')\n",
    )]);
    let extracted = extract_source(
        &mut source(&bytes, temp.path()),
        &scratch,
        Instant::now() + Duration::from_secs(10),
        Default::default(),
    )
    .unwrap();
    assert_eq!(extracted.initial_requirements, ["setuptools"]);
    assert_eq!(extracted.backend, "setuptools.build_meta:__legacy__");
    assert_eq!(extracted.extracted_entries, 2);
    assert_eq!(
        extracted.extracted_path_bytes,
        "demo-1.0".len() + "demo-1.0/setup.py".len()
    );
    assert!(extracted.root.join("setup.py").is_file());
    let owned = extracted.workspace.path().to_owned();
    assert!(owned.starts_with(&scratch));
    drop(extracted);
    assert!(!owned.exists());
}

#[test]
fn s1_source_build_binds_declared_backend_path_inside_private_source() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let bytes = bundle(&[
        ("demo-1.0/pyproject.toml", b"[build-system]\nrequires=[]\nbuild-backend='demo_backend'\nbackend-path=['backend']\n"),
        ("demo-1.0/backend/demo_backend.py", b"def get_requires_for_build_wheel(config_settings=None): return []\n"),
    ]);
    let extracted = extract_source(
        &mut source(&bytes, temp.path()),
        &scratch,
        Instant::now() + Duration::from_secs(10),
        Default::default(),
    )
    .unwrap();
    assert!(extracted.initial_requirements.is_empty());
    assert_eq!(extracted.backend, "demo_backend");
    assert_eq!(extracted.backend_paths, [extracted.root.join("backend")]);
}

#[test]
fn s1_source_build_rejects_truncated_gzip_and_cleans_its_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let mut bytes = bundle(&[("demo-1.0/setup.py", b"pass\n")]);
    bytes.truncate(bytes.len() - 4);
    assert!(
        extract_source(
            &mut source(&bytes, temp.path()),
            &scratch,
            Instant::now() + Duration::from_secs(10),
            Default::default(),
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn s1_source_build_rejects_extra_bundle_roots_and_entry_budget_without_leaks() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    for (bytes, limits) in [
        (
            bundle(&[
                ("demo-1.0/setup.py", b"pass\n"),
                ("other-1.0/a.py", b"pass\n"),
            ]),
            SourceBuildLimits::default(),
        ),
        (
            bundle(&[
                ("demo-1.0/setup.py", b"pass\n"),
                ("demo-1.0/a.py", b"pass\n"),
            ]),
            SourceBuildLimits {
                max_entries: 1,
                ..Default::default()
            },
        ),
    ] {
        assert!(
            extract_source(
                &mut source(&bytes, temp.path()),
                &scratch,
                Instant::now() + Duration::from_secs(10),
                limits,
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }
}

#[test]
fn s1_source_build_rejects_raw_traversal_and_backend_path_escape_without_leaks() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let safe = bundle(&[("demo-1.0/setup.py", b"pass\n")]);
    for name in [
        "demo-1.0/../outside.py",
        "/outside.py",
        "demo-1.0\\outside.py",
    ] {
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(safe.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw[..100].fill(0);
        raw[..name.len()].copy_from_slice(name.as_bytes());
        raw[148..156].fill(b' ');
        let sum: u32 = raw[..512].iter().map(|value| *value as u32).sum();
        raw[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(
            extract_source(
                &mut source(&bytes, temp.path()),
                &scratch,
                Instant::now() + Duration::from_secs(10),
                Default::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        assert!(!temp.path().join("outside.py").exists());
    }
    let bytes = bundle(&[(
        "demo-1.0/pyproject.toml",
        b"[build-system]\nrequires=[]\nbuild-backend='demo_backend'\nbackend-path=['../outside']\n",
    )]);
    assert!(
        extract_source(
            &mut source(&bytes, temp.path()),
            &scratch,
            Instant::now() + Duration::from_secs(10),
            Default::default()
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

fn backend(optional: bool, mismatch: bool) -> String {
    let mut body = r#"import base64, hashlib, os, pathlib, zipfile
MISMATCH = False
META = b'Metadata-Version: 2.4\nName: demo\nVersion: 1.0\n\n'
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    metadata = META if not MISMATCH else b'Metadata-Version: 2.4\nName: demo\nVersion: 1.0\nRequires-Dist: unexpected\n\n'
    files = {'demo.py': b'VALUE=42\n', 'demo-1.0.dist-info/METADATA': metadata,
             'demo-1.0.dist-info/WHEEL': b'Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n'}
    record = ''.join(p+',sha256='+base64.urlsafe_b64encode(hashlib.sha256(v).digest()).rstrip(b'=').decode()+','+str(len(v))+'\n' for p,v in files.items())
    files['demo-1.0.dist-info/RECORD'] = (record+'demo-1.0.dist-info/RECORD,,\n').encode()
    filename = 'demo-1.0-py3-none-any.whl'
    with zipfile.ZipFile(pathlib.Path(wheel_directory)/filename, 'w') as z:
        for p,v in files.items():
            item=zipfile.ZipInfo(p)
            item.external_attr=0o100644 << 16
            z.writestr(item,v)
    return filename
"#.to_owned();
    if mismatch {
        body = body.replace("MISMATCH = False", "MISMATCH = True");
    }
    if optional {
        body.push_str(
            r#"
def get_requires_for_build_wheel(config_settings=None):
    assert 'PAKRO_SOURCE_BUILD_PRIVATE_SENTINEL' not in os.environ
    return []
def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None):
    directory=pathlib.Path(metadata_directory)/'demo-1.0.dist-info'
    directory.mkdir()
    (directory/'METADATA').write_bytes(META)
    return directory.name
"#,
        );
    }
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_source_build_real_hooks_optional_fallback_and_metadata_agreement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let scratch = root.join("builds");
    std::fs::create_dir(&scratch).unwrap();
    for (optional, mismatch, auxiliary) in [
        (true, false, false),
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let mut backend = backend(optional, mismatch);
        if auxiliary {
            backend = backend
                .replace(
                    "    (directory/'METADATA').write_bytes(META)",
                    "    (directory/'METADATA').write_bytes(META)\n    (directory/'backend-build-plan').write_bytes(b'preserved-plan')",
                )
                .replace(
                    "    metadata = META if not MISMATCH",
                    "    assert (pathlib.Path(metadata_directory)/'backend-build-plan').read_bytes() == b'preserved-plan'\n    metadata = META if not MISMATCH",
                );
        }
        let bytes = bundle(&[
            ("demo-1.0/pyproject.toml", b"[build-system]\nrequires=[]\nbuild-backend='demo_backend'\nbackend-path=['backend']\n"),
            ("demo-1.0/backend/demo_backend.py", backend.as_bytes()),
        ]);
        let mut session = SourceBuildSession::prepare(
            source(&bytes, &root),
            "demo",
            "1.0",
            language.clone(),
            &scratch,
            context.policy().deadline,
            Default::default(),
        )
        .unwrap();
        assert!(session.initial_requirements().is_empty());
        assert!(session.get_requires(Vec::new()).await.unwrap().is_empty());
        let result = session.build(Vec::new()).await;
        if mismatch {
            assert!(result.is_err(), "changed metadata cannot mint completion");
        } else {
            let completed = result.unwrap();
            completed.recheck().unwrap();
            let (wheel, record) = completed.into_parts().unwrap();
            assert_eq!(wheel.manifest().name(), "demo");
            assert_eq!(wheel.manifest().version(), "1.0");
            assert_eq!(record.output_filename, "demo-1.0-py3-none-any.whl");
            assert_eq!(record.output_digest, wheel.digest().to_string());
        }
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        language.check_binding().unwrap();
    }
}

#[test]
#[cfg(unix)]
fn s1_source_build_output_readers_reject_parent_directory_aliases() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let reader = archive::WorkspaceReader::open(&workspace).unwrap();
    std::fs::write(outside.join("demo-1.0-py3-none-any.whl"), b"owned-wheel").unwrap();
    std::fs::write(outside.join("result.json"), b"{\"ok\":true}").unwrap();
    let metadata = outside.join("demo-1.0.dist-info");
    std::fs::create_dir(&metadata).unwrap();
    std::fs::write(metadata.join("METADATA"), b"owned-metadata").unwrap();
    for name in ["output", "response", "metadata"] {
        symlink(&outside, workspace.join(name)).unwrap();
    }
    let mut followed = Vec::new();
    if open_output(
        &reader,
        &workspace.join("output/demo-1.0-py3-none-any.whl"),
        1024,
    )
    .is_ok()
    {
        followed.push("wheel");
    }
    if archive::read_bounded(&reader, &workspace.join("response/result.json"), 1024).is_ok() {
        followed.push("response");
    }
    if capture_metadata(
        &reader,
        &workspace.join("metadata/demo-1.0.dist-info"),
        Instant::now() + Duration::from_secs(5),
        Default::default(),
    )
    .is_ok()
    {
        followed.push("metadata");
    }
    assert!(
        followed.is_empty(),
        "source output readers followed owned outside parent aliases: {followed:?}"
    );
}

#[test]
#[cfg(unix)]
fn s1_source_build_cleanup_does_not_chmod_a_swapped_outside_alias() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let child = workspace.join("nested");
    let outside = root.join("outside");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(target_os = "macos")]
    {
        std::fs::create_dir_all(workspace.join("sealed/nested")).unwrap();
        for path in [workspace.join("sealed/nested"), workspace.join("sealed")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0)).unwrap();
        }
    }
    let reader = archive::WorkspaceReader::open(&workspace).unwrap();
    reader
        .restore_directory_permissions(100, |directory| {
            if directory == child {
                std::fs::remove_dir(&child).unwrap();
                symlink(&outside, &child).unwrap();
            }
        })
        .unwrap();
    assert_eq!(
        std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o750
    );
    #[cfg(target_os = "macos")]
    for path in [workspace.join("sealed"), workspace.join("sealed/nested")] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
#[cfg(unix)]
fn s1_source_build_workspace_monitor_does_not_follow_swapped_directory() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let child = root.join("nested");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("must-not-be-observed"), [1; 32]).unwrap();
    let result = worker::check_workspace_with_hook(
        &root,
        SourceBuildLimits {
            max_workspace_bytes: 16,
            ..Default::default()
        },
        Instant::now() + Duration::from_secs(5),
        |directory| {
            if directory == child {
                std::fs::remove_dir(&child)?;
                symlink(&outside, &child)?;
            }
            Ok(())
        },
    );
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("directory changed"),
        "the live monitor traversed an outside alias: {error:#}"
    );
}

fn counter_dependency(
    original: &[u8],
    archive: InspectedWheelArchive,
) -> (SourceBuildInput, InspectedWheelArchive) {
    use crate::packages::python_resolution::lock_model::{
        ContentPin, PinEvidence, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    };
    let source_digest = pakro_store::Digest::from_sha256_bytes(original);
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "public",
            canonical_base_fingerprint: [9; 32],
            project: "counter-dep",
            version: "1.0",
            filename: "counter_dep-1.0.tar.gz",
            kind: RegistryArtifactKind::Sdist,
        },
        Default::default(),
    )
    .unwrap();
    let pin = ContentPin::new(
        &file,
        source_digest,
        original.len() as u64,
        PinEvidence::ObservedDuringFreshResolution,
        Default::default(),
    )
    .unwrap();
    (
        SourceBuildInput {
            name: "counter-dep".into(),
            version: "1.0".into(),
            source_pin: hex::encode(pin.id().as_bytes()),
            registry: "public".into(),
            base: [9; 32],
            source_filename: "counter_dep-1.0.tar.gz".into(),
            source_digest: source_digest.to_string(),
            source_size: original.len() as u64,
            source_is_sdist: true,
            advertised: false,
            output_filename: archive.filename().into(),
            output_digest: archive.digest().to_string(),
            output_size: archive.size(),
        },
        archive,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_source_build_reuses_exact_initial_environment_without_resolving_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = crate::libraries::package::layouts::tests::real_language(&context).await;
    let scratch = root.join("builds");
    std::fs::create_dir(&scratch).unwrap();
    let counter_backend = format!(
        "import time\nNONCE = str(time.time_ns())\n{}",
        backend(true, false)
            .replace("demo", "counter_dep")
            .replace("b'VALUE=42\\n'", "('VALUE='+repr(NONCE)+'\\n').encode()")
    );
    let original = bundle(&[
        ("counter_dep-1.0/pyproject.toml",b"[build-system]\nrequires=[]\nbuild-backend='fixture_backend'\nbackend-path=['backend']\n"),
        ("counter_dep-1.0/backend/fixture_backend.py",counter_backend.as_bytes()),
    ]);
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let mut build = SourceBuildSession::prepare(
            source_named(&original, "counter_dep-1.0.tar.gz", &root),
            "counter-dep",
            "1.0",
            language.clone(),
            &scratch,
            context.policy().deadline,
            Default::default(),
        )
        .unwrap();
        assert!(build.get_requires(Vec::new()).await.unwrap().is_empty());
        let completed = build.build(Vec::new()).await.unwrap();
        assert_eq!(
            completed.record().source_digest,
            pakro_store::Digest::from_sha256_bytes(&original).to_string()
        );
        outputs.push(completed);
    }
    let first = outputs.remove(0);
    let second = outputs.remove(0);
    assert_ne!(first.record().output_digest, second.record().output_digest);
    assert_eq!(first.record().request_id, second.record().request_id);
    let expected_record = first.record().clone();
    let (first, duplicate) = first
        .duplicate_for_attempt(
            &scratch,
            context.policy().deadline,
            256 * 1024 * 1024,
            512 * 1024 * 1024,
        )
        .unwrap();
    first.recheck().unwrap();
    duplicate.recheck().unwrap();
    assert_eq!(first.record(), &expected_record);
    assert_eq!(duplicate.record(), &expected_record);
    let (duplicate_archive, duplicate_record) = duplicate.into_parts().unwrap();
    assert_eq!(duplicate_record, expected_record);
    assert_eq!(
        duplicate_archive.digest().to_string(),
        expected_record.output_digest
    );
    drop(duplicate_archive);
    let (first_archive, _) = first.into_parts().unwrap();
    let (second_archive, _) = second.into_parts().unwrap();
    let (first_input, first_archive) = counter_dependency(&original, first_archive);
    let (second_input, second_archive) = counter_dependency(&original, second_archive);
    assert_eq!(first_input.source_pin, second_input.source_pin);
    assert_ne!(first_input.output_digest, second_input.output_digest);
    let first_value = first_archive
        .manifest()
        .members()
        .iter()
        .find(|member| member.archive_path() == "counter_dep.py")
        .unwrap()
        .archive_index();
    let mut first_archive = first_archive;
    let module = first_archive
        .with_members(|members| members.read_member(first_value, 1024))
        .unwrap();
    let expected = std::str::from_utf8(&module)
        .unwrap()
        .trim()
        .strip_prefix("VALUE=")
        .unwrap();
    let backend = format!(
        "import counter_dep\nassert counter_dep.VALUE == {expected}\n{}",
        backend(true, false)
    );
    let bytes = bundle(&[
        ("demo-1.0/pyproject.toml",b"[build-system]\nrequires=['counter-dep==1.0']\nbuild-backend='demo_backend'\nbackend-path=['backend']\n"),
        ("demo-1.0/backend/demo_backend.py",backend.as_bytes()),
    ]);
    let mut session = SourceBuildSession::prepare(
        source(&bytes, &root),
        "demo",
        "1.0",
        language.clone(),
        &scratch,
        context.policy().deadline,
        Default::default(),
    )
    .unwrap();
    assert!(
        session
            .get_requires(vec![(first_input.clone(), first_archive)])
            .await
            .unwrap()
            .is_empty()
    );
    // There is deliberately no second dependency argument to this API. A
    // nondeterministic second build must not replace the inputs just observed.
    drop(second_archive);
    let completed = session.build_with_initial_environment().await.unwrap();
    assert_eq!(completed.record().inputs, [first_input]);
    drop(completed);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    language.check_binding().unwrap();

    // Materialization must account for already-held source and environments,
    // before allocating another prefix with its own independent byte limit.
    let limit = 1024 * 1024;
    let bounded = SourceBuildSession::prepare(
        source(&bytes, &root),
        "demo",
        "1.0",
        language.clone(),
        &scratch,
        context.policy().deadline,
        SourceBuildLimits {
            max_workspace_bytes: limit,
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(
        bounded.workspace_path().join("source/owned-ballast"),
        vec![0; limit as usize],
    )
    .unwrap();
    assert!(
        bounded.environment(Vec::new()).is_err(),
        "a full existing workspace must reject a new build environment"
    );
    assert_eq!(
        std::fs::read_dir(bounded.workspace_path()).unwrap().count(),
        1,
        "workspace budget must be checked before creating a deps store"
    );
    drop(bounded);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}
