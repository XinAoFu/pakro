use std::time::{Duration, Instant};

use super::*;
use crate::programs::shared::errors::MissingOperationInput;
use crate::programs::shared::policy::{AllowedWrites, RequestBudget};
use crate::python_runtime::{
    ImplementationVersionSource, PythonImplementation, PythonMarkerValues, PythonVersion,
    ReleaseLevel,
};
use pakro_index::python_source::PythonRegistryId;

fn inputs<'a>(settings: &'a Settings, root: &Path) -> OperationContextInputs<'a> {
    OperationContextInputs {
        settings,
        policy: OperationPolicy {
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
                declarations: false,
            },
        },
        cancellation: CancellationToken::new(),
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    }
}

#[test]
fn r01_context_freezes_supplied_settings_policy_and_sources() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = Settings::default();
    settings.lockfile_mode = Some("generate".into());
    let registry =
        PythonRegistry::new("chosen", "https://index.example.test/simple/", None).unwrap();
    let mut supplied = inputs(&settings, root.path());
    let policy = supplied.policy;
    supplied.python_sources = Some(PythonSourceInputs {
        registries: vec![registry.clone()],
        policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("chosen").unwrap()),
    });
    // Caller-supplied data only: capture must not probe this path or claim that
    // this description is an installed/verified runtime.
    let version = PythonVersion {
        major: 3,
        minor: 11,
        micro: 9,
        release_level: ReleaseLevel::Final,
        serial: 0,
    };
    supplied.python_target_description = Some(PythonRuntimeDescriptor {
        schema: 1,
        observation_mode: "caller-description-not-verified".into(),
        probe_flags: Vec::new(),
        executable: root.path().join("unprobed/python"),
        implementation: PythonImplementation::CPython,
        language_version: version.clone(),
        implementation_version: version,
        implementation_version_source: ImplementationVersionSource::SysVersionInfo,
        markers: PythonMarkerValues {
            implementation_name: "cpython".into(),
            implementation_version: "3.11.9".into(),
            os_name: "posix".into(),
            platform_machine: "fixture".into(),
            platform_python_implementation: "CPython".into(),
            platform_release: "fixture".into(),
            platform_system: "fixture".into(),
            platform_version: "fixture".into(),
            python_full_version: "3.11.9".into(),
            python_version: "3.11".into(),
            sys_platform: "fixture".into(),
        },
        soabi: None,
        ext_suffix: None,
        abiflags: None,
        extension_suffixes: Vec::new(),
        gil_disabled: None,
        compatibility: None,
        sysconfig_paths: Default::default(),
    });
    let context = capture_operation_context(supplied).unwrap();
    settings.lockfile_mode = Some("merge".into());
    assert_eq!(
        context.settings().lockfile_mode.as_deref(),
        Some("generate")
    );
    assert_eq!(*context.policy(), policy);
    assert_eq!(context.python_sources().unwrap().registries, vec![registry]);
    assert_eq!(context.requested_data_root(), root.path().join("data"));
    assert_eq!(context.requested_cache_root(), root.path().join("cache"));
    assert_eq!(
        context.python_target_description().unwrap().executable,
        root.path().join("unprobed/python")
    );
    assert!(!format!("{context:?}").contains("index.example.test"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_context_missing_inputs_do_not_get_defaults() {
    let root = tempfile::tempdir().unwrap();
    let context = capture_operation_context(inputs(&Settings::default(), root.path())).unwrap();
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
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_context_clones_share_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::default();
    let supplied = inputs(&settings, root.path());
    let cancellation = supplied.cancellation.clone();
    let context = capture_operation_context(supplied).unwrap();
    let clone = context.clone();
    context.checkpoint().unwrap();
    cancellation.cancel();
    assert_eq!(context.checkpoint(), Err(OperationError::Cancelled));
    assert_eq!(clone.checkpoint(), Err(OperationError::Cancelled));
}

#[test]
fn r01_context_rejects_expired_capture() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::default();
    let mut supplied = inputs(&settings, root.path());
    supplied.policy.deadline = Instant::now() - Duration::from_secs(1);
    assert!(matches!(
        capture_operation_context(supplied),
        Err(OperationError::DeadlineExceeded)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_context_rejects_undeclared_source_policy() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::default();
    let mut supplied = inputs(&settings, root.path());
    supplied.python_sources = Some(PythonSourceInputs {
        registries: vec![
            PythonRegistry::new("chosen", "https://index.example.test/simple/", None).unwrap(),
        ],
        policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("absent").unwrap()),
    });
    assert!(matches!(
        capture_operation_context(supplied),
        Err(OperationError::InvalidSourceConfiguration)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_context_checkpoint_keeps_the_original_deadline() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::default();
    let mut supplied = inputs(&settings, root.path());
    let deadline = Instant::now() + Duration::from_secs(2);
    supplied.policy.deadline = deadline;
    let context = capture_operation_context(supplied).unwrap();
    let clone = context.clone();
    std::thread::sleep(
        deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
    );
    assert_eq!(context.policy().deadline, deadline);
    assert_eq!(clone.policy().deadline, deadline);
    assert_eq!(context.checkpoint(), Err(OperationError::DeadlineExceeded));
    assert_eq!(clone.checkpoint(), Err(OperationError::DeadlineExceeded));
}

#[test]
fn r01_context_translates_invalid_library_locations() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::default();
    let mut supplied = inputs(&settings, root.path());
    supplied.data_root = PathBuf::from("relative/data");
    assert!(matches!(
        capture_operation_context(supplied),
        Err(OperationError::InvalidLibraryLocations)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
