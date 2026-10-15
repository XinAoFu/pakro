use pakro_index::{
    Index, PythonMarkerContext, PythonMarkerContextBuilder, parse_pypi_response_for_context,
    parse_pypi_versions_for_context, python_requires_matches_for_context,
};
use serde_json::{Value, json};

fn builder<'a>(
    full: &'a str,
    implementation: &'a str,
    implementation_version: &'a str,
) -> PythonMarkerContextBuilder<'a> {
    PythonMarkerContextBuilder {
        implementation_name: implementation,
        implementation_version,
        os_name: "posix",
        platform_machine: "armv7l",
        platform_python_implementation: if implementation == "pypy" {
            "PyPy"
        } else {
            "CPython"
        },
        platform_release: "6.8.12-fixture",
        platform_system: "Linux",
        platform_version: "fixture kernel version",
        python_full_version: full,
        python_version: "3.10",
        sys_platform: "linux2",
    }
}

fn context(full: &str) -> PythonMarkerContext {
    PythonMarkerContext::new(builder(full, "cpython", full)).unwrap()
}

fn fixture() -> Value {
    json!({
        "info": {
            "name": "marker-demo", "version": "1.0.0", "requires_python": ">=3.10.13",
            "requires_dist": [
                "always>=1",
                "patch-old; python_full_version < '3.10.14'",
                "patch-new; python_full_version >= '3.10.14'",
                "pypy-only; implementation_name == 'pypy'",
                "cpython-only; implementation_name == 'cpython'",
                "pypy-version; implementation_version >= '7.3.17'",
                "extra-only; extra == 'dev'"
            ]
        },
        "urls": [{
            "filename": "marker_demo-1.0.0-py3-none-any.whl",
            "url": "https://files.pythonhosted.org/packages/demo.whl",
            "digests": {"sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        }]
    })
}

fn names(metadata: &pakro_index::PackageMetadata) -> Vec<&str> {
    metadata.deps.iter().map(|dep| dep.name.as_str()).collect()
}

#[test]
fn full_patch_changes_dependencies() {
    let older = parse_pypi_response_for_context(&fixture(), &context("3.10.13")).unwrap();
    let newer = parse_pypi_response_for_context(&fixture(), &context("3.10.14")).unwrap();
    assert_eq!(names(&older), vec!["always", "patch-old", "cpython-only"]);
    assert_eq!(names(&newer), vec!["always", "patch-new", "cpython-only"]);
}

#[test]
fn pypy_language_and_implementation_versions_are_distinct() {
    let runtime = PythonMarkerContext::new(builder("3.10.14", "pypy", "7.3.17")).unwrap();
    assert_eq!(runtime.python_full_version(), "3.10.14");
    assert_eq!(runtime.implementation_name(), "pypy");
    let metadata = parse_pypi_response_for_context(&fixture(), &runtime).unwrap();
    assert_eq!(
        names(&metadata),
        vec!["always", "patch-new", "pypy-only", "pypy-version"]
    );
    assert!(python_requires_matches_for_context("==3.10.14", &runtime).unwrap());
    assert!(!python_requires_matches_for_context(">=7.0", &runtime).unwrap());
}

#[test]
fn all_eleven_marker_values_come_from_explicit_input() {
    let mut value = fixture();
    value["info"]["requires_dist"] = json!([
        "field-01; implementation_name == 'pypy'",
        "field-02; implementation_version == '7.3.17'",
        "field-03; os_name == 'posix'",
        "field-04; platform_machine == 'armv7l'",
        "field-05; platform_python_implementation == 'PyPy'",
        "field-06; platform_release == '6.8.12-fixture'",
        "field-07; platform_system == 'Linux'",
        "field-08; platform_version == 'fixture kernel version'",
        "field-09; python_full_version == '3.10.14'",
        "field-10; python_version == '3.10'",
        "field-11; sys_platform == 'linux2'",
        "wrong; sys_platform == 'darwin'"
    ]);
    let runtime = PythonMarkerContext::new(builder("3.10.14", "pypy", "7.3.17")).unwrap();
    let metadata = parse_pypi_response_for_context(&value, &runtime).unwrap();
    assert_eq!(
        names(&metadata),
        vec![
            "field-01", "field-02", "field-03", "field-04", "field-05", "field-06", "field-07",
            "field-08", "field-09", "field-10", "field-11"
        ]
    );
}

#[test]
fn invalid_or_inconsistent_contexts_are_rejected() {
    for key in 0..3 {
        let mut input = builder("3.10.14", "cpython", "3.10.14");
        match key {
            0 => input.python_full_version = "broken",
            1 => input.python_version = "broken",
            _ => input.implementation_version = "broken",
        }
        assert!(PythonMarkerContext::new(input).is_err());
    }
    let mut input = builder("3.10.14", "cpython", "3.10.14");
    input.python_version = "3.11";
    assert!(PythonMarkerContext::new(input).is_err());
    assert!(PythonMarkerContext::new(builder("3.10", "cpython", "3.10")).is_err());
    assert!(PythonMarkerContext::new(builder("2.7.18", "cpython", "2.7.18")).is_err());
    assert!(PythonMarkerContext::new(builder("3.10.14", "cpython", "3.10.13")).is_err());
    let mut input = builder("3.10.14", "pypy", "7.3.17");
    input.platform_python_implementation = "CPython";
    assert!(PythonMarkerContext::new(input).is_err());
    let mut input = builder("3.10.14", "cpython", "3.10.14");
    input.platform_machine = "";
    assert!(PythonMarkerContext::new(input).is_err());
}

#[test]
fn requires_python_uses_full_pep440_language_version() {
    let runtime = context("3.10.14");
    for (spec, expected) in [
        (">=3.10.14", true),
        (">=3.10.15", false),
        ("!=3.10.14", false),
        ("==3.10.*", true),
        ("~=3.10.13", true),
        ("<3.10.14", false),
        ("", true),
    ] {
        assert_eq!(
            python_requires_matches_for_context(spec, &runtime).unwrap(),
            expected,
            "{spec}"
        );
    }
    let mut input = builder("3.13.0rc2", "cpython", "3.13.0rc2");
    input.python_version = "3.13";
    let prerelease = PythonMarkerContext::new(input).unwrap();
    assert!(python_requires_matches_for_context("==3.13.0rc2", &prerelease).unwrap());
    assert!(!python_requires_matches_for_context(">=3.13.0", &prerelease).unwrap());
}

#[test]
fn invalid_requires_python_is_not_permissive() {
    let runtime = context("3.10.14");
    for spec in ["not-a-spec", ">=>3.10", "~=3"] {
        assert!(
            python_requires_matches_for_context(spec, &runtime).is_err(),
            "{spec}"
        );
        let mut value = fixture();
        value["info"]["requires_python"] = json!(spec);
        assert!(
            parse_pypi_response_for_context(&value, &runtime).is_err(),
            "{spec}"
        );
    }
}

fn versions_fixture() -> Value {
    json!({
        "info": {"name": "marker-demo"},
        "releases": {
            "1.0": [{"requires_python": "==3.10.13"}],
            "2.0": [{"requires_python": ">=3.10.14"}],
            "3.0": [{"requires_python": null}],
            "4.0": [],
            "5.0": [{"requires_python": "<3.10.13"}, {"requires_python": "==3.10.14"}]
        }
    })
}

#[test]
fn release_listing_checks_every_file_with_full_patch() {
    assert_eq!(
        parse_pypi_versions_for_context(&versions_fixture(), &context("3.10.13")).unwrap(),
        vec!["1.0", "3.0"]
    );
    assert_eq!(
        parse_pypi_versions_for_context(&versions_fixture(), &context("3.10.14")).unwrap(),
        vec!["2.0", "3.0", "5.0"]
    );
}

#[test]
fn malformed_file_spec_is_reported_even_beside_a_matching_file() {
    for invalid in [json!("not-a-spec"), json!(42)] {
        let value = json!({
            "info": {"name": "marker-demo"},
            "releases": {"1.0": [{"requires_python": null}, {"requires_python": invalid}]}
        });
        assert!(parse_pypi_versions_for_context(&value, &context("3.10.14")).is_err());
    }
}

#[tokio::test]
async fn metadata_cache_keeps_raw_json_and_re_evaluates_context() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/marker-demo/1.0.0/json")
        .with_status(200)
        .with_body(fixture().to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let pypy = PythonMarkerContext::new(builder("3.10.14", "pypy", "7.3.17")).unwrap();
    let first = index
        .metadata_for_context("marker-demo", "1.0.0", &pypy)
        .await
        .unwrap();
    let second = index
        .metadata_for_context("marker-demo", "1.0.0", &context("3.10.13"))
        .await
        .unwrap();
    assert_eq!(
        names(&first),
        vec!["always", "patch-new", "pypy-only", "pypy-version"]
    );
    assert_eq!(names(&second), vec!["always", "patch-old", "cpython-only"]);
    mock.assert_async().await;
}

#[tokio::test]
async fn version_cache_keeps_raw_json_and_re_evaluates_full_patch() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/marker-demo/json")
        .with_status(200)
        .with_body(versions_fixture().to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path()).with_base_url(server.url());
    let first = index
        .versions_for_context("marker-demo", &context("3.10.13"))
        .await
        .unwrap();
    let second = index
        .versions_for_context("marker-demo", &context("3.10.14"))
        .await
        .unwrap();
    assert_eq!(first, vec!["1.0", "3.0"]);
    assert_eq!(second, vec!["2.0", "3.0", "5.0"]);
    mock.assert_async().await;
}

#[tokio::test]
async fn explicit_context_metadata_still_rewrites_artifact_mirror() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/marker-demo/1.0.0/json")
        .with_status(200)
        .with_body(fixture().to_string())
        .expect(1)
        .create_async()
        .await;
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path())
        .with_base_url(server.url())
        .with_pypi_files_url("https://mirror.example.test");
    let metadata = index
        .metadata_for_context("marker-demo", "1.0.0", &context("3.10.14"))
        .await
        .unwrap();
    assert_eq!(
        metadata.artifacts[0].url,
        "https://mirror.example.test/packages/demo.whl"
    );
    mock.assert_async().await;
}
