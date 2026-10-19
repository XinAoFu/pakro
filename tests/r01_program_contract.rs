//! Exercise the request facade from a separate crate. These tests do not route
//! product commands, observe an installed runtime, or claim an installation.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pakro::config::Settings;
use pakro::packages::PackageInstallRequest;
use pakro::programs::{
    AllowedWrites, DeclarationOperation, DeclarationTarget, InstallCommandIntent, InvocationInputs,
    InvocationSnapshot, MissingOperationInput, OperationContext, OperationContextInputs,
    OperationEffects, OperationError, OperationPolicy, OperationReport, OperationState,
    OperationTermination, RequestBudget, RequestLimit, RuntimeCandidate, capture_operation_context,
    normalize_intent,
};
use pakro::toolset::{ConfigScope, ToolSource};
use tokio_util::sync::CancellationToken;

const BASE_BYTES: &[u8] =
    b"[tools]\npython = \"3.11.9\"\n\n[packages.\"python@3.11.9\"]\ndemo = \"0.8.0\"\n";
const LOCAL_BYTES: &[u8] = b"[packages.\"python@3.11.9\"]\ndemo = \"0.9.0\"\n";
const LOCK_BYTES: &[u8] = b"# original exact lock\n";

struct Fixture {
    directory: tempfile::TempDir,
    base: PathBuf,
    local: PathBuf,
    lock: PathBuf,
    data_root: PathBuf,
    cache_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("pakro.toml");
        let local = directory.path().join("pakro.local.toml");
        let lock = directory.path().join("pakro.lock");
        let data_root = directory.path().join("data");
        let cache_root = data_root.join("cache");
        fs::write(&base, BASE_BYTES).unwrap();
        fs::write(&local, LOCAL_BYTES).unwrap();
        fs::write(&lock, LOCK_BYTES).unwrap();
        Self {
            directory,
            base,
            local,
            lock,
            data_root,
            cache_root,
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
        settings: &Settings,
        policy: OperationPolicy,
        cancellation: CancellationToken,
    ) -> OperationContext {
        capture_operation_context(OperationContextInputs {
            settings,
            policy,
            cancellation,
            data_root: self.data_root.clone(),
            cache_root: self.cache_root.clone(),
            python_sources: None,
            python_target_description: None,
        })
        .expect("explicit request context must be capturable without creating storage")
    }

    fn declarations(&self) -> Vec<(PackageInstallRequest, ToolSource)> {
        [(&self.base, "0.8.0"), (&self.local, "0.9.0")]
            .into_iter()
            .map(|(path, version)| {
                (
                    PackageInstallRequest {
                        runtime_name: "python".into(),
                        runtime_version: "3.11.9".into(),
                        name: "demo".into(),
                        version: version.into(),
                    },
                    ToolSource::PakroToml(path.clone()),
                )
            })
            .collect()
    }

    fn inputs(&self, context: OperationContext, arguments: Vec<String>) -> InvocationInputs {
        InvocationInputs {
            arguments,
            declarations: self.declarations(),
            runtime_candidates: vec![RuntimeCandidate {
                runtime_name: "python".into(),
                runtime_version: "3.11.9".into(),
                source: ToolSource::PakroToml(self.base.clone()),
            }],
            known_runtime_names: vec!["python".into()],
            scope: ConfigScope::LocalOnly,
            context,
        }
    }

    fn invocation(&self, context: OperationContext, arguments: Vec<String>) -> InvocationSnapshot {
        InvocationSnapshot::from_inputs(self.inputs(context, arguments))
            .expect("external callers must be able to supply descriptive invocation inputs")
    }

    fn assert_unchanged(&self) {
        assert_eq!(fs::read(&self.base).unwrap(), BASE_BYTES);
        assert_eq!(fs::read(&self.local).unwrap(), LOCAL_BYTES);
        assert_eq!(fs::read(&self.lock).unwrap(), LOCK_BYTES);
        assert!(!self.data_root.exists());
        assert!(!self.cache_root.exists());
        assert_eq!(fs::read_dir(self.directory.path()).unwrap().count(), 3);
    }
}

#[test]
fn r01_external_install_and_use_preserve_explicit_intents_and_owners() {
    let fixture = Fixture::new();
    let settings = Settings::default();
    let policy = fixture.policy();
    let context = fixture.context(&settings, policy, CancellationToken::new());
    let arguments = vec!["python@3.11.9+demo@1.0.0".to_owned()];
    let expected_declarations = fixture.declarations();
    let target = DeclarationTarget {
        source: ToolSource::PakroToml(fixture.local.clone()),
        field_path: vec!["tools".into()],
    };

    for intent in [
        InstallCommandIntent::Install,
        InstallCommandIntent::Use {
            target: target.clone(),
        },
    ] {
        let invocation = fixture.invocation(context.clone(), arguments.clone());
        let request = normalize_intent(intent.clone(), invocation)
            .expect("the external facade must normalize an explicit request");
        assert_eq!(request.intent(), &intent);
        assert_eq!(request.arguments(), arguments.as_slice());
        assert_eq!(request.scope(), ConfigScope::LocalOnly);
        assert_eq!(request.context().policy(), &policy);

        let actual_declarations = request.original_declarations().collect::<Vec<_>>();
        assert_eq!(actual_declarations.len(), expected_declarations.len());
        for ((actual_request, actual_owner), (expected_request, expected_owner)) in
            actual_declarations.iter().zip(&expected_declarations)
        {
            assert_eq!(*actual_request, expected_request);
            assert_eq!(*actual_owner, expected_owner);
        }

        let packages = request.packages().collect::<Vec<_>>();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].0.name, "demo");
        assert_eq!(packages[0].0.version, "1.0.0");
        assert_eq!(packages[0].1, &ToolSource::Argument);
        match intent {
            InstallCommandIntent::Install => assert!(request.declaration_edit().is_none()),
            InstallCommandIntent::Use { .. } => {
                let edit = request
                    .declaration_edit()
                    .expect("use must retain the exact edit target");
                assert_eq!(edit.target, target);
                assert_eq!(edit.operation, DeclarationOperation::SetSelection);
            }
            _ => unreachable!("the fixture only contains install and use"),
        }
        fixture.assert_unchanged();
    }
}

