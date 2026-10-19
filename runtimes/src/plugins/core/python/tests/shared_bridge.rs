use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

#[tokio::test]
async fn r06_compatibility_directory_only_references_one_payload() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let compatibility = root.join("compatibility");
    publish_native_compatibility(&lease, &compatibility, &context).unwrap();
    assert!(std::fs::symlink_metadata(&compatibility).unwrap().is_dir());
    assert!(
        std::fs::symlink_metadata(compatibility.join("bin"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let actual = std::fs::metadata(lease.entrypoint()).unwrap();
    let alias = std::fs::metadata(compatibility.join("bin/python")).unwrap();
    assert_eq!((actual.dev(), actual.ino()), (alias.dev(), alias.ino()));
    std::fs::write(compatibility.join("backend-receipt"), b"outer metadata").unwrap();
    assert!(!lease.root().join("backend-receipt").exists());
    lease.check_binding().unwrap();
}

#[tokio::test]
async fn r06_compatibility_conflict_never_replaces_existing_content() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let compatibility = root.join("compatibility");
    std::fs::create_dir_all(compatibility.join("bin")).unwrap();
    std::fs::write(compatibility.join("bin/sentinel"), b"do not overwrite").unwrap();
    assert!(publish_native_compatibility(&lease, &compatibility, &context).is_err());
    assert_eq!(
        std::fs::read(compatibility.join("bin/sentinel")).unwrap(),
        b"do not overwrite"
    );
    lease.check_binding().unwrap();
}

#[tokio::test]
async fn r06_compatibility_rejects_a_root_alias_to_published_content() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let compatibility = root.join("compatibility");
    std::os::unix::fs::symlink(lease.root(), &compatibility).unwrap();
    assert!(publish_native_compatibility(&lease, &compatibility, &context).is_err());
    assert_eq!(std::fs::read_link(&compatibility).unwrap(), lease.root());
    lease.check_binding().unwrap();
}

#[tokio::test]
async fn r06_native_source_selection_keeps_the_frozen_settings() {
    use crate::config::settings::SettingsPartial;
    use confique::Layer;
    let _guard = crate::test::SettingsGuard::lock();
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let mut changed = SettingsPartial::empty();
    changed.offline = Some(true);
    changed.python.precompiled_flavor = Some("freethreaded-install_only".into());
    Settings::reset(Some(changed));
    let backend = PythonPlugin::new();
    let mut version = crate::backend::static_helpers::test_tool_version(&backend, "3.15.0", &[]);
    let target = PlatformTarget::new(Platform::parse("macos-arm64").unwrap());
    let plan = install_spec(&archive("3.15.0"));
    version.lock_platforms.insert(
        target.to_key(),
        PlatformInfo {
            url: Some(plan.plan().source().url().to_string()),
            checksum: Some(
                plan.plan()
                    .source()
                    .digest()
                    .to_string()
                    .replacen('-', ":", 1),
            ),
            ..Default::default()
        },
    );
    let selected = backend
        .native_plan_in_context(&version, &target, &context)
        .await
        .unwrap();
    assert_eq!(
        selected.flavor(),
        native::RuntimeFlavor::InstallOnly,
        "a later global settings change must not alter this invocation's selected artifact"
    );
}

#[test]
fn r06_native_archive_snapshot_reads_the_held_inode_after_path_replacement() {
    use std::io::{Read, Seek, Write};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let mut temporary = tempfile::NamedTempFile::new_in(&root).unwrap();
    temporary.write_all(&bytes).unwrap();
    temporary.flush().unwrap();
    std::fs::rename(temporary.path(), root.join("held-original")).unwrap();
    std::fs::write(temporary.path(), b"replacement must not be read").unwrap();
    let mut captured =
        spool_native_archive(&mut temporary, spec.plan().source(), &context).unwrap();
    captured.rewind().unwrap();
    let mut actual = Vec::new();
    captured.read_to_end(&mut actual).unwrap();
    assert_eq!(
        actual, bytes,
        "verified archive bytes must come from the retained file"
    );
}

#[test]
fn r06_native_archive_snapshot_never_cleans_up_a_replacement_inode() {
    use std::io::Write;
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let mut temporary = tempfile::NamedTempFile::new_in(&root).unwrap();
    temporary.write_all(&bytes).unwrap();
    temporary.flush().unwrap();
    let path = temporary.path().to_path_buf();
    std::fs::rename(&path, root.join("held-original")).unwrap();
    std::fs::write(&path, b"replacement must survive").unwrap();
    let _ = spool_native_archive(&mut temporary, spec.plan().source(), &context);
    drop(temporary);
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"replacement must survive",
        "the private snapshot must not unlink an unowned replacement"
    );
}

#[tokio::test]
async fn r06_native_archive_private_file_stays_in_the_frozen_cache_root() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let original = operation_context(&root, true, Duration::from_secs(10));
    let mut settings = Settings::default();
    settings.http_timeout = "5s".into();
    settings.http_download_timeout = "10s".into();
    let context =
        crate::programs::capture_operation_context(crate::programs::OperationContextInputs {
            settings: &settings,
            policy: *original.policy(),
            cancellation: original.cancellation_token(),
            data_root: original.requested_data_root().to_path_buf(),
            cache_root: original.requested_cache_root().to_path_buf(),
            python_sources: None,
            python_target_description: None,
        })
        .unwrap();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let mut verified = crate::artifact_snapshot::spool_verified_reader(
        bytes.as_slice(),
        spec.plan().source().filename(),
        spec.plan().source().digest(),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();
    crate::artifact_snapshot::publish_verified_artifact(
        &mut verified,
        &context.requested_cache_root().join("python/native-v1"),
        1024 * 1024,
    )
    .unwrap();
    let private = PythonPlugin::new()
        .native_archive_in_context(spec.plan(), &context)
        .await
        .unwrap();
    assert!(
        private.path().starts_with(context.requested_cache_root()),
        "a runtime archive must not be staged outside this invocation's controlled cache"
    );
}

#[test]
fn r06_native_provenance_sha256_uses_verified_bytes_and_rewinds_both_digest_kinds() {
    use std::io::{Read, Seek, SeekFrom};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = b"immutable expected runtime archive bytes";
    let pakro_store::Digest::Sha256(wanted) = pakro_store::Digest::from_sha256_bytes(bytes) else {
        unreachable!()
    };
    for expected in [
        pakro_store::Digest::from_sha256_bytes(bytes),
        pakro_store::Digest::from_sha512_bytes(bytes),
    ] {
        let mut artifact = crate::artifact_snapshot::spool_verified_reader(
            bytes.as_slice(),
            "held.tar.gz",
            expected,
            1024,
            context.requested_cache_root(),
        )
        .unwrap();
        artifact.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(
            native_artifact_sha256(&mut artifact, &context).unwrap(),
            wanted
        );
        let mut reread = Vec::new();
        artifact.read_to_end(&mut reread).unwrap();
        assert_eq!(
            reread, bytes,
            "provenance hashing must return the same held artifact at offset zero"
        );
    }
}
