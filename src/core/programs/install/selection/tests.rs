use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ObservedProjectInputs, ReadPolicy, observe_project};
use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
use crate::programs::shared::report::OperationState;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A self-contained workspace: one outer tempdir keeps the store and every
/// project inside it, so parallel tests never mutate a shared ancestor that
/// path observations pin from `/` downwards.
struct Workspace {
    _owner: tempfile::TempDir,
    root: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        Self {
            root: owner.path().canonicalize().unwrap(),
            _owner: owner,
        }
    }
    fn stage(&self) -> PathBuf {
        let stage = self.root.join("stage");
        std::fs::create_dir(&stage).unwrap();
        stage
    }
    fn project(&self, name: &str) -> PathBuf {
        let project = self.root.join(name);
        std::fs::create_dir(&project).unwrap();
        project
    }
}

/// Staging plus the directory-selection commit need both library and project
/// selection writes in one frozen context.
fn selection_context(stage: &Path, duration: Duration) -> crate::programs::OperationContext {
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + duration,
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
        data_root: stage.join("data"),
        cache_root: stage.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}

fn read_policy(root: &Path) -> ReadPolicy {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60))
}

fn declare(root: &Path, version: &str) -> ObservedProjectInputs {
    std::fs::write(
        root.join("pakro.toml"),
        format!("[tools]\npython = \"{version}\"\n"),
    )
    .unwrap();
    observe_project(root, read_policy(root)).unwrap()
}

fn selected<'a>(
    context: &crate::programs::OperationContext,
    observed: &'a ObservedProjectInputs,
) -> crate::programs::query::snapshot::LanguageCandidate {
    let query = open_query_context(context, observed).unwrap();
    match select_existing(&query).unwrap() {
        SelectionReport::Selected(candidate) => candidate,
        other => panic!("expected one exact local candidate, got {other:?}"),
    }
}

fn find_projects_catalog(data_root: &Path) -> Option<PathBuf> {
    let control = data_root.join("projects/.control/catalog");
    if control.is_dir() {
        return Some(control);
    }
    None
}

#[tokio::test]
async fn f1_language_only_selection_survives_restart() {
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let context = selection_context(&stage, Duration::from_secs(300));
    let lease = prepare(&spec, &context, &bytes).await.unwrap();

    // Project A declares the exact published version and is selected.
    let project = workspace.project("project-a");
    let observed = declare(&project, "3.15.0");
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let candidate = selected(&context, &observed);
    assert_eq!(candidate.key().fingerprint(), spec.key().fingerprint());
    let report = apply_directory_selection(&context, &observed, &candidate).unwrap();
    assert_eq!(report.state(), OperationState::Committed);

    // Reopen from disk only: the language reference exists, carries no fake
    // layout, and the payload of the shared library is untouched.
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let views = repository.read_views();
    let selection = views
        .language_selection(&identity)
        .expect("committed selection disappeared after restart");
    assert_eq!(selection.language(), &spec.key().fingerprint());
    assert_eq!(selection.generation_allocation(), None);
    assert!(selection.package_references().is_empty());
    drop(views);
    drop(repository);
    lease.check_binding().unwrap();

    // Project B, a different directory with the same declaration, selects the
    // same language identity without a second payload.
    let other = workspace.project("project-b");
    let observed_b = declare(&other, "3.15.0");
    let identity_b = ProjectIdentity::from_observation(&observed_b).unwrap();
    assert_ne!(identity_b.as_uuid(), identity.as_uuid());
    let candidate_b = selected(&context, &observed_b);
    assert_eq!(candidate_b.key().fingerprint(), spec.key().fingerprint());
    apply_directory_selection(&context, &observed_b, &candidate_b).unwrap();

    // A full reopen keeps both projects' references to the one language.
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let views = repository.read_views();
    let registered = views.registered_projects();
    assert_eq!(registered.len(), 2);
    for identity in [&identity, &identity_b] {
        let selection = views
            .language_selection(identity)
            .expect("registered project lost its selection");
        assert_eq!(selection.language(), &spec.key().fingerprint());
        assert_eq!(selection.generation_allocation(), None);
        assert!(selection.package_references().is_empty());
    }
}

