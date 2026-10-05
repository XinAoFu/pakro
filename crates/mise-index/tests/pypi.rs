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
        wheel.sha256.as_deref(),
        Some("abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd")
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
