use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::config::Settings;
use crate::packages::PackageInstallRequest;
use crate::programs::shared::policy::{
    AllowedWrites, OperationPolicy, RequestBudget, RequestLimit,
};
use crate::programs::shared::types::DeclarationOperation;
use crate::programs::{OperationContextInputs, capture_operation_context};
use crate::toolset::ToolSource;
use tokio_util::sync::CancellationToken;

const BASE_CONFIG: &str = r#"[tools]
python = "3.11.9"

[packages."python@3.11.9"]
demo = "0.8.0"
"#;
const LOCAL_CONFIG: &str = r#"[packages."python@3.11.9"]
demo = "0.9.0"
"#;
const LOCK_SENTINEL: &str = "# exact lock sentinel\n";

struct Fixture {
    directory: tempfile::TempDir,
    config: PathBuf,
    local_config: PathBuf,
    lock: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("pakro.toml");
        let local_config = directory.path().join("pakro.local.toml");
        let lock = directory.path().join("pakro.lock");
        fs::write(&config, BASE_CONFIG).unwrap();
        fs::write(&local_config, LOCAL_CONFIG).unwrap();
        fs::write(&lock, LOCK_SENTINEL).unwrap();
        Self {
            directory,
            config,
            local_config,
            lock,
        }
    }

    fn target(&self, packages: bool) -> DeclarationTarget {
        DeclarationTarget {
            source: ToolSource::PakroToml(self.local_config.clone()),
            field_path: if packages {
                vec!["packages".into(), "python@3.11.9".into()]
            } else {
                vec!["tools".into()]
            },
        }
    }

    fn policy(&self) -> OperationPolicy {
        OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: true,
                declarations: true,
            },
        }
    }

    fn context(
        &self,
        policy: OperationPolicy,
        cancellation: CancellationToken,
    ) -> Result<OperationContext, OperationError> {
        capture_operation_context(OperationContextInputs {
            settings: &Settings::default(),
            policy,
            cancellation,
            data_root: self.directory.path().join("data"),
            cache_root: self.directory.path().join("cache"),
            python_sources: None,
            python_target_description: None,
        })
    }

    // Test setup creates a new explicit call; production contexts have no policy setter.
    fn replace_fixture_context(
        &self,
        invocation: &mut InvocationSnapshot,
        policy: OperationPolicy,
    ) -> Result<(), OperationError> {
        invocation.context = self.context(policy, CancellationToken::new())?;
        Ok(())
    }

    fn invocation(&self, arguments: &[&str]) -> InvocationSnapshot {
        let original_declarations = [(&self.config, "0.8.0"), (&self.local_config, "0.9.0")]
            .into_iter()
            .map(|(path, version)| OwnedPackageRequest {
                request: PackageInstallRequest {
                    runtime_name: "python".into(),
                    runtime_version: "3.11.9".into(),
                    name: "demo".into(),
                    version: version.into(),
                },
                source: ToolSource::PakroToml(path.clone()),
            })
            .collect();
        InvocationSnapshot {
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
            original_declarations,
            runtime_candidates: vec![RuntimeCandidate {
                runtime_name: "python".into(),
                runtime_version: "3.11.9".into(),
                source: ToolSource::PakroToml(self.config.clone()),
            }],
            known_runtime_names: vec!["python".into()],
            scope: ConfigScope::LocalOnly,
            context: self
                .context(self.policy(), CancellationToken::new())
                .unwrap(),
        }
    }

    fn assert_files_unchanged(&self) {
        assert_eq!(fs::read(&self.config).unwrap(), BASE_CONFIG.as_bytes());
        assert_eq!(
            fs::read(&self.local_config).unwrap(),
            LOCAL_CONFIG.as_bytes()
        );
        assert_eq!(fs::read(&self.lock).unwrap(), LOCK_SENTINEL.as_bytes());
        assert_eq!(fs::read_dir(self.directory.path()).unwrap().count(), 3);
    }
}

