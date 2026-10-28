//! Real CLI admission owners must survive a successful intervening forget.
//! Child isolation keeps process-wide config/cwd/statics out of other tests.

use super::*;
use pakro_store::control::{
    ControlLimits, ControlNamespace, RootLimits, StoreWriter, lease::StableLock,
};
use sha2::{Digest as _, Sha256};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const CHILD: &str = "cli::admission_tests::cli_admission_forget_child_helper";
const ROOT_ENV: &str = "PAKRO_TEST_CLI_ADMISSION_ROOT";
const CASE_ENV: &str = "PAKRO_TEST_CLI_ADMISSION_CASE";

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
        "CLI admission case {case} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("P3_CLI_ADMISSION_CASE_PASSED={case}"))
    );
}

#[test]
fn p3_install_cli_preflight_then_forget_rejects_the_original_admitted_request() {
    for case in ["install", "exact", "update"] {
        child(case);
    }
}

#[test]
fn p3_deps_cli_preflight_then_forget_rejects_the_original_admitted_request() {
    child("deps");
}

#[test]
fn p3_cli_package_classification_cannot_rebase_a_frozen_admission_after_forget() {
    child("classification");
}

#[test]
fn p3_install_and_deps_admission_and_previews_do_not_write_or_open_a_lock_guard() {
    for case in ["install-dry", "update-dry", "deps-dry"] {
        child(case);
    }
}

fn tree(root: &Path) -> Vec<(PathBuf, u64, u64, u32, u64, Vec<u8>)> {
    use std::os::unix::fs::MetadataExt;
    fn visit(root: &Path, path: &Path, rows: &mut Vec<(PathBuf, u64, u64, u32, u64, Vec<u8>)>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_dir() || metadata.is_file());
        rows.push((
            path.strip_prefix(root).unwrap().to_owned(),
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.len(),
            if metadata.is_file() {
                std::fs::read(path).unwrap()
            } else {
                Vec::new()
            },
        ));
        if metadata.is_dir() {
            let mut entries = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for entry in entries {
                visit(root, &entry, rows);
            }
        }
    }
    let mut rows = Vec::new();
    visit(root, root, &mut rows);
    rows
}

