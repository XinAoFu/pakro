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

#[test]
fn f1_python_selector_and_options_are_not_silently_ignored() {
    for declaration in [
        "[tools]\npython={path='3.15.0'}\n",
        "[tools]\npython={prefix='3.15.0'}\n",
        "[tools]\npython={ref='3.15.0'}\n",
        "[tools]\npython={version='3.15.0',compile=true}\n",
        "[tools]\npython={version='3.15.0',postinstall='echo ignored'}\n",
    ] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("pakro.toml"), declaration).unwrap();
        let observed = observe_project(&root, read_policy(&root)).unwrap();
        let raw =
            crate::programs::query::snapshot::effective_python_declaration(&observed).unwrap();
        assert!(
            crate::programs::shared::exact_version::exact_python_request(&raw).is_err(),
            "unsupported Python declaration was silently accepted: {declaration}"
        );
    }
}

#[tokio::test]
async fn f1_observed_runtime_pin_selects_only_the_committed_artifact() {
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let context = operation_context(&stage, true, Duration::from_secs(60));
    let target = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    let first_bytes = archive_for("3.15.0", &target, "first");
    let first = install_spec_for("3.15.0", &first_bytes, &target, "20261013");
    drop(prepare(&first, &context, &first_bytes).await.unwrap());
    let second_bytes = archive_for("3.15.0", &target, "second");
    let second = install_spec_for("3.15.0", &second_bytes, &target, "20261014");
    drop(prepare(&second, &context, &second_bytes).await.unwrap());
    let project = workspace.project("pinned");
    declare(&project, "3.15.0");
    let platform = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let lock = |digest: pakro_store::Digest| {
        format!(
            "lockfile_version=3\n[[tools.python]]\nversion='3.15.0'\nbackend='core:python'\nspecifiers=['3.15.0']\n[tools.python.platforms.{platform}]\nurl='{}'\nchecksum='{}'\n",
            first.plan().source().url().as_str().replace('+', "%2B"),
            digest.to_string().replacen('-', ":", 1)
        )
    };
    std::fs::write(
        project.join("pakro.lock"),
        lock(first.plan().source().digest()),
    )
    .unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    match select_existing(&query).unwrap() {
        SelectionReport::Selected(candidate) => assert_eq!(*candidate.key(), first.key()),
        other => {
            panic!("a known runtime source pin must select its one actual artifact, got {other:?}")
        }
    }
    assert_eq!(
        std::fs::read_dir(&project).unwrap().count(),
        2,
        "query must not create a side lock or edit project inputs"
    );
    drop(query);
    drop(observed);
    std::fs::write(
        project.join("pakro.lock"),
        lock(pakro_store::Digest::Sha256([99; 32])),
    )
    .unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    assert!(matches!(
        select_existing(&query).unwrap(),
        SelectionReport::Missing { .. }
    ));
}