fn assert_original_inputs(request: &InstallRequest, original: &InvocationSnapshot) {
    assert_eq!(request.invocation.arguments, original.arguments);
    assert_eq!(request.invocation.scope, original.scope);
    assert_eq!(
        request.invocation.context.policy(),
        original.context.policy()
    );
    assert_eq!(
        request.invocation.runtime_candidates,
        original.runtime_candidates
    );
    assert_eq!(
        request.invocation.known_runtime_names,
        original.known_runtime_names
    );
    let declarations = &request.invocation.original_declarations;
    assert_eq!(declarations.len(), original.original_declarations.len());
    for (actual, expected) in declarations.iter().zip(&original.original_declarations) {
        assert_eq!(actual.request, expected.request);
        assert_eq!(actual.source, expected.source);
    }
}

#[test]
fn r01_install_does_not_edit_declarations() {
    let fixture = Fixture::new();
    let invocation = fixture.invocation(&["python@3.11.9"]);
    let install = normalize_intent(InstallCommandIntent::Install, invocation.clone())
        .expect("ordinary install must normalize without a declaration edit");
    assert_eq!(install.intent, InstallCommandIntent::Install);
    assert!(install.declaration_edit.is_none());
    assert_original_inputs(&install, &invocation);

    let target = fixture.target(false);
    let use_intent = InstallCommandIntent::Use {
        target: target.clone(),
    };
    let use_request = normalize_intent(use_intent.clone(), invocation.clone())
        .expect("use must retain its explicit declaration edit target");
    assert_eq!(use_request.intent, use_intent);
    assert_eq!(
        use_request.declaration_edit,
        Some(DeclarationEditIntent {
            target,
            operation: DeclarationOperation::SetSelection,
        })
    );
    assert_original_inputs(&use_request, &invocation);
    fixture.assert_files_unchanged();
}

#[test]
fn r01_ambiguous_binding_stops_before_io() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["demo@1.0.0"]);
    invocation.runtime_candidates.push(RuntimeCandidate {
        runtime_name: "python".into(),
        runtime_version: "3.12.7".into(),
        source: ToolSource::PakroToml(fixture.local_config.clone()),
    });
    let result = normalize_intent(InstallCommandIntent::Install, invocation.clone());
    fixture.assert_files_unchanged();
    assert_eq!(
        result.expect_err("an unbound package must not choose a Python"),
        OperationError::AmbiguousRuntimeBinding { candidate_count: 2 }
    );

    // A valid explicit binding must not be rejected just because two candidates
    // exist. Neither candidate requires an installed interpreter or canonicalize.
    invocation.arguments = vec!["python@3.12.7+demo@1.0.0".into()];
    let request = normalize_intent(InstallCommandIntent::Install, invocation)
        .expect("an explicit binding must resolve the request ambiguity");
    assert!(request.packages.iter().any(|owned| {
        owned.request.runtime_name == "python"
            && owned.request.runtime_version == "3.12.7"
            && owned.request.name == "demo"
            && owned.request.version == "1.0.0"
            && owned.source == ToolSource::Argument
    }));
    fixture.assert_files_unchanged();
}

#[test]
fn r01_deps_add_preserves_target_scope_and_declarations() {
    let fixture = Fixture::new();
    let invocation = fixture.invocation(&["python@3.11.9+demo@1.0.0"]);
    let target = fixture.target(true);
    let intent = InstallCommandIntent::DepsAdd {
        target: target.clone(),
    };
    let request = normalize_intent(intent.clone(), invocation.clone())
        .expect("deps add must describe an edit to its exact owner and field");
    assert_eq!(request.intent, intent);
    assert_eq!(
        request.declaration_edit,
        Some(DeclarationEditIntent {
            target,
            operation: DeclarationOperation::AddDependency,
        })
    );
    assert_original_inputs(&request, &invocation);
    let expected = PackageInstallRequest {
        runtime_name: "python".into(),
        runtime_version: "3.11.9".into(),
        name: "demo".into(),
        version: "1.0.0".into(),
    };
    assert!(
        request
            .packages
            .iter()
            .any(|owned| owned.request == expected && owned.source == ToolSource::Argument)
    );
    fixture.assert_files_unchanged();
}

#[test]
fn r01_deps_install_and_upgrade_keep_distinct_intents() {
    let fixture = Fixture::new();
    let invocation = fixture.invocation(&["python@3.11.9"]);
    for intent in [
        InstallCommandIntent::DepsInstall,
        InstallCommandIntent::Upgrade {
            declaration_target: None,
        },
    ] {
        let request = normalize_intent(intent.clone(), invocation.clone())
            .expect("each install command must retain its own intent");
        assert_eq!(request.intent, intent);
        assert!(request.declaration_edit.is_none());
        assert_original_inputs(&request, &invocation);
    }
    fixture.assert_files_unchanged();
}

