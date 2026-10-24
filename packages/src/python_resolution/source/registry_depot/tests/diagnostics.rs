//! Failures retain safe stage/category facts, never peer-controlled locators.
use super::*;

const PRIVATE_QUERY: &str = "private-query-secret";
const PRIVATE_BODY: &str = "private-error-body-secret";

async fn failure(case: u8) -> (RegistryDepotError, Vec<String>) {
    let declarations = if case == 4 {
        vec!["first-dep", "second-dep"]
    } else {
        Vec::new()
    };
    let archive = wheel("demo_pkg", "1.0", &declarations, "");
    let sidecar = b"Metadata-Version: 2.6\nName: demo_pkg\nVersion: 1.1\n";
    let peer = Peer::new(move |target| {
        if target == "/simple/demo-pkg/" {
            let mut file = wire_file(DEMO);
            file["url"] = serde_json::json!(format!("/files/{DEMO}?token={PRIVATE_QUERY}"));
            if case == 1 {
                file["hashes"] = serde_json::json!({"sha256": "00".repeat(32)});
            }
            if case == 3 {
                file["core-metadata"] =
                    serde_json::json!({"sha256": Digest::from_sha256_bytes(sidecar).hex()});
            }
            Reply::page("demo-pkg", vec![file])
        } else if target.starts_with(&format!("/files/{DEMO}.metadata?")) {
            Reply::file(sidecar)
        } else if target.starts_with(&format!("/files/{DEMO}?")) {
            match case {
                0 => Reply {
                    status: 503,
                    media: None,
                    body: PRIVATE_BODY.as_bytes().to_vec(),
                },
                2 => Reply::file(PRIVATE_BODY.as_bytes()),
                _ => Reply::file(&archive),
            }
        } else {
            Reply::missing()
        }
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let mut context = fixture.context(&endpoints, &policy);
    if case == 4 {
        context.limits.input.max_dependencies = 1;
    }
    let error = ResolveRegistryDepot::new(&root, &["demo-pkg==1.0"], context)
        .unwrap()
        .prepare()
        .await
        .expect_err("the actual registry operation must fail");
    fixture.no_publish();
    let requests = peer.requests();
    assert!(requests.iter().any(|target| target.contains(PRIVATE_QUERY)));
    for rendered in [error.to_string(), format!("{error:?}")] {
        for secret in [PRIVATE_QUERY, PRIVATE_BODY, &peer.url] {
            assert!(
                !rendered.contains(secret),
                "registry diagnostics exposed a locator or peer-controlled body: {rendered}"
            );
        }
    }
    (error, requests)
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_http_status_without_locators() {
    let (error, requests) = failure(0).await;
    assert_eq!(requests.len(), 2);
    let message = error.to_string();
    assert!(
        message.contains("registry file acquisition failed"),
        "{message}"
    );
    assert!(
        message.contains("HTTP status is not successful: 503"),
        "{message}"
    );
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_project_http_status_without_locators() {
    let peer = Peer::new(|_| Reply {
        status: 503,
        media: None,
        body: PRIVATE_BODY.as_bytes().to_vec(),
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let error = ResolveRegistryDepot::new(
        &root,
        &["demo-pkg==1.0"],
        fixture.context(&endpoints, &policy),
    )
    .unwrap()
    .prepare()
    .await
    .expect_err("the actual project HTTP response must fail");
    fixture.no_publish();
    assert_eq!(peer.requests().len(), 1);
    let message = error.to_string();
    assert!(
        message.contains("registry project acquisition failed"),
        "{message}"
    );
    assert!(
        message.contains("HTTP status is not successful: 503"),
        "{message}"
    );
    for rendered in [message, format!("{error:?}")] {
        for secret in [PRIVATE_BODY, &peer.url] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
    }
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_advertised_digest_failure() {
    let (error, requests) = failure(1).await;
    assert_eq!(requests.len(), 2);
    let message = error.to_string();
    assert!(
        message.contains("registry file acquisition failed"),
        "{message}"
    );
    assert!(message.contains("bytes could not be verified"), "{message}");
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_wheel_inspection_failure() {
    let (error, requests) = failure(2).await;
    assert_eq!(requests.len(), 2);
    let message = error.to_string();
    assert!(
        message.contains("registry wheel inspection failed"),
        "{message}"
    );
    assert!(message.contains("archive inspection failed"), "{message}");
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_sidecar_mismatch() {
    let (error, requests) = failure(3).await;
    assert_eq!(requests.len(), 3, "the real sidecar must be fetched");
    let message = error.to_string();
    assert!(
        message.contains("registry advertised metadata verification failed"),
        "{message}"
    );
    assert!(
        message.contains("differs from the complete actual wheel metadata"),
        "{message}"
    );
}

#[tokio::test]
async fn f3_registry_diagnostics_preserve_metadata_input_failure() {
    let (error, requests) = failure(4).await;
    assert_eq!(requests.len(), 2);
    let message = error.to_string();
    assert!(
        message.contains("registry dependency metadata compilation failed"),
        "{message}"
    );
    assert!(
        message.contains("metadata input budget exceeded"),
        "{message}"
    );
}

#[test]
fn f3_registry_diagnostics_keep_original_deadline_priority() {
    assert_eq!(
        stage_error(
            Instant::now() - Duration::from_secs(1),
            RegistryDepotError::NoSolution
        ),
        RegistryDepotError::Deadline
    );
    assert_eq!(
        stage_error(
            Instant::now() + Duration::from_secs(5),
            RegistryDepotError::NoSolution
        ),
        RegistryDepotError::NoSolution
    );
}
