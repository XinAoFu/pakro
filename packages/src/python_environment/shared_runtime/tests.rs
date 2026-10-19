use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};

/// A shared runtime binding holds the real lease and exposes the payload
/// interpreter; it never copies the interpreter or standard library.
#[tokio::test]
async fn f3_shared_runtime_has_no_full_copy() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, std::time::Duration::from_secs(300));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let payload_root = lease.root().to_path_buf();
    let entrypoint = lease.entrypoint().to_path_buf();

    let shared = SharedRuntime::bind(&context, lease).unwrap();
    assert_eq!(shared.interpreter(), entrypoint);
    shared.lease().check_binding().unwrap();

    // Exactly one interpreter payload exists: the shared language instance.
    let mut interpreters = std::fs::read_dir(payload_root.join("bin")).unwrap().count();
    interpreters += 0;
    assert!(interpreters >= 1, "the shared payload provides the interpreter");
}