fn identity(root: &Path) -> uuid::Uuid {
    let hash = Sha256::new_with_prefix(b"pakro.project-root.v1")
        .chain_update(root.as_os_str().as_encoded_bytes())
        .finalize();
    let mut bytes: [u8; 16] = hash[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn seed_registration(data: &Path, project: &Path) -> uuid::Uuid {
    let id = identity(project);
    let transaction = uuid::Uuid::from_u128(1);
    let key =
        serde_json::json!({"project":id.to_string(),"group":"default","target":"bb".repeat(32)});
    let stamp = serde_json::json!({"revision":1,"generation":null,"language":format!("sha256-{}","aa".repeat(32)),"transaction":transaction.to_string()});
    let document = serde_json::json!({
        "schema":3,"generations":[],
        "selections":[{"key":key.clone(),"stamp":stamp}],
        "transactions":[{"id":transaction.to_string(),"key":key,
            "expected":{"revision":0,"generation":null,"language":null,"transaction":null},
            "generation":null,"language":format!("sha256-{}","aa".repeat(32))}],
        "projects":[{"project":id.to_string(),"root":project.to_str().unwrap(),"declaration":"cc".repeat(32)}]
    });
    let uid = nix::unistd::geteuid().as_raw();
    let writer = StoreWriter::open_writer(
        &data.join("projects"),
        uid,
        RootLimits::default(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    drop(
        StableLock::create_new(
            writer.build_handle(),
            &format!("project-{}.lock", id.hyphenated()),
            uid,
        )
        .unwrap(),
    );
    writer
        .control(ControlLimits::default())
        .unwrap()
        .replace(
            ControlNamespace::Catalog,
            "projects",
            None,
            &serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    id
}

fn parse(arguments: &[&str]) -> Cli {
    *crate::env::ARGS.write().unwrap() = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
    let argv = arguments
        .iter()
        .map(std::ffi::OsStr::new)
        .collect::<Vec<_>>();
    let mut cli = Cli::parse_from_argv(&argv).unwrap();
    if let Some(Commands::Install(command)) = &mut cli.command {
        command.inherit_root_dry_run(cli.dry_run);
    }
    if let Some(Commands::Deps(command)) = &mut cli.command {
        command.inherit_root_dry_run(cli.dry_run);
    }
    cli
}

#[ignore = "subprocess-only isolated CLI admission regression helper"]
#[tokio::test]
async fn cli_admission_forget_child_helper() {
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("owned parent root is required"))
        .canonicalize()
        .unwrap();
    let case = std::env::var(CASE_ENV).unwrap();
    let project = root.join("project");
    let data = root.join("data");
    for directory in [
        &project,
        &data,
        &root.join("config"),
        &root.join("cache"),
        &root.join("state"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let declarations = b"[tools]\npython='3.15.0'\n";
    std::fs::write(root.join("global.toml"), "").unwrap();
    for (key, value) in [
        ("PAKRO_DATA_DIR", data.clone()),
        ("PAKRO_CONFIG_DIR", root.join("config")),
        ("PAKRO_CACHE_DIR", root.join("cache")),
        ("PAKRO_STATE_DIR", root.join("state")),
        ("PAKRO_GLOBAL_CONFIG_FILE", root.join("global.toml")),
        (
            "PAKRO_SYSTEM_CONFIG_FILE",
            root.join("nonexistent-system.toml"),
        ),
        ("PAKRO_SYSTEM_CONFIG_DIR", root.join("nonexistent-system")),
        ("PAKRO_CEILING_PATHS", root.clone()),
        ("PAKRO_TRUSTED_CONFIG_PATHS", root.clone()),
    ] {
        crate::env::set_var(key, value);
    }
    for (key, value) in [
        ("PAKRO_OVERRIDE_CONFIG_FILENAMES", "pakro.toml"),
        (
            "PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES",
            "nonexistent-tool-versions",
        ),
        ("PAKRO_USE_TOML", "1"),
        ("PAKRO_LOCKED", "0"),
        ("PAKRO_AUTO_UPDATE", "0"),
        ("PAKRO_HTTP_TIMEOUT", "2s"),
        ("PAKRO_HTTP_DOWNLOAD_TIMEOUT", "2s"),
        (
            "PAKRO_URL_REPLACEMENTS",
            r#"{"https://github.com":"http://127.0.0.1:9","https://api.github.com":"http://127.0.0.1:9","https://pypi.org":"http://127.0.0.1:9","https://files.pythonhosted.org":"http://127.0.0.1:9"}"#,
        ),
    ] {
        crate::env::set_var(key, value);
    }
    let config_path = project.join(&*crate::env::PAKRO_DEFAULT_CONFIG_FILENAME);
    std::fs::write(&config_path, declarations).unwrap();
    crate::env::set_current_dir(&project).unwrap();
    crate::config::set_readonly_invocation(true);
    config_file::set_implicitly_trust_active_config(true);
    Settings::reset(None);
    *crate::env::ARGS.write().unwrap() = vec!["pakro".into(), "install".into()];
    assert_eq!(
        crate::dirs::DATA.to_path_buf(),
        data,
        "test constructor accessed shared DATA before isolation"
    );
    crate::toolset::install_state::init_readonly_plugin_metadata().unwrap();
    let id = seed_registration(&data, &project);
    assert_eq!(
        crate::programs::library::projects().unwrap()[0].identity,
        id.to_string()
    );
    let before = tree(&data);
    if case == "classification" {
        let config = read_config_for_admission().await.unwrap();
        let admission = crate::programs::python::capture_install_admission().unwrap();
        assert_eq!(tree(&data), before);
        assert!(
            crate::programs::library::forget_project(&id.to_string(), false)
                .unwrap()
                .forgotten
        );
        let forgotten = tree(&data);
        let _classified = config.get_tool_request_set().await.unwrap();
        let error = admission
            .project(&[], false)
            .err()
            .expect("classification rebased the admitted state");
        assert!(format!("{error:#}").contains("project registration or selections changed"));
        assert_eq!(tree(&data), forgotten);
    } else if case.ends_with("-dry") {
        let arguments = match case.as_str() {
            "install-dry" => vec!["pakro", "install", "--dry-run"],
            "update-dry" => vec!["pakro", "install", "--update-lock", "--dry-run"],
            "deps-dry" => vec!["pakro", "--dry-run", "deps", "install", "python"],
            _ => panic!("unknown isolated preview case"),
        };
        let prepared = parse(&arguments)
            .command
            .unwrap()
            .preflight()
            .await
            .unwrap();
        assert_eq!(tree(&data), before);
        prepared.run().await.unwrap();
        assert_eq!(tree(&data), before);
        assert!(!project.join(".pakro.lock.guard").exists());
    } else {
        let arguments = match case.as_str() {
            "install" => vec!["pakro", "install"],
            "exact" => vec!["pakro", "install", "python@3.15.0"],
            "update" => vec!["pakro", "install", "--update-lock"],
            "deps" => vec!["pakro", "deps", "install", "python"],
            _ => panic!("unknown isolated admission case"),
        };
        // This is the same consuming preflight/run boundary as Cli::run_inner.
        let prepared = parse(&arguments)
            .command
            .unwrap()
            .preflight()
            .await
            .unwrap();
        assert_eq!(tree(&data), before, "CLI preflight wrote business state");
        assert!(!project.join(".pakro.lock.guard").exists());
        assert!(
            crate::programs::library::forget_project(&id.to_string(), false)
                .unwrap()
                .forgotten
        );
        let forgotten = tree(&data);
        let error = prepared
            .run()
            .await
            .expect_err("the original CLI request revived a forgotten project");
        assert!(
            format!("{error:#}").contains("project registration or selections changed"),
            "unexpected failure: {error:#}"
        );
        assert_eq!(
            tree(&data),
            forgotten,
            "a rejected admitted CLI mutated the forgotten state"
        );
        assert!(crate::programs::library::projects().unwrap().is_empty());
    }
    assert_eq!(std::fs::read(&config_path).unwrap(), declarations);
    assert!(!project.join("pakro.lock").exists() && !project.join(".pakro.lock.guard").exists());
    assert!(!data.join("languages").exists() && !data.join("packages").exists());
    println!("P3_CLI_ADMISSION_CASE_PASSED={case}");
}