#[test]
fn r01_external_cancellation_after_capture_stops_normalization() {
    let fixture = Fixture::new();
    let settings = Settings::default();
    let cancellation = CancellationToken::new();
    let context = fixture.context(&settings, fixture.policy(), cancellation.clone());
    let invocation = fixture.invocation(context.clone(), vec!["python@3.11.9".into()]);
    cancellation.cancel();

    assert_eq!(context.checkpoint(), Err(OperationError::Cancelled));
    assert!(matches!(
        normalize_intent(InstallCommandIntent::Install, invocation),
        Err(OperationError::Cancelled)
    ));
    fixture.assert_unchanged();
}

#[test]
fn r01_external_invocation_rejects_declarations_over_budget() {
    let fixture = Fixture::new();
    let settings = Settings::default();
    let mut policy = fixture.policy();
    policy.budget.max_declarations = 1;
    let context = fixture.context(&settings, policy, CancellationToken::new());
    let inputs = fixture.inputs(context, vec!["python@3.11.9".into()]);
    assert_eq!(inputs.declarations.len(), 2);

    let result = InvocationSnapshot::from_inputs(inputs);
    assert!(matches!(
        result,
        Err(OperationError::ResourceLimit(RequestLimit::Declarations))
    ));
    fixture.assert_unchanged();
}

#[test]
fn r01_external_invocation_rejects_cancellation_after_context_capture() {
    let fixture = Fixture::new();
    let settings = Settings::default();
    let cancellation = CancellationToken::new();
    let context = fixture.context(&settings, fixture.policy(), cancellation.clone());
    cancellation.cancel();

    let inputs = fixture.inputs(context, vec!["python@3.11.9".into()]);
    let result = InvocationSnapshot::from_inputs(inputs);
    assert!(matches!(result, Err(OperationError::Cancelled)));
    fixture.assert_unchanged();
}

#[test]
fn r01_external_context_keeps_missing_facts_and_storage_descriptions() {
    let fixture = Fixture::new();
    let mut settings = Settings::default();
    settings.jobs = 2;
    let policy = fixture.policy();
    let context = fixture.context(&settings, policy, CancellationToken::new());
    settings.jobs = 9;

    assert_eq!(settings.jobs, 9);
    assert_eq!(context.settings().jobs, 2);
    assert_eq!(context.policy(), &policy);
    assert_eq!(context.requested_data_root(), fixture.data_root.as_path());
    assert_eq!(context.requested_cache_root(), fixture.cache_root.as_path());
    assert!(matches!(
        context.python_sources(),
        Err(OperationError::MissingInput(
            MissingOperationInput::PythonSources
        ))
    ));
    assert!(matches!(
        context.python_target_description(),
        Err(OperationError::MissingInput(
            MissingOperationInput::PythonTargetDescription
        ))
    ));
    assert_eq!(context.checkpoint(), Ok(()));
    fixture.assert_unchanged();
}

fn committed_report() -> OperationReport<&'static str> {
    OperationReport::new(
        OperationState::CommittedNeedsFinalization,
        OperationEffects {
            added: Vec::new(),
            reused: vec!["shared language"],
            associations_changed: vec!["project selection"],
            retained: vec!["another project's package"],
            removed: Vec::new(),
        },
        vec!["the committed selection still needs finalization".into()],
    )
}

#[test]
fn r01_external_report_retains_committed_facts_after_termination() {
    for (termination, expected_cancelled) in [
        (OperationTermination::Cancelled, true),
        (OperationTermination::Failed("finalization failed"), false),
    ] {
        let interrupted = committed_report().with_termination(termination);
        let report = interrupted.report();
        assert_eq!(report.state(), OperationState::CommittedNeedsFinalization);
        assert!(report.effects().added.is_empty());
        assert_eq!(report.effects().reused.as_slice(), &["shared language"]);
        assert_eq!(
            report.effects().associations_changed.as_slice(),
            &["project selection"]
        );
        assert_eq!(
            report.effects().retained.as_slice(),
            &["another project's package"]
        );
        assert!(report.effects().removed.is_empty());
        assert_eq!(
            report.warnings(),
            &["the committed selection still needs finalization".to_owned()]
        );
        match (interrupted.termination(), expected_cancelled) {
            (OperationTermination::Cancelled, true) => {}
            (OperationTermination::Failed(reason), false) => {
                assert_eq!(*reason, "finalization failed");
            }
            _ => panic!("termination must retain the caller's cancellation or failure"),
        }
    }
}