#[tokio::test]
async fn f1_failed_selection_keeps_old_references() {
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let context = selection_context(&stage, Duration::from_secs(300));
    prepare(&spec, &context, &bytes).await.unwrap();

    // Commit one selection; its reference is the old reference to keep.
    let project = workspace.project("project");
    let observed = declare(&project, "3.15.0");
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let candidate = selected(&context, &observed);
    apply_directory_selection(&context, &observed, &candidate).unwrap();

    // A declaration that changed after the observation must not commit.
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython = \"3.15.0\"\n# touched\n",
    )
    .unwrap();
    assert!(
        apply_directory_selection(&context, &observed, &candidate).is_err(),
        "a stale observation must not commit a selection"
    );

    // A blocked catalog commit fails and keeps the previous reference.
    std::fs::write(project.join("pakro.toml"), "[tools]\npython = \"3.15.0\"\n").unwrap();
    let fresh = declare(&project, "3.15.0");
    let candidate_fresh = selected(&context, &fresh);
    let catalog = find_projects_catalog(&stage.join("data"))
        .expect("the projects catalog exists after a committed selection");
    let original = std::fs::metadata(&catalog).unwrap().permissions();
    std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o500)).unwrap();
    let blocked = apply_directory_selection(&context, &fresh, &candidate_fresh);
    std::fs::set_permissions(&catalog, original).unwrap();
    assert!(
        blocked.is_err(),
        "a failed catalog commit must not report success"
    );

    // The old committed reference is exactly what survives both failures.
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let selection = repository
        .read_views()
        .language_selection(&identity)
        .expect("old selection reference was lost");
    assert_eq!(selection.language(), &spec.key().fingerprint());
    assert_eq!(selection.generation_allocation(), None);
    assert!(selection.package_references().is_empty());
}

// Staging helper for tests/e2e/cli/test_f1_directory_selection. It publishes
// one real fixture-language instance into the isolated data root given by
// PAKRO_F1_STAGE_DATA_ROOT, so the e2e run exercises the genuine hook path
// without a network or a user-facing install command (F1 installs nothing).
#[ignore = "env-driven staging for tests/e2e/cli/test_f1_directory_selection"]
#[tokio::test]
async fn f1_e2e_stage_shared_language() {
    use crate::config::Settings;
    use crate::libraries::language::repository::tests::{archive_for, install_spec_for};
    use crate::libraries::language::repository::{LanguageRepository, LanguageState};
    use crate::plugins::core::python::native::RuntimePlatform;
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    use tokio_util::sync::CancellationToken;

    let root = std::path::PathBuf::from(
        std::env::var_os("PAKRO_F1_STAGE_DATA_ROOT")
            .unwrap_or_else(|| panic!("PAKRO_F1_STAGE_DATA_ROOT must name the isolated data root")),
    );
    // The store opens roots without following links; caller-supplied paths
    // like macOS /tmp contain symlinked components, so bind the canonical
    // location (the root itself when it exists, else its existing parent).
    let root = match root.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => {
            let parent = root.parent().expect("an absolute root has a parent");
            let name = root.file_name().expect("the root has a leaf name");
            parent
                .canonicalize()
                .expect("the staging parent directory must exist")
                .join(name)
        }
    };
    let version = std::env::var("PAKRO_F1_STAGE_VERSION").unwrap_or_else(|_| "3.15.0".into());
    let (arch, platform) = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => ("aarch64", RuntimePlatform::Macos),
        ("x86_64", "macos") => ("x86_64", RuntimePlatform::Macos),
        ("aarch64", "linux") => ("aarch64", RuntimePlatform::LinuxGnu),
        ("x86_64", "linux") => ("x86_64", RuntimePlatform::LinuxGnu),
        other => panic!("the f1 e2e fixture archive has no target for {other:?}"),
    };
    let target =
        crate::plugins::core::python::native::RuntimeArchiveTarget::new(arch, platform).unwrap();
    let bytes = archive_for(&version, &target, "");
    let spec = install_spec_for(&version, &bytes, &target, "20261013");
    // Bind the context the way the real CLI does: the stage root IS the data
    // root, so the store lands at <data root>/languages like in production.
    // The fixture helper used by unit tests nests an extra "data" directory
    // that would not match the product layout the e2e asserts against.
    let context = capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: std::time::Instant::now() + Duration::from_secs(600),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: false,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: CancellationToken::new(),
        data_root: root.clone(),
        cache_root: root.join(".f1-stage-cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let lease = prepare(&spec, &context, &bytes).await.unwrap();
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    match repository.lookup(&spec).unwrap() {
        LanguageState::Ready(found) => {
            found.check_binding().unwrap();
        }
        LanguageState::Missing => {
            panic!("staged language is not Ready after publication");
        }
    }
    println!(
        "f1 e2e staged {} at {}",
        spec.key().filename(),
        lease.root().display()
    );
}
