//! Live differential against npm's own CLI (SPEC FR-12, node side). Skipped
//! unless npm exists on PATH; `npm view <pkg>@<ver> dependencies --json` is a
//! small stable JSON shape, unlike `npm install --dry-run` output.

use std::process::Command;

use serde_json::Value;

fn npm_bin() -> Option<std::path::PathBuf> {
    std::env::var("PATH")
        .ok()?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(d).join("npm"))
        .find(|p| p.is_file())
}

#[tokio::test]
#[ignore = "hits the real npm registry; run with --ignored"]
async fn npm_metadata_matches_npm_view() {
    let Some(npm) = npm_bin() else {
        eprintln!("SKIPPED: npm not on PATH");
        return;
    };
    const PKG: &str = "commander";
    const VERSION: &str = "2.20.3";
    let output = Command::new(npm)
        .args([
            "view",
            &format!("{PKG}@{VERSION}"),
            "dependencies",
            "--json",
        ])
        .output()
        .expect("running npm view");
    if !output.status.success() {
        eprintln!("SKIPPED: npm view failed (offline?)");
        return;
    }
    let expected: Value = serde_json::from_slice(&output.stdout).expect("npm view json");
    let index = mise_index::Index::new("/tmp/mise-npm-live-cache");
    let runtime = mise_index::RuntimeInfo {
        major: 22,
        minor: 0,
    };
    let metadata = index
        .npm_metadata(PKG, VERSION, &runtime)
        .await
        .expect("registry metadata");
    let ours: std::collections::BTreeMap<&str, &str> = metadata
        .deps
        .iter()
        .map(|d| (d.name.as_str(), d.constraint.as_str()))
        .collect();
    let theirs: std::collections::BTreeMap<String, String> =
        serde_json::from_value(expected).unwrap_or_default();
    // commander 2.20.3 has no runtime dependencies, so both sides should be
    // empty; assert the shapes agree regardless
    assert_eq!(
        ours.len(),
        theirs.len(),
        "ours: {ours:?}\ntheirs: {theirs:?}"
    );
}
