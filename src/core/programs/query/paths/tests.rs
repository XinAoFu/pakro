use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{
    archive, archive_for, install_spec, install_spec_for, operation_context, prepare,
};
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ObservedProjectInputs, ReadPolicy, observe_project};
use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
use std::path::Path;
use std::time::{Duration, Instant};

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

/// One outer tempdir keeps the store and every project directory inside it,
/// so parallel tests never mutate a shared ancestor that path observations
/// pin from `/` downwards.
struct Workspace {
    _owner: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        Self {
            root: owner.path().canonicalize().unwrap(),
            _owner: owner,
        }
    }
    fn stage(&self) -> std::path::PathBuf {
        let stage = self.root.join("stage");
        std::fs::create_dir(&stage).unwrap();
        stage
    }
    fn project(&self, name: &str) -> std::path::PathBuf {
        let project = self.root.join(name);
        std::fs::create_dir(&project).unwrap();
        project
    }
}

#[tokio::test]
async fn f1_local_selection_reports_missing_and_ambiguity() {
    // The store opens roots without following links; the workspace root is
    // canonicalized once and every inner path is already symlink-free.
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let context = operation_context(&stage, true, Duration::from_secs(300));
    let lease = prepare(&spec, &context, &bytes).await.unwrap();

    // One exact published candidate is selected with its real payload paths.
    let project = workspace.project("project");
    let observed = declare(&project, "3.15.0");
    let query = open_query_context(&context, &observed).unwrap();
    let candidate = match select_existing(&query).unwrap() {
        SelectionReport::Selected(candidate) => candidate,
        other => panic!("expected one exact local candidate, got {other:?}"),
    };
    assert_eq!(candidate.key().fingerprint(), spec.key().fingerprint());
    assert_eq!(candidate.version(), "3.15.0");
    assert_eq!(candidate.root(), lease.root());
    let paths = selection_paths(&candidate);
    assert_eq!(paths.executable(), lease.entrypoint());
    assert_eq!(paths.bin_dir(), lease.entrypoint().parent().unwrap());
    assert_eq!(paths.root(), lease.root());

    // A complete declaration without any local instance is Missing, and the
    // report names the requested version instead of installing it.
    let elsewhere = workspace.project("elsewhere");
    let observed = declare(&elsewhere, "3.11.14");
    let query = open_query_context(&context, &observed).unwrap();
    match select_existing(&query).unwrap() {
        SelectionReport::Missing { requested } => assert_eq!(requested.as_str(), "3.11.14"),
        other => panic!("expected a missing report, got {other:?}"),
    }

    // A second real instance of the same version makes the choice ambiguous;
    // the report lists the instances instead of picking one for the caller.
    let host = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    let second_bytes = archive_for("3.15.0", &host, "second");
    let second = install_spec_for("3.15.0", &second_bytes, &host, "20261014");
    let second_lease = prepare(&second, &context, &second_bytes).await.unwrap();
    assert_ne!(second.key().fingerprint(), spec.key().fingerprint());
    let observed = declare(&project, "3.15.0");
    let query = open_query_context(&context, &observed).unwrap();
    match select_existing(&query).unwrap() {
        SelectionReport::Ambiguous {
            requested,
            candidates,
        } => {
            assert_eq!(requested.as_str(), "3.15.0");
            assert_eq!(candidates.len(), 2);
        }
        other => panic!("expected an ambiguous report, got {other:?}"),
    }

    // A damaged stored fact is Corrupt, not an uninstalled version.
    let catalog = stage
        .join("data/languages/.control/catalog")
        .join(format!("{}.json", second.key().filename()));
    std::fs::write(&catalog, b"{ damaged manifest").unwrap();
    match select_existing(&query).unwrap() {
        SelectionReport::Corrupt { .. } => {}
        other => panic!("expected a corrupt report, got {other:?}"),
    }

    // Read-only queries never register the observed projects.
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let views = repository.read_views();
    assert!(views.registered_projects().is_empty());
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    assert!(views.language_selection(&identity).is_none());
    drop(views);
    drop(repository);
    drop(second_lease);
    drop(lease);
}