#[test]
fn r01_locked_rejects_declaration_edits() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["python@3.11.9+demo@1.0.0"]);
    let mut policy = *invocation.context.policy();
    policy.locked = true;
    fixture
        .replace_fixture_context(&mut invocation, policy)
        .unwrap();
    for intent in [
        InstallCommandIntent::Use {
            target: fixture.target(false),
        },
        InstallCommandIntent::DepsAdd {
            target: fixture.target(true),
        },
        InstallCommandIntent::Upgrade {
            declaration_target: Some(fixture.target(false)),
        },
    ] {
        let result = normalize_intent(intent, invocation.clone());
        fixture.assert_files_unchanged();
        assert_eq!(
            result.expect_err("locked requests must reject declaration changes"),
            OperationError::LockedDeclarationEdit
        );
    }
}

#[test]
fn r01_expired_deadline_stops_before_normalization() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&[""]);
    let mut policy = *invocation.context.policy();
    policy.deadline = Instant::now() - Duration::from_secs(1);
    let error = fixture
        .replace_fixture_context(&mut invocation, policy)
        .and_then(|()| normalize_intent(InstallCommandIntent::Install, invocation))
        .unwrap_err();
    assert_eq!(error, OperationError::DeadlineExceeded);
    fixture.assert_files_unchanged();
}

fn assert_budget_exceeded(limit: RequestLimit) {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["python@3.11.9", "python@版本"]);
    let mut policy = *invocation.context.policy();
    match limit {
        RequestLimit::Arguments => policy.budget.max_arguments = 1,
        RequestLimit::ArgumentBytes => {
            policy.budget.max_total_argument_bytes =
                invocation.arguments.iter().map(String::len).sum::<usize>() - 1;
            assert!(
                invocation
                    .arguments
                    .iter()
                    .all(|arg| { arg.len() <= policy.budget.max_total_argument_bytes })
            );
        }
        RequestLimit::Declarations => policy.budget.max_declarations = 1,
        RequestLimit::RuntimeCandidates => policy.budget.max_runtime_candidates = 0,
    }
    fixture
        .replace_fixture_context(&mut invocation, policy)
        .unwrap();
    let error = normalize_intent(InstallCommandIntent::Install, invocation).unwrap_err();
    assert_eq!(error, OperationError::ResourceLimit(limit));
    fixture.assert_files_unchanged();
}

#[test]
fn r01_argument_count_budget_is_enforced() {
    assert_budget_exceeded(RequestLimit::Arguments);
}

#[test]
fn r01_total_argument_bytes_budget_is_enforced() {
    assert_budget_exceeded(RequestLimit::ArgumentBytes);
}

#[test]
fn r01_declaration_budget_is_enforced() {
    assert_budget_exceeded(RequestLimit::Declarations);
}

#[test]
fn r01_runtime_candidate_budget_is_enforced() {
    assert_budget_exceeded(RequestLimit::RuntimeCandidates);
}

#[test]
fn r01_budget_boundary_is_inclusive() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["python@3.11.9", "python@版本"]);
    let mut policy = *invocation.context.policy();
    policy.budget = RequestBudget {
        max_arguments: invocation.arguments.len(),
        max_total_argument_bytes: invocation.arguments.iter().map(String::len).sum(),
        max_declarations: invocation.original_declarations.len(),
        max_runtime_candidates: invocation.runtime_candidates.len(),
    };
    fixture
        .replace_fixture_context(&mut invocation, policy)
        .unwrap();
    let request = normalize_intent(InstallCommandIntent::Install, invocation.clone())
        .expect("an input exactly on each budget boundary is allowed");
    assert_original_inputs(&request, &invocation);

    invocation.arguments.clear();
    invocation.original_declarations.clear();
    invocation.runtime_candidates.clear();
    policy.budget = RequestBudget {
        max_arguments: 0,
        max_total_argument_bytes: 0,
        max_declarations: 0,
        max_runtime_candidates: 0,
    };
    fixture
        .replace_fixture_context(&mut invocation, policy)
        .unwrap();
    normalize_intent(InstallCommandIntent::Install, invocation)
        .expect("a zero budget permits an empty corresponding input");
    fixture.assert_files_unchanged();
}

