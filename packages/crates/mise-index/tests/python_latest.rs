use mise_index::{Index, IndexError, PythonMarkerContext, PythonMarkerContextBuilder};
use serde_json::json;

fn context(full: &str) -> PythonMarkerContext {
    PythonMarkerContext::new(PythonMarkerContextBuilder {
        implementation_name: "cpython",
        implementation_version: full,
        os_name: "posix",
        platform_machine: "x86_64",
        platform_python_implementation: "CPython",
        platform_release: "6.8.12-fixture",
        platform_system: "Linux",
        platform_version: "fixture kernel version",
        python_full_version: full,
        python_version: "3.10",
        sys_platform: "linux",
    })
    .unwrap()
}

#[tokio::test]
async fn stable_candidates_use_pep440_descending_order() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.2": [{"requires_python": null}],
            "1.9": [{"requires_python": null}],
            "1.10": [{"requires_python": null}],
            "2.0": [{"requires_python": null}]
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert_eq!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        vec!["2.0", "1.10", "1.9", "1.2"]
    );
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        Some("2.0".to_string())
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn prerelease_and_dev_are_excluded_but_post_raw_names_survive() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.0": [{"requires_python": null}],
            "1.0-1": [{"requires_python": null}],
            "1.0.post2": [{"requires_python": null}],
            "2.0a1": [{"requires_python": null}],
            "2.0b1": [{"requires_python": null}],
            "2.0rc1": [{"requires_python": null}],
            "2.0.dev1": [{"requires_python": null}],
            "not-a-version": [{"requires_python": null}]
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert_eq!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        vec!["1.0.post2", "1.0-1", "1.0"]
    );
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        Some("1.0.post2".to_string())
    );
    assert_eq!(
        index.latest_stable_python("candidate-demo").await.unwrap(),
        Some("1.0.post2".to_string())
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn incompatible_stable_release_never_becomes_a_raw_fallback() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.0": [{"requires_python": ">=3.11"}],
            "99.0": [{"requires_python": ">=3.12"}]
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        index.latest_stable_python("candidate-demo").await.unwrap(),
        Some("99.0".to_string())
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn no_stable_candidates_returns_none() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.0a1": [{"requires_python": null}],
            "1.0b1": [{"requires_python": null}],
            "1.0rc1": [{"requires_python": null}],
            "1.0.dev1": [{"requires_python": null}],
            "broken": [{"requires_python": null}],
            "2.0": []
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await
            .unwrap(),
        None
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn latest_re_evaluates_cached_listing_for_each_full_patch() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.0": [{"requires_python": "==3.10.13"}],
            "2.0": [{"requires_python": ">=3.10.14"}]
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &context("3.10.13"))
            .await
            .unwrap(),
        Some("1.0".to_string())
    );
    assert_eq!(
        index
            .latest_stable_python_for_context("candidate-demo", &context("3.10.14"))
            .await
            .unwrap(),
        Some("2.0".to_string())
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn invalid_file_constraint_stays_an_error_in_both_context_apis() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "info": {"name": "candidate-demo"},
        "releases": {
            "1.0": [{"requires_python": null}],
            "2.0": [{"requires_python": null}, {"requires_python": "not-a-spec"}]
        }
    });
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert!(matches!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await,
        Err(IndexError::InvalidPythonSpecifier { .. })
    ));
    assert!(matches!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await,
        Err(IndexError::InvalidPythonSpecifier { .. })
    ));
    mock.assert_async().await;
}

#[tokio::test]
async fn malformed_response_keeps_its_error_class() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/candidate-demo/json")
        .with_status(200)
        .with_body(json!({"info": {"name": "candidate-demo"}}).to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let target = context("3.10.14");
    assert!(matches!(
        index
            .stable_python_versions_for_context("candidate-demo", &target)
            .await,
        Err(IndexError::BadResponse { .. })
    ));
    assert!(matches!(
        index
            .latest_stable_python_for_context("candidate-demo", &target)
            .await,
        Err(IndexError::BadResponse { .. })
    ));
    mock.assert_async().await;
}
