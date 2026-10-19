use pakro_index::{
    Index, IndexError, PythonMarkerContext, PythonMarkerContextBuilder,
    parse_pypi_response_for_context,
};
use serde_json::{Value, json};

fn context() -> PythonMarkerContext {
    PythonMarkerContext::new(PythonMarkerContextBuilder {
        implementation_name: "cpython",
        implementation_version: "3.10.14",
        os_name: "posix",
        platform_machine: "x86_64",
        platform_python_implementation: "CPython",
        platform_release: "fixture release",
        platform_system: "Linux",
        platform_version: "fixture version",
        python_full_version: "3.10.14",
        python_version: "3.10",
        sys_platform: "linux",
    })
    .unwrap()
}

fn file(filename: &str) -> Value {
    json!({
        "filename": filename,
        "url": "https://secret:token@private.invalid/artifact",
        "digests": {"sha256": "a".repeat(64)}
    })
}

fn metadata(files: Vec<Value>) -> Value {
    json!({
        "info": {"name": "demo", "version": "1.0", "requires_python": ">=3.10"},
        "urls": files
    })
}

fn invalid_wheel(value: &Value) {
    assert!(matches!(
        parse_pypi_response_for_context(value, &context()),
        Err(IndexError::InvalidWheelArtifact { .. })
    ));
}

#[test]
fn malformed_wheel_is_not_hidden_by_a_valid_source_or_input_order() {
    let mut value = metadata(vec![
        file("demo-1.0.tar.gz"),
        file("demo-1.0-bad-py3-none-any.whl"),
    ]);
    invalid_wheel(&value);
    value["urls"].as_array_mut().unwrap().reverse();
    invalid_wheel(&value);
}

#[test]
fn incompatible_file_conditions_do_not_hide_wrong_distribution() {
    let mut wrong = file("other-1.0-cp399-cp399-any.whl");
    wrong["requires_python"] = json!(">=3.99");
    invalid_wheel(&metadata(vec![file("demo-1.0.tar.gz"), wrong]));
}

#[test]
fn incompatible_file_conditions_do_not_hide_wrong_version() {
    let mut wrong = file("demo-2.0-cp399-cp399-any.whl");
    wrong["requires_python"] = json!(">=3.99");
    invalid_wheel(&metadata(vec![file("demo-1.0-py3-none-any.whl"), wrong]));
}

#[test]
fn present_distribution_kind_must_agree_with_filename() {
    let mut wrong_wheel = file("demo-1.0-py3-none-any.whl");
    wrong_wheel["packagetype"] = json!("sdist");
    let mut wrong_source = file("demo-1.0.tar.gz");
    wrong_source["packagetype"] = json!("bdist_wheel");
    for wrong in [wrong_wheel, wrong_source] {
        invalid_wheel(&metadata(vec![file("demo-1.0-py3-none-any.whl"), wrong]));
    }
}

#[test]
fn unsupported_or_non_string_kind_is_not_silently_reclassified() {
    for kind in [json!("bdist_egg"), json!(""), json!(false), Value::Null] {
        let mut wrong = file("demo-1.0.tar.gz");
        wrong["packagetype"] = kind;
        assert!(matches!(
            parse_pypi_response_for_context(&metadata(vec![wrong]), &context()),
            Err(IndexError::BadResponse { .. })
        ));
    }
}

#[test]
fn normalized_distribution_and_equivalent_pep440_version_are_allowed() {
    let mut value = metadata(vec![file("Demo.Package-1.0.0-py3-none-any.whl")]);
    value["info"]["name"] = json!("demo-package");
    let parsed = parse_pypi_response_for_context(&value, &context()).unwrap();
    assert_eq!(parsed.name, "demo-package");
    assert_eq!(parsed.artifacts.len(), 1);
    assert!(parsed.artifacts[0].kind.is_wheel());
    let mut epoch = metadata(vec![file("demo-1!2.0+cpu.1-py3-none-any.whl")]);
    epoch["info"]["version"] = json!("1!2.0+cpu.1");
    assert!(parse_pypi_response_for_context(&epoch, &context()).is_ok());
}

#[test]
fn expected_metadata_identity_must_be_valid_even_for_source_only() {
    for name in ["", "invalid/name"] {
        let mut value = metadata(vec![file("demo-1.0.tar.gz")]);
        value["info"]["name"] = json!(name);
        assert!(matches!(
            parse_pypi_response_for_context(&value, &context()),
            Err(IndexError::BadResponse { .. })
        ));
    }
    for key in ["name", "version"] {
        for invalid in [Value::Null, json!(42)] {
            let mut value = metadata(vec![file("demo-1.0-py3-none-any.whl")]);
            value["info"][key] = invalid;
            assert!(matches!(
                parse_pypi_response_for_context(&value, &context()),
                Err(IndexError::BadResponse { .. })
            ));
        }
        let mut value = metadata(vec![file("demo-1.0-py3-none-any.whl")]);
        value["info"].as_object_mut().unwrap().remove(key);
        assert!(matches!(
            parse_pypi_response_for_context(&value, &context()),
            Err(IndexError::BadResponse { .. })
        ));
    }
    for version in ["", "not_a_version"] {
        let mut value = metadata(vec![file("demo-1.0.tar.gz")]);
        value["info"]["version"] = json!(version);
        assert!(matches!(
            parse_pypi_response_for_context(&value, &context()),
            Err(IndexError::BadResponse { .. })
        ));
    }
}

#[test]
fn artifact_credentials_and_digest_are_absent_from_diagnostics() {
    let value = metadata(vec![file("other-1.0-py3-none-any.whl")]);
    let message = parse_pypi_response_for_context(&value, &context())
        .unwrap_err()
        .to_string();
    assert!(!message.contains("secret"));
    assert!(!message.contains("token"));
    assert!(!message.contains("https://"));
    assert!(!message.contains(&"a".repeat(64)));
}

#[tokio::test]
async fn cached_raw_metadata_is_revalidated_without_a_second_request() {
    let root = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let value = metadata(vec![
        file("demo-1.0.tar.gz"),
        file("demo-1.0-bad-py3-none-any.whl"),
    ]);
    let request = server
        .mock("GET", "/demo/1.0/json")
        .with_body(value.to_string())
        .expect(1)
        .create_async()
        .await;
    let index = Index::new(root.path()).with_base_url(server.url());
    for _ in 0..2 {
        assert!(matches!(
            index.metadata_for_context("demo", "1.0", &context()).await,
            Err(IndexError::InvalidWheelArtifact { .. })
        ));
    }
    request.assert_async().await;
}
