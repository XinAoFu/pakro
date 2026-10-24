use crate::artifact_snapshot::VerifiedArtifact;
use crate::artifact_snapshot::spool_verified_reader;
use crate::config::{FrozenConfigDiscovery, Settings};
use crate::libraries::language::repository::tests::{archive, install_spec};
use crate::libraries::language::repository::{LanguageRepository, LanguageState};
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ObservedProjectInputs, ReadPolicy, observe_project};
use crate::programs::install::runtime::prepare_language;
use eyre::Result;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A self-contained workspace (see F1 selection tests) plus the writable
/// context an explicit install needs.
struct Workspace {
    _owner: tempfile::TempDir,
    stage: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap().join("stage");
        std::fs::create_dir(&stage).unwrap();
        Self {
            _owner: owner,
            stage,
        }
    }
    fn context(&self) -> crate::programs::OperationContext {
        use crate::programs::{
            AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
            capture_operation_context,
        };
        capture_operation_context(OperationContextInputs {
            settings: &Settings::default(),
            policy: OperationPolicy {
                locked: false,
                offline: true,
                deadline: Instant::now() + Duration::from_secs(300),
                budget: RequestBudget {
                    max_arguments: 8,
                    max_total_argument_bytes: 4096,
                    max_declarations: 16,
                    max_runtime_candidates: 8,
                },
                writes: AllowedWrites {
                    libraries: true,
                    project_selection: true,
                    lockfile: false,
                    declarations: false,
                },
            },
            cancellation: tokio_util::sync::CancellationToken::new(),
            data_root: self.stage.join("data"),
            cache_root: self.stage.join("cache"),
            python_sources: None,
            python_target_description: None,
        })
        .unwrap()
    }
    fn project(&self, name: &str, version: &str) -> PathBuf {
        let project = self._owner.path().canonicalize().unwrap().join(name);
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            project.join("pakro.toml"),
            format!("[tools]\npython = \"{version}\"\n"),
        )
        .unwrap();
        project
    }
}

fn observe(root: &Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    observe_project(
        root,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60)),
    )
    .unwrap()
}

/// An acquire counter returning fixture bytes: reuse must never fetch again,
/// and a failing acquire proves no half-published state.
struct AcquireCounter {
    bytes: Vec<u8>,
    calls: RefCell<usize>,
    fail: bool,
}

impl AcquireCounter {
    fn acquire(&self, context: &crate::programs::OperationContext) -> Result<VerifiedArtifact> {
        *self.calls.borrow_mut() += 1;
        if self.fail {
            eyre::bail!("simulated acquisition failure");
        }
        let spec = install_spec(&self.bytes);
        spool_verified_reader(
            self.bytes.as_slice(),
            spec.plan().source().filename(),
            spec.plan().source().digest(),
            1024 * 1024,
            context.requested_cache_root(),
        )
    }
}

#[tokio::test]
async fn f2_exact_install_reuses_and_records_only_real_projects() {
    let workspace = Workspace::new();
    let context = workspace.context();
    let counter = AcquireCounter {
        bytes: archive("3.15.0"),
        calls: RefCell::new(0),
        fail: false,
    };

    // A project install fetches once, publishes Ready, and links the project.
    let project = workspace.project("project-a", "3.15.0");
    let observed = observe(&project);
    let spec = install_spec(&counter.bytes);
    drop(
        prepare_language(&spec, &context, || async { counter.acquire(&context) })
            .await
            .unwrap(),
    );
    let config = crate::config::Config::get().await.unwrap();
    crate::programs::python::project_plan(context.clone(), observed, &[])
        .unwrap()
        .unwrap()
        .install(config.clone())
        .await
        .unwrap();
    assert_eq!(*counter.calls.borrow(), 1);

    let spec = install_spec(&counter.bytes);
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    match repository.lookup(&spec).unwrap() {
        LanguageState::Ready(found) => found.check_binding().unwrap(),
        LanguageState::Missing => panic!("explicit install did not publish a Ready instance"),
    }
    let registered = ProjectRepository::open_readonly(&context)
        .unwrap()
        .read_views()
        .registered_projects();
    assert_eq!(registered.len(), 1, "a real project was observed");

    // A second install of the same exact request reuses without fetching.
    let observed = observe(&project);
    crate::programs::python::project_plan(context.clone(), observed, &[])
        .unwrap()
        .unwrap()
        .install(config.clone())
        .await
        .unwrap();
    drop(
        prepare_language(&spec, &context, || async { counter.acquire(&context) })
            .await
            .unwrap(),
    );
    assert_eq!(
        *counter.calls.borrow(),
        1,
        "an existing exact instance must be reused without acquisition"
    );

    // A global install publishes no project facts.
    crate::plugins::core::python::PythonPlugin::new()
        .install_shared_exact_in_context(&context, "3.15.0", config, None)
        .await
        .unwrap();
    let registered = ProjectRepository::open_readonly(&context)
        .unwrap()
        .read_views()
        .registered_projects();
    assert_eq!(
        registered.len(),
        1,
        "a global download must not fabricate a project"
    );
}

#[tokio::test]
async fn f2_failed_acquire_keeps_published_state() {
    let workspace = Workspace::new();
    let context = workspace.context();
    let counter = AcquireCounter {
        bytes: archive("3.15.0"),
        calls: RefCell::new(0),
        fail: true,
    };
    let project = workspace.project("project-a", "3.15.0");
    let _observed = observe(&project);
    let spec = install_spec(&counter.bytes);
    let failed = prepare_language(&spec, &context, || async { counter.acquire(&context) }).await;
    assert!(
        failed.is_err(),
        "a failed acquisition must not report success"
    );
    assert_eq!(*counter.calls.borrow(), 1);

    // Nothing was published and nothing was registered.
    let spec = install_spec(&counter.bytes);
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    match repository.lookup(&spec).unwrap() {
        LanguageState::Ready(_) => panic!("a failed acquisition published a Ready instance"),
        LanguageState::Missing => {}
    }
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    assert!(repository.read_views().registered_projects().is_empty());
}
