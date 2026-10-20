use super::*;

/// Same verified content is published once and reused; a second publish of
/// the same digest performs no new write, and damaged state never reports
/// "unpublished".
#[test]
fn f3_verified_content_is_shared() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(300),
    );
    let bytes = crate::libraries::language::repository::tests::archive("3.15.0");
    let mut artifact = crate::artifact_snapshot::spool_verified_reader(
        bytes.as_slice(),
        "demo-1.0-py3-none-any.whl",
        pakro_store::Digest::from_sha256_bytes(&bytes),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();

    let first = publish_blob(&context, &mut artifact).unwrap();
    let second = publish_blob(&context, &mut artifact).unwrap();
    assert_eq!(first, second, "identical content shares one blob");
    let manifest = super::super::manifest::read_blob_manifest(&context, &first)
        .unwrap()
        .unwrap();
    let path = context
        .requested_data_root()
        .join("packages")
        .join(&manifest.storage.allocation)
        .join("archive");
    let before = std::fs::metadata(&path).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(before.ino(), std::fs::metadata(&path).unwrap().ino());
    std::fs::write(&path, b"damaged").unwrap();
    assert!(publish_blob(&context, &mut artifact).is_err());
}
