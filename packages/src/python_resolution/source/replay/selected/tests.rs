use super::*;
use crate::packages::python_resolution::lock_model::*;
use crate::packages::python_resolution::lock_session::{FilePinLookup, ResolutionIntent};
use std::fs;
use std::time::Duration;
#[path = "../tests/support.rs"]
mod support;
use support::*;

async fn observed(fixture: &Fixture) -> BoundRegistryProject {
    let context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    let RegistryProjectLookup::Present(project) = fetch_registry_project(
        context.client,
        context.project_session,
        &NormalizedPythonName::new("demo-pkg").unwrap(),
        context.policy,
        context.project_limits,
    )
    .await
    .expect("actual Simple observation prelude") else {
        panic!("actual project must exist");
    };
    project
        .select_exact_file(FILENAME)
        .expect("actual file selection prelude");
    project
}

#[tokio::test]
async fn cold_existing_selection_uses_the_original_observation_and_no_second_simple() {
    let files = Peer::new(|_| Reply::file(PAYLOAD));
    let body = page(vec![file_json(
        FILENAME,
        &format!("{}/exact", files.url),
        serde_json::json!({}),
    )]);
    let index = index(body);
    let fixture = Fixture::new(&index);
    let project = observed(&fixture).await;
    let selected = project.select_exact_file(FILENAME).unwrap();
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
    );
    let mut actual = acquire_selected_existing_registry_file(
        slot(&lock, &file),
        &selected,
        fixture.context(NetworkPolicy::Allowed, &ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(read(&mut actual), PAYLOAD);
    assert_eq!(actual.origin(), LockedReplayOrigin::Registry);
    let ReplayProvenance::Registry(identity) = &actual.provenance else {
        panic!("cold bytes need actual registry provenance");
    };
    assert!(Arc::ptr_eq(&identity.project, &project.data));
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn an_observed_existing_selection_can_reuse_expected_bytes_without_another_request() {
    let index = Peer::new(|target| {
        if target == "/simple/demo-pkg/" {
            Reply::page(page(vec![file_json(
                FILENAME,
                "/never-file",
                serde_json::json!({}),
            )]))
        } else {
            Reply::denied()
        }
    });
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    fixture.seed_cache(FILENAME, digest, PAYLOAD);
    // The original cache-first entry point still needs no project observation
    // or router. The selected path below additionally retains its real page.
    let mut standalone = acquire_locked_registry_file(
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Offline, &NO_ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(read(&mut standalone), PAYLOAD);
    assert!(index.requests().is_empty());
    let project = observed(&fixture).await;
    let selected = project.select_exact_file(FILENAME).unwrap();
    let mut actual = acquire_selected_existing_registry_file(
        slot(&lock, &file),
        &selected,
        fixture.context(NetworkPolicy::Offline, &ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(read(&mut actual), PAYLOAD);
    assert_eq!(actual.origin(), LockedReplayOrigin::Cache);
    assert_eq!(index.requests().len(), 1);
    assert!(!fixture.scratch.exists());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn another_auth_scope_cannot_rebind_an_actual_project_even_when_expected_cache_is_warm() {
    let index = index(page(vec![file_json(
        FILENAME,
        "/never-file",
        serde_json::json!({}),
    )]));
    let fixture = Fixture::new(&index);
    let project = observed(&fixture).await;
    let selected = project.select_exact_file(FILENAME).unwrap();
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    fixture.seed_cache(FILENAME, digest, PAYLOAD);
    let scope = AuthScopeKey::from_local_revision([9; 32]);
    let mut context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    context.project_session.auth_scope = &scope;
    assert_eq!(
        acquire_selected_existing_registry_file(slot(&lock, &file), &selected, context)
            .await
            .unwrap_err(),
        LockedReplayError::ProjectContext
    );
    assert_eq!(index.requests().len(), 1);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn expected_selection_honors_actual_file_snapshot_and_original_deadline() {
    let index = index(page(vec![
        file_json(FILENAME, "/never-file", serde_json::json!({})),
        file_json(OTHER, "/never-other", serde_json::json!({})),
    ]));
    let fixture = Fixture::new(&index);
    let project = observed(&fixture).await;
    let selected = project.select_exact_file(OTHER).unwrap();
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
    );
    assert_eq!(
        acquire_selected_existing_registry_file(
            slot(&lock, &file),
            &selected,
            fixture.context(NetworkPolicy::Allowed, &ROUTE)
        )
        .await
        .unwrap_err(),
        LockedReplayError::FileIdentity
    );
    let selected = project.select_exact_file(FILENAME).unwrap();
    let mut context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    context.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        acquire_selected_existing_registry_file(slot(&lock, &file), &selected, context)
            .await
            .unwrap_err(),
        LockedReplayError::Deadline
    );
    let FilePinLookup::Existing(held) = lock.lookup(&file, ResolutionIntent::Locked).unwrap()
    else {
        panic!("actual slot prelude");
    };
    fs::write(
        &fixture.lock_path,
        b"# private external replacement\nlockfile_version=4\n",
    )
    .unwrap();
    assert_eq!(
        acquire_selected_existing_registry_file(
            held,
            &selected,
            fixture.context(NetworkPolicy::Allowed, &ROUTE)
        )
        .await
        .unwrap_err(),
        LockedReplayError::Revoked
    );
    assert_eq!(index.requests().len(), 1);
    assert!(!fixture.cache.exists());
    assert!(!fixture.scratch.exists());
    fixture.assert_no_authority_writes();
}
