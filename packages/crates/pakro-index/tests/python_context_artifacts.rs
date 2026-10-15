use pakro_index::{
    IndexError, PythonMarkerContext, PythonMarkerContextBuilder, parse_pypi_response_for_context,
};
use serde_json::{Value, json};

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

fn fixture() -> Value {
    json!({
        "info": {
            "name": "artifact-demo",
            "version": "1.0.0",
            "requires_python": ">=3.10",
            "requires_dist": null
        },
        "urls": [
            {
                "filename": "artifact_demo-1.0.0-cp310-cp310-manylinux_2_17_x86_64.whl",
                "url": "https://files.pythonhosted.org/packages/native.whl",
                "requires_python": ">=3.10.13,!=3.10.14"
            },
            {
                "filename": "artifact_demo-1.0.0-py3-none-any.whl",
                "url": "https://files.pythonhosted.org/packages/pure.whl",
                "requires_python": "==3.10.14"
            }
        ]
    })
}

#[test]
fn same_release_files_use_full_patch_conditions_before_selection() {
    let value = fixture();
    let older = parse_pypi_response_for_context(&value, &context("3.10.13")).unwrap();
    let newer = parse_pypi_response_for_context(&value, &context("3.10.14")).unwrap();
    assert_eq!(older.requires_python.as_deref(), Some(">=3.10"));
    assert_eq!(newer.requires_python.as_deref(), Some(">=3.10"));
    assert_eq!(
        older
            .artifacts
            .iter()
            .map(|artifact| artifact.filename.as_str())
            .collect::<Vec<_>>(),
        vec!["artifact_demo-1.0.0-cp310-cp310-manylinux_2_17_x86_64.whl"]
    );
    assert_eq!(
        newer
            .artifacts
            .iter()
            .map(|artifact| artifact.filename.as_str())
            .collect::<Vec<_>>(),
        vec!["artifact_demo-1.0.0-py3-none-any.whl"]
    );
}

#[test]
fn invalid_file_condition_is_reported_after_accepted_and_rejected_files() {
    for invalid in [json!("not-a-spec"), json!(42), json!(false)] {
        let mut value = fixture();
        value["urls"][0]["requires_python"] = json!(">=3.10");
        value["urls"][1]["requires_python"] = json!("<3.10");
        let mut malformed = value["urls"][0].clone();
        malformed["requires_python"] = invalid;
        value["urls"].as_array_mut().unwrap().push(malformed);
        assert!(matches!(
            parse_pypi_response_for_context(&value, &context("3.10.14")),
            Err(IndexError::InvalidPythonSpecifier { .. })
        ));
    }
}

#[test]
fn info_allowing_runtime_does_not_override_all_files_rejecting_it() {
    let mut value = fixture();
    for file in value["urls"].as_array_mut().unwrap() {
        file["requires_python"] = json!("<3.10");
    }
    match parse_pypi_response_for_context(&value, &context("3.10.14")) {
        Err(IndexError::BadResponse { package, reason }) => {
            assert_eq!(package, "artifact-demo");
            assert_eq!(reason, "no downloadable artifacts");
        }
        result => panic!("expected no-artifact error, got {result:?}"),
    }
}
