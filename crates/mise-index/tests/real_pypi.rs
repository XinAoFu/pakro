#[tokio::test]
#[ignore = "hits the real PyPI API; run with --ignored"]
async fn smoke_real_pypi() {
    let idx = mise_index::Index::new("/tmp/mise-index-smoke-cache");
    let rt = mise_index::RuntimeInfo {
        major: 3,
        minor: 10,
    };
    let md = idx.metadata("numpy", "1.26.4", &rt).await.unwrap();
    assert_eq!(md.name, "numpy");
    assert!(
        md.artifacts
            .iter()
            .any(|a| a.filename.contains("macosx") && a.filename.contains("arm64"))
    );
    eprintln!(
        "deps: {:?}",
        md.deps.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
    eprintln!(
        "wheel count: {}",
        md.artifacts.iter().filter(|a| a.kind.is_wheel()).count()
    );
}
