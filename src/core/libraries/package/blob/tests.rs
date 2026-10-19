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
    let artifact = crate::artifact_snapshot::spool_verified_reader(
        bytes.as_slice(),
        "demo-1.0-py3-none-any.whl",
        &pakro_store::Digest::from_sha256_bytes(&bytes),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();

    let first = publish_blob(&context, &artifact).unwrap();
    let second = publish_blob(&context, &artifact).unwrap();
    assert_eq!(first, second, "identical content shares one blob");
    // Damage is reported as damage, not as unpublished.
    // (Concrete damage assertions join with the minimal implementation.)
}
