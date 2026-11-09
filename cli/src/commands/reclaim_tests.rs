//! Catalog-only reclaim commands must work without a usable project directory.
use super::*;
use std::process::Command;

const CHILD: &str = "cli::reclaim_tests::catalog_reclaim_cli_child_helper";
const ROOT_ENV: &str = "PAKRO_TEST_CATALOG_RECLAIM_ROOT";
const CASE_ENV: &str = "PAKRO_TEST_CATALOG_RECLAIM_CASE";

fn child(case: &str) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env(ROOT_ENV, &root)
        .env(CASE_ENV, case)
        .env("RUST_TEST_THREADS", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "catalog reclaim case {case} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("P4_CATALOG_CLI_CASE_PASSED={case}"))
    );
}

#[test]
fn p4_prune_catalog_only_preview_ignores_bad_project_configuration() {
    child("bad-preview");
}

#[test]
fn p4_prune_catalog_only_preview_works_after_the_current_directory_is_removed() {
    child("missing-preview");
}

#[test]
fn p4_exact_uninstall_does_not_load_broken_project_configuration() {
    child("bad-uninstall");
}

#[ignore = "subprocess-only catalog reclaim CLI fixture"]
#[tokio::test]
async fn catalog_reclaim_cli_child_helper() {
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("owned root is required"))
        .canonicalize()
        .unwrap();
    let case = std::env::var(CASE_ENV).unwrap();
    let data = root.join("data");
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    for (name, path) in [
        ("PAKRO_DATA_DIR", data.clone()),
        ("PAKRO_CACHE_DIR", root.join("cache")),
        ("PAKRO_CONFIG_DIR", root.join("config")),
        ("PAKRO_STATE_DIR", root.join("state")),
        (
            "PAKRO_GLOBAL_CONFIG_FILE",
            root.join("nonexistent-global.toml"),
        ),
        (
            "PAKRO_SYSTEM_CONFIG_FILE",
            root.join("nonexistent-system.toml"),
        ),
        ("PAKRO_SYSTEM_CONFIG_DIR", root.join("nonexistent-system")),
        ("PAKRO_CEILING_PATHS", root.clone()),
    ] {
        crate::env::set_var(name, path);
    }
    for (name, value) in [
        ("PAKRO_OVERRIDE_CONFIG_FILENAMES", "pakro.toml"),
        (
            "PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES",
            "nonexistent-tool-versions",
        ),
        ("PAKRO_AUTO_UPDATE", "0"),
        ("PAKRO_LOCKED", "0"),
    ] {
        crate::env::set_var(name, value);
    }
    Settings::reset(None);
    assert_eq!(
        crate::dirs::DATA.to_path_buf(),
        data,
        "fixture DATA was initialized before isolation"
    );
    let config = project.join(&*crate::env::PAKRO_DEFAULT_CONFIG_FILENAME);
    let invalid = b"[unclosed invalid project declaration\n";
    if case != "missing-preview" {
        std::fs::write(&config, invalid).unwrap();
    }
    crate::env::set_current_dir(&project).unwrap();
    if case == "missing-preview" {
        std::fs::remove_dir(&project).unwrap();
        assert!(std::env::current_dir().is_err());
    }
    assert!(!data.exists());
    let values = match case.as_str() {
        "bad-preview" | "missing-preview" => vec!["pakro", "prune", "--dry-run"],
        "bad-uninstall" => vec!["pakro", "-n", "uninstall", "python@3.15.0"],
        _ => panic!("unknown isolated catalog case"),
    };
    let arguments = values.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let result = Cli::run_inner(&arguments).await;
    match case.as_str() {
        "bad-preview" | "missing-preview" => result.unwrap(),
        "bad-uninstall" => {
            let error = result.expect_err("there is no installed target in this isolated library");
            assert!(
                format!("{error:#}").contains("no published Python instance matches"),
                "did not reach managed-library selection: {error:#}"
            );
        }
        _ => unreachable!(),
    }
    assert!(
        !data.exists(),
        "preview or rejected catalog operation initialized business state"
    );
    if case != "missing-preview" {
        assert_eq!(std::fs::read(&config).unwrap(), invalid);
    }
    assert!(!project.join("pakro.lock").exists() && !project.join(".pakro.lock.guard").exists());
    println!("P4_CATALOG_CLI_CASE_PASSED={case}");
}
