use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::time::{Duration, Instant};

fn observe(root: &Path) -> ObservedProjectInputs {
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    observe_project(
        root,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30)),
    )
    .unwrap()
}

fn set_python() -> DeclarationChange {
    DeclarationChange::SetTool {
        key: "python".into(),
        versions: vec!["3.12.8".into()],
    }
}

#[test]
fn r03_proposed_edit_preserves_comments_and_unrelated_fields_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "# project header\n[tools]\npython = '3.11.9' # selected Python\nnode = '22.0.0'\n\n[future]\nliteral = 'PRIVATE_SENTINEL' # untouched\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    let proposed = prepare_edit(&snapshot, &path, &[set_python()]).unwrap();
    let output = std::str::from_utf8(proposed.bytes()).unwrap();
    assert!(output.starts_with("# project header\n[tools]\n"));
    assert!(output.contains("# selected Python"));
    assert!(output.contains("node = '22.0.0'"));
    assert!(output.ends_with("[future]\nliteral = 'PRIVATE_SENTINEL' # untouched\n"));
    assert!(output.contains("3.12.8"));
    assert_eq!(proposed.target(), path);
    assert_eq!(
        proposed.sha256(),
        &<[u8; 32]>::from(Sha256::digest(proposed.bytes()))
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert!(!format!("{proposed:?}").contains("PRIVATE_SENTINEL"));
    proposed.recheck_for(&snapshot).unwrap();
}

#[test]
fn r03_proposed_edit_is_bound_to_the_same_observation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    fs::write(&path, "[tools]\npython='3.11.9'\n").unwrap();
    let snapshot = observe(root.path());
    let other = observe(root.path());
    let proposed = prepare_edit(&snapshot, &path, &[set_python()]).unwrap();
    assert_eq!(
        proposed.recheck_for(&other),
        Err(DeclarationEditError::WrongOwner)
    );
}

#[test]
fn r03_external_edit_revokes_a_prepared_document() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    fs::write(&path, "[tools]\npython='3.11.9'\n").unwrap();
    let snapshot = observe(root.path());
    let proposed = prepare_edit(&snapshot, &path, &[set_python()]).unwrap();
    let external = "# external writer\n[tools]\npython='3.13.1'\n";
    fs::write(&path, external).unwrap();
    assert_eq!(
        proposed.recheck_for(&snapshot),
        Err(DeclarationEditError::Revoked)
    );
    assert_eq!(
        prepare_edit(&snapshot, &path, &[set_python()]).unwrap_err(),
        DeclarationEditError::Revoked
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
}

#[test]
fn r03_missing_observed_target_can_be_proposed_without_creation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let snapshot = observe(root.path());
    let proposed = prepare_edit(&snapshot, &path, &[set_python()]).unwrap();
    assert!(
        std::str::from_utf8(proposed.bytes())
            .unwrap()
            .contains("3.12.8")
    );
    assert!(!path.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r03_unobserved_edit_target_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let path = other.path().join("pakro.toml");
    fs::write(&path, "[tools]\npython='3.11.9'\n").unwrap();
    let snapshot = observe(root.path());
    assert_eq!(
        prepare_edit(&snapshot, &path, &[set_python()]).unwrap_err(),
        DeclarationEditError::UnobservedTarget
    );
}

#[test]
fn r03_pyproject_is_not_rewritten_as_native_configuration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    let raw = "[project]\nname='app'\nversion='1.0'\ndynamic=['dependencies']\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    assert_eq!(
        prepare_edit(&snapshot, &path, &[set_python()]).unwrap_err(),
        DeclarationEditError::UnsupportedFormat
    );
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn r03_package_and_remove_changes_preserve_other_declarations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "[tools]\npython='3.11.9'\nnode='22.0.0' # retained\n\n[packages.'python@3.11.9']\nold='1.0' # retained package\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    let proposed = prepare_edit(
        &snapshot,
        &path,
        &[
            DeclarationChange::RemoveTool {
                key: "python".into(),
            },
            DeclarationChange::AddPinnedPackages {
                requests: vec![PackageInstallRequest {
                    runtime_name: "python".into(),
                    runtime_version: "3.11.9".into(),
                    name: "new".into(),
                    version: "2.0".into(),
                }],
            },
        ],
    )
    .unwrap();
    let output = std::str::from_utf8(proposed.bytes()).unwrap();
    assert!(output.contains("node='22.0.0' # retained"));
    assert!(output.contains("old='1.0' # retained package"));
    let document: toml::Value = toml::from_str(output).unwrap();
    assert!(document["tools"].get("python").is_none());
    assert_eq!(
        document["packages"]["python@3.11.9"]["new"].as_str(),
        Some("2.0")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn r03_proposed_document_obeys_the_snapshot_byte_budget() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "[tools]\npython='3.11.9'\n";
    fs::write(&path, raw).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.path().parent().unwrap().to_path_buf()];
    let mut policy = ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30));
    policy.file_limits.max_bytes = raw.len();
    let snapshot = observe_project(root.path(), policy).unwrap();
    assert_eq!(
        prepare_edit(
            &snapshot,
            &path,
            &[DeclarationChange::SetTool {
                key: "python".into(),
                versions: vec!["3.12.1234567890".into()],
            },]
        )
        .unwrap_err(),
        DeclarationEditError::Limit
    );
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn r03_version_edit_preserves_table_options_and_array_comments() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "[tools.python]\nversion = '3.11.9' # version note\nos = ['linux'] # option note\n\n[tools]\nnode = [\n  '20.0.0', # first\n  '22.0.0', # second\n]\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    let proposed = prepare_edit(
        &snapshot,
        &path,
        &[
            set_python(),
            DeclarationChange::SetTool {
                key: "node".into(),
                versions: vec!["20.1.0".into(), "22.1.0".into()],
            },
        ],
    )
    .unwrap();
    let output = std::str::from_utf8(proposed.bytes()).unwrap();
    for retained in [
        "# version note",
        "os = ['linux'] # option note",
        "# first",
        "# second",
    ] {
        assert!(output.contains(retained), "missing {retained}");
    }
    let document: toml::Value = toml::from_str(output).unwrap();
    assert_eq!(
        document["tools"]["python"]["version"].as_str(),
        Some("3.12.8")
    );
    assert_eq!(document["tools"]["node"][1].as_str(), Some("22.1.0"));
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn r03_version_edit_does_not_reinterpret_a_path_selector() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "[tools]\npython = { path = './local-python', lazy = true }\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    assert_eq!(
        prepare_edit(&snapshot, &path, &[set_python()]).unwrap_err(),
        DeclarationEditError::UnsupportedSelector
    );
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn r03_unchanged_declarations_preserve_the_exact_document_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.toml");
    let raw = "# original spelling\n[tools]\npython = '3.12.8' # selected\n\n[tools.node]\nversion = '22.0.0' # table selection\nlazy = true\n\n[packages.'python@3.12.8']\ndemo = '1.0' # package pin\n";
    fs::write(&path, raw).unwrap();
    let snapshot = observe(root.path());
    let proposed = prepare_edit(
        &snapshot,
        &path,
        &[
            set_python(),
            DeclarationChange::SetTool {
                key: "node".into(),
                versions: vec!["22.0.0".into()],
            },
            DeclarationChange::AddPinnedPackages {
                requests: vec![PackageInstallRequest {
                    runtime_name: "python".into(),
                    runtime_version: "3.12.8".into(),
                    name: "demo".into(),
                    version: "1.0".into(),
                }],
            },
        ],
    )
    .unwrap();
    assert_eq!(proposed.bytes(), raw.as_bytes());
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}