#[test]
fn r01_declaration_write_scope_is_enforced() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["python@3.11.9"]);
    let mut policy = *invocation.context.policy();
    policy.writes.declarations = false;
    fixture
        .replace_fixture_context(&mut invocation, policy)
        .unwrap();
    let request = normalize_intent(InstallCommandIntent::Install, invocation.clone())
        .expect("ordinary install does not require declaration-write permission");
    assert!(request.declaration_edit.is_none());
    for intent in [
        InstallCommandIntent::Use {
            target: fixture.target(false),
        },
        InstallCommandIntent::DepsAdd {
            target: fixture.target(true),
        },
        InstallCommandIntent::Upgrade {
            declaration_target: Some(fixture.target(false)),
        },
    ] {
        let error = normalize_intent(intent, invocation.clone()).unwrap_err();
        assert_eq!(error, OperationError::DeclarationWriteDenied);
    }
    fixture.assert_files_unchanged();
}

#[test]
fn r01_declaration_target_requires_a_document_and_field() {
    let fixture = Fixture::new();
    let invocation = fixture.invocation(&["python@3.11.9"]);
    let mut invalid_targets = Vec::new();
    for source in [
        ToolSource::Argument,
        ToolSource::Unknown,
        ToolSource::Environment("PRIVATE_CONFIG".into(), "secret-placeholder".into()),
        ToolSource::PakroToml(PathBuf::new()),
        ToolSource::PakroToml(PathBuf::from("relative/pakro.toml")),
    ] {
        invalid_targets.push(DeclarationTarget {
            source,
            field_path: vec!["tools".into()],
        });
    }
    for field_path in [Vec::new(), vec!["tools".into(), String::new()]] {
        invalid_targets.push(DeclarationTarget {
            source: ToolSource::PakroToml(fixture.config.clone()),
            field_path,
        });
    }
    for target in invalid_targets {
        let error =
            normalize_intent(InstallCommandIntent::Use { target }, invocation.clone()).unwrap_err();
        assert_eq!(error, OperationError::InvalidDeclarationTarget);
    }
    fixture.assert_files_unchanged();
}

#[test]
fn r01_new_absolute_target_does_not_require_existing_file() {
    let fixture = Fixture::new();
    let target = DeclarationTarget {
        source: ToolSource::PakroToml(fixture.directory.path().join("new-pakro.toml")),
        field_path: vec!["tools".into()],
    };
    let request = normalize_intent(
        InstallCommandIntent::Use {
            target: target.clone(),
        },
        fixture.invocation(&["python@3.11.9"]),
    )
    .expect("normalization validates a descriptor, not a current file owner");
    assert_eq!(request.declaration_edit.unwrap().target, target);
    fixture.assert_files_unchanged();
}

#[test]
fn r01_diagnostics_do_not_expose_raw_arguments_or_environment_values() {
    let fixture = Fixture::new();
    let secret = "private-secret-without-version";
    let argument = format!("python@3.11.9+{secret}");
    let mut invocation = fixture.invocation(&[&argument]);
    invocation.original_declarations[0].source =
        ToolSource::Environment("PRIVATE_CONFIG".into(), secret.into());
    assert!(!format!("{invocation:?}").contains(secret));
    let error = normalize_intent(InstallCommandIntent::Install, invocation).unwrap_err();
    assert_eq!(error, OperationError::InvalidArgument { index: 0 });
    assert!(!format!("{error:?}: {error}").contains(secret));
    fixture.assert_files_unchanged();
}

#[test]
fn r01_normalization_observes_context_cancellation() {
    let fixture = Fixture::new();
    let mut invocation = fixture.invocation(&["python@3.11.9"]);
    let cancellation = CancellationToken::new();
    invocation.context = fixture
        .context(*invocation.context.policy(), cancellation.clone())
        .unwrap();
    cancellation.cancel();
    let error = normalize_intent(InstallCommandIntent::Install, invocation).unwrap_err();
    assert_eq!(error, OperationError::Cancelled);
    fixture.assert_files_unchanged();
}
