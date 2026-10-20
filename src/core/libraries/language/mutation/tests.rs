use super::*;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::language::repository::{LanguageRepository, LanguageState};
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn f2_legacy_creation_inside_shared_library_is_rejected() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    assert!(
        reject_legacy_mutation(
            &root,
            LegacyMutation::Force,
            &root.join("languages/not-created/payload"),
        )
        .is_err()
    );
    assert!(!root.join("languages").exists());
}

#[cfg(unix)]
#[test]
fn f2_parent_components_cannot_hide_a_shared_target() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("languages/nested")).unwrap();
    std::fs::create_dir(root.join("outside")).unwrap();
    std::os::unix::fs::symlink(root.join("languages/nested"), root.join("outside/alias")).unwrap();
    assert!(
        reject_legacy_mutation(
            &root,
            LegacyMutation::Force,
            &root.join("outside/alias/../not-created")
        )
        .is_err()
    );
    assert!(!root.join("languages/not-created").exists());
}

/// Every legacy mutation kind is refused on the shared payload before any
/// effect, while paths outside the shared library stay untouched by the gate.
#[tokio::test]
async fn f2_legacy_mutation_is_rejected_before_effects() {
    let owner = tempfile::tempdir().unwrap();
    let stage: PathBuf = owner.path().canonicalize().unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(300));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let shared_payload = lease.root().to_path_buf();
    drop(lease);

    for mutation in [
        LegacyMutation::Force,
        LegacyMutation::Repair,
        LegacyMutation::PostInstall,
        LegacyMutation::Uninstall,
        LegacyMutation::Purge,
        LegacyMutation::Rollback,
        LegacyMutation::DelayedCleanup,
    ] {
        assert!(
            reject_legacy_mutation(context.requested_data_root(), mutation, &shared_payload)
                .is_err(),
            "{mutation:?} must be rejected against the shared payload"
        );
    }
    let outside = owner.path().join("legacy-install");
    std::fs::create_dir(&outside).unwrap();
    for mutation in [
        LegacyMutation::Force,
        LegacyMutation::Uninstall,
        LegacyMutation::Purge,
        LegacyMutation::Rollback,
    ] {
        assert!(
            reject_legacy_mutation(context.requested_data_root(), mutation, &outside).is_ok(),
            "{mutation:?} outside the shared library keeps its existing behavior"
        );
    }

    // Nothing above changed the published instance: still Ready and complete.
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    match repository.lookup(&spec).unwrap() {
        LanguageState::Ready(found) => found.check_binding().unwrap(),
        LanguageState::Missing => panic!("the shared instance disappeared after refusals"),
    }
}
