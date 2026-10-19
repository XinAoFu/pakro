use super::*;
use crate::config::{FrozenConfigDiscovery, Settings};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::{
    AllowedWrites, DeclarationTarget, InstallCommandIntent, InvocationInputs, InvocationSnapshot,
    OperationContextInputs, OperationPolicy, RequestBudget, capture_operation_context,
    normalize_intent,
};
use crate::toolset::{ConfigScope, ToolSource};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn context(root: &Path) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 64,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: true,
                declarations: true,
            },
        },
        cancellation: CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}

fn observe(root: &Path, operation: &OperationContext, args: &[&str]) -> ObservedProjectInputs {
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    let mut policy = ReadPolicy::new(discovery, operation.policy().deadline);
    policy.scope = crate::libraries::project::snapshot::ReadScope::LocalOnly;
    policy.cancellation = operation.cancellation_token();
    policy.cli_arguments = args.iter().map(|arg| (*arg).into()).collect();
    observe_project(root, policy).unwrap()
}

fn request(
    root: &Path,
    operation: &OperationContext,
    use_command: bool,
    args: &[&str],
) -> InstallRequest {
    let intent = if use_command {
        InstallCommandIntent::Use {
            target: DeclarationTarget {
                source: ToolSource::PakroToml(root.join("pakro.toml")),
                field_path: vec!["tools".into()],
            },
        }
    } else {
        InstallCommandIntent::Install
    };
    normalize_intent(
        intent,
        InvocationSnapshot::from_inputs(InvocationInputs {
            arguments: args.iter().map(|arg| (*arg).into()).collect(),
            declarations: vec![],
            runtime_candidates: vec![],
            known_runtime_names: vec!["python".into()],
            scope: ConfigScope::LocalOnly,
            context: operation.clone(),
        })
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn r03_bound_use_prepares_only_target_document_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(dir.path().join("pakro.toml"), "invalid cwd input").unwrap();
    fs::write(target.join("pakro.toml"), "# target\n").unwrap();
    let operation = context(dir.path());
    let observed = observe(&target, &operation, &["python@3.12.8"]);
    let request = request(&target, &operation, true, &["python@3.12.8"]);
    let bound = capture_context(&request, &observed, &operation).unwrap();
    let edit = bound.prepare_declaration_edit().unwrap().unwrap();
    assert_eq!(edit.target(), target.join("pakro.toml"));
    assert!(
        std::str::from_utf8(edit.bytes())
            .unwrap()
            .contains("3.12.8")
    );
    assert_eq!(
        fs::read_to_string(target.join("pakro.toml")).unwrap(),
        "# target\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("pakro.toml")).unwrap(),
        "invalid cwd input"
    );
}

#[test]
fn r03_bound_install_has_no_declaration_edit() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let observed = observe(dir.path(), &operation, &["python@3.11.9"]);
    let request = request(dir.path(), &operation, false, &["python@3.11.9"]);
    let bound = capture_context(&request, &observed, &operation).unwrap();
    assert!(bound.prepare_declaration_edit().unwrap().is_none());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn r03_binding_rejects_a_different_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let other = context(dir.path());
    let observed = observe(dir.path(), &operation, &[]);
    let request = request(dir.path(), &operation, false, &[]);
    assert!(matches!(
        capture_context(&request, &observed, &other),
        Err(ContextBindingError::InvocationMismatch)
    ));
}

#[test]
fn r03_binding_rejects_unobserved_or_stale_target() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let observed = observe(dir.path(), &operation, &["python@3.11.9"]);
    let unrelated = dir.path().join("elsewhere");
    let unrelated_request = request(&unrelated, &operation, true, &["python@3.11.9"]);
    assert!(matches!(
        capture_context(&unrelated_request, &observed, &operation),
        Err(ContextBindingError::InvalidTarget)
    ));
    fs::write(dir.path().join("pakro.local.toml"), "# external\n").unwrap();
    let request = request(dir.path(), &operation, false, &[]);
    assert!(capture_context(&request, &observed, &operation).is_err());
}

#[test]
fn r03_binding_does_not_promote_description_only_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    fs::write(
        dir.path().join("pakro.toml"),
        "[packages.\"python@3.11.9\"]\ndemo=\"1.0\"\n",
    )
    .unwrap();
    let observed = observe(dir.path(), &operation, &[]);
    let request = request(dir.path(), &operation, false, &[]);
    assert!(matches!(
        capture_context(&request, &observed, &operation),
        Err(ContextBindingError::DeclarationMismatch)
    ));
}

#[test]
fn r03_global_request_cannot_prepare_a_local_document() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let observed = observe(dir.path(), &operation, &["python@3.11.9"]);
    let mut request = request(dir.path(), &operation, true, &["python@3.11.9"]);
    request.invocation.scope = ConfigScope::GlobalOnly;
    assert!(capture_context(&request, &observed, &operation).is_err());
}

#[test]
fn r03_use_preserves_multiple_explicit_versions() {
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let args = ["python@3.11.9", "python@3.12.8"];
    let observed = observe(dir.path(), &operation, &args);
    let request = request(dir.path(), &operation, true, &args);
    let bound = capture_context(&request, &observed, &operation).unwrap();
    let edit = bound.prepare_declaration_edit().unwrap().unwrap();
    let document: toml::Value = toml::from_str(std::str::from_utf8(edit.bytes()).unwrap()).unwrap();
    assert_eq!(
        document["tools"]["python"].as_array().unwrap(),
        &vec![
            toml::Value::String("3.11.9".into()),
            toml::Value::String("3.12.8".into())
        ]
    );
}

