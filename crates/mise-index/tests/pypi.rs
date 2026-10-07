use mise_index::RuntimeInfo;
use mise_index::pypi::parse_pypi_response;
use serde_json::json;

fn fixture() -> serde_json::Value {
    json!({
        "info": {
            "name": "numpy",
            "version": "4.2.2",
            "requires_python": ">=3.10",
            "requires_dist": [
                "contingent-pyprops>=0.1,<0.3",
                "tomli>=1.1.0; python_version < '3.11'",
                "rich[testutils]>=12; extra == 'dev'"
            ]
        },
        "urls": [
            {
                "filename": "numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.whl",
                "url": "https://files.pythonhosted.org/packages/aa/numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.whl",
                "digests": {"sha256": "abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd"},
                "packagetype": "bdist_wheel"
            },
            {
                "filename": "numpy-4.2.2.tar.gz",
                "url": "https://files.pythonhosted.org/packages/aa/numpy-4.2.2.tar.gz",
                "digests": {"sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"},
                "packagetype": "sdist"
            }
        ]
    })
}

fn rt(major: u32, minor: u32) -> RuntimeInfo {
    RuntimeInfo { major, minor }
}

#[test]
fn parses_metadata_and_filters_markers_for_3_10() {
    let md = parse_pypi_response(&fixture(), &rt(3, 10)).unwrap();
    assert_eq!(md.name, "numpy");
    assert_eq!(md.version, "4.2.2");
    assert_eq!(md.requires_python.as_deref(), Some(">=3.10"));
    // tomli applies to <3.11; the extra-gated dep is dropped (no extras requested)
    assert_eq!(
        md.deps
            .iter()
            .map(|d| (d.name.as_str(), d.constraint.as_str()))
            .collect::<Vec<_>>(),
        vec![("contingent-pyprops", ">=0.1, <0.3"), ("tomli", ">=1.1.0")]
    );
}

#[test]
fn marker_drops_inapplicable_deps_for_3_12() {
    let md = parse_pypi_response(&fixture(), &rt(3, 12)).unwrap();
    assert_eq!(
        md.deps.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        vec!["contingent-pyprops"]
    );
}

#[test]
fn artifacts_split_into_wheel_and_sdist() {
    let md = parse_pypi_response(&fixture(), &rt(3, 10)).unwrap();
    assert_eq!(md.artifacts.len(), 2);
    let wheel = md
        .artifacts
        .iter()
        .find(|a| a.filename.ends_with(".whl"))
        .unwrap();
    assert_eq!(
        wheel.digest.as_deref(),
        Some("sha256-abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd")
    );
    assert!(wheel.kind.is_wheel());
    let sdist = md
        .artifacts
        .iter()
        .find(|a| a.filename.ends_with(".tar.gz"))
        .unwrap();
    assert!(!sdist.kind.is_wheel());
}

#[test]
fn empty_requires_dist_is_ok() {
    let mut f = fixture();
    f["info"]["requires_dist"] = json!(serde_json::Value::Null);
    let md = parse_pypi_response(&f, &rt(3, 10)).unwrap();
    assert!(md.deps.is_empty());
}

#[test]
fn parses_version_listing() {
    use mise_index::pypi::parse_pypi_versions;
    let value = serde_json::json!({
        "info": {"name": "numpy", "version": "4.2.2"},
        "releases": {
            "4.2.1": [{"filename": "numpy-4.2.1.tar.gz"}],
            "4.2.2": [{"filename": "numpy-4.2.2-cp310-cp310-any.whl"}],
            "0.1.0-broken": []
        }
    });
    let versions = parse_pypi_versions(&value).unwrap();
    assert_eq!(versions, vec!["4.2.1".to_string(), "4.2.2".to_string()]);
}

#[test]
fn version_listing_filters_by_requires_python() {
    use mise_index::pypi::parse_pypi_versions_for_runtime;
    let value = serde_json::json!({
        "info": {"name": "networkx", "version": "3.7"},
        "releases": {
            "3.4.2": [{"filename": "networkx-3.4.2.tar.gz", "requires_python": ">=3.10"}],
            "3.7.0": [{"filename": "networkx-3.7.0.tar.gz", "requires_python": "!=3.14.1,>=3.12"}],
            "3.8.0": [{"filename": "networkx-3.8.0.tar.gz", "requires_python": null}],
            "3.9.0-empty": []
        }
    });
    // 3.11 runtime: 3.7.0 demands >=3.12 and must drop out of the candidate
    // list before the solver ever sees it; a null spec stays permissive
    let for_311 = parse_pypi_versions_for_runtime(&value, &rt(3, 11)).unwrap();
    assert_eq!(for_311, vec!["3.4.2".to_string(), "3.8.0".to_string()]);
    let for_312 = parse_pypi_versions_for_runtime(&value, &rt(3, 12)).unwrap();
    assert_eq!(
        for_312,
        vec![
            "3.4.2".to_string(),
            "3.7.0".to_string(),
            "3.8.0".to_string()
        ]
    );
}
