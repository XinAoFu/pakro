use pakro_index::{Index, PythonMarkerContext, PythonMarkerContextBuilder};

#[tokio::test]
#[ignore = "hits the real PyPI API; run with --ignored"]
async fn smoke_real_pypi() {
    let cache = tempfile::tempdir().unwrap();
    let index = Index::new(cache.path());
    let target = PythonMarkerContext::new(PythonMarkerContextBuilder {
        implementation_name: "cpython",
        implementation_version: "3.10.14",
        os_name: "posix",
        platform_machine: "arm64",
        platform_python_implementation: "CPython",
        platform_release: "fixture release",
        platform_system: "Darwin",
        platform_version: "fixture version",
        python_full_version: "3.10.14",
        python_version: "3.10",
        sys_platform: "darwin",
    })
    .unwrap();
    let metadata = index
        .metadata_for_context("numpy", "1.26.4", &target)
        .await
        .unwrap();
    assert_eq!(metadata.name, "numpy");
    assert!(
        metadata
            .artifacts
            .iter()
            .any(|a| a.filename.contains("macosx") && a.filename.contains("arm64"))
    );
    eprintln!(
        "deps: {:?}",
        metadata.deps.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    eprintln!(
        "wheel count: {}",
        metadata
            .artifacts
            .iter()
            .filter(|a| a.kind.is_wheel())
            .count()
    );
}