#[test]
fn r03_unrelated_environment_variable_cannot_prove_runtime_binding() {
    use crate::programs::RuntimeCandidate;
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![dir.path().parent().unwrap().to_path_buf()];
    let mut read = ReadPolicy::new(discovery, operation.policy().deadline);
    read.scope = crate::libraries::project::snapshot::ReadScope::LocalOnly;
    read.cancellation = operation.cancellation_token();
    read.cli_arguments = vec!["demo@1.0".into()];
    read.environment.insert("UNRELATED".into(), "3.11.9".into());
    let observed = observe_project(dir.path(), read).unwrap();
    let invocation = InvocationSnapshot::from_inputs(InvocationInputs {
        arguments: vec!["demo@1.0".into()],
        declarations: vec![],
        runtime_candidates: vec![RuntimeCandidate {
            runtime_name: "python".into(),
            runtime_version: "3.11.9".into(),
            source: ToolSource::Environment("UNRELATED".into(), "3.11.9".into()),
        }],
        known_runtime_names: vec!["python".into()],
        scope: ConfigScope::LocalOnly,
        context: operation.clone(),
    })
    .unwrap();
    let request = normalize_intent(InstallCommandIntent::Install, invocation).unwrap();
    assert!(capture_context(&request, &observed, &operation).is_err());
}

#[test]
fn r03_overridden_runtime_description_cannot_bind_a_package() {
    use crate::programs::RuntimeCandidate;
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let base = dir.path().join("pakro.toml");
    fs::write(&base, "[tools]\npython='3.11.9'\n").unwrap();
    fs::write(
        dir.path().join("pakro.local.toml"),
        "[tools]\npython='3.12.8'\n",
    )
    .unwrap();
    let observed = observe(dir.path(), &operation, &["demo@1.0"]);
    let invocation = InvocationSnapshot::from_inputs(InvocationInputs {
        arguments: vec!["demo@1.0".into()],
        declarations: vec![],
        runtime_candidates: vec![RuntimeCandidate {
            runtime_name: "python".into(),
            runtime_version: "3.11.9".into(),
            source: ToolSource::PakroToml(base),
        }],
        known_runtime_names: vec!["python".into()],
        scope: ConfigScope::LocalOnly,
        context: operation.clone(),
    })
    .unwrap();
    let request = normalize_intent(InstallCommandIntent::Install, invocation).unwrap();
    assert!(capture_context(&request, &observed, &operation).is_err());
}

#[test]
fn r03_omitted_runtime_cannot_hide_a_bare_package_ambiguity() {
    use crate::programs::RuntimeCandidate;
    let dir = tempfile::tempdir().unwrap();
    let operation = context(dir.path());
    let path = dir.path().join("pakro.toml");
    fs::write(&path, "[tools]\npython='3.11.9'\nnode='22.0.0'\n").unwrap();
    let observed = observe(dir.path(), &operation, &["demo@1.0"]);
    let invocation = InvocationSnapshot::from_inputs(InvocationInputs {
        arguments: vec!["demo@1.0".into()],
        declarations: vec![],
        runtime_candidates: vec![RuntimeCandidate {
            runtime_name: "python".into(),
            runtime_version: "3.11.9".into(),
            source: ToolSource::PakroToml(path),
        }],
        known_runtime_names: vec!["python".into(), "node".into()],
        scope: ConfigScope::LocalOnly,
        context: operation.clone(),
    })
    .unwrap();
    let request = normalize_intent(InstallCommandIntent::Install, invocation).unwrap();
    assert!(capture_context(&request, &observed, &operation).is_err());
}

#[test]
fn r03_runtime_alias_cannot_hide_an_override_or_ambiguity() {
    use crate::programs::RuntimeCandidate;
    for (key, version) in [
        ("core:python", "3.12.8"),
        ("python[compile=true]", "3.12.8"),
        ("nodejs", "22.0.0"),
        ("core:node", "22.0.0"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let operation = context(dir.path());
        let base = dir.path().join("pakro.toml");
        fs::write(&base, "[tools]\npython='3.11.9'\n").unwrap();
        fs::write(
            dir.path().join("pakro.local.toml"),
            format!("[tools]\n'{key}'='{version}'\n"),
        )
        .unwrap();
        let observed = observe(dir.path(), &operation, &["demo@1.0"]);
        let invocation = InvocationSnapshot::from_inputs(InvocationInputs {
            arguments: vec!["demo@1.0".into()],
            declarations: vec![],
            runtime_candidates: vec![RuntimeCandidate {
                runtime_name: "python".into(),
                runtime_version: "3.11.9".into(),
                source: ToolSource::PakroToml(base),
            }],
            known_runtime_names: vec!["python".into(), "node".into()],
            scope: ConfigScope::LocalOnly,
            context: operation.clone(),
        })
        .unwrap();
        let request = normalize_intent(InstallCommandIntent::Install, invocation).unwrap();
        assert!(
            capture_context(&request, &observed, &operation).is_err(),
            "{key}"
        );
    }
}
