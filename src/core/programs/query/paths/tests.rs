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
    drop(query);
    drop(observed);
    // Current tool_alias overrides deprecated alias and keeps the exact pin
    // under its logical lock key rather than ignoring the alias commitment.
    std::fs::write(
        project.join("pakro.toml"),
        "[alias.py]\nbackend='asdf:other'\n[tool_alias.py]\nbackend='core:python'\n[tools]\npy='3.15.0'\n",
    )
    .unwrap();
    let alias_lock = lock(first.plan().source().digest()).replace("tools.python", "tools.py");
    std::fs::write(project.join("pakro.lock"), &alias_lock).unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    assert!(
        matches!(select_existing(&query).unwrap(), SelectionReport::Selected(candidate) if *candidate.key() == first.key())
    );
    drop(query);
    drop(observed);
    let conflicting = lock(second.plan().source().digest()).replacen("lockfile_version=3\n", "", 1);
    std::fs::write(
        project.join("pakro.lock"),
        format!("{alias_lock}\n{conflicting}"),
    )
    .unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    assert!(
        select_existing(&query).is_err(),
        "conflicting applicable alias pins must fail explicitly"
    );
}

#[test]
fn p1_frozen_environment_and_backend_aliases_share_declaration_selection() {
    use crate::libraries::project::snapshot::ReadScope;
    use crate::programs::query::snapshot::effective_python_declaration;
    let workspace = Workspace::new();
    let project = workspace.project("alias");
    std::fs::write(
        project.join("pakro.toml"),
        "[alias.py]\nbackend='core:python'\n[tools]\npy='3.15.0'\n",
    )
    .unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    assert_eq!(
        effective_python_declaration(&observed).as_deref(),
        Some("3.15.0")
    );
    drop(observed);
    let mut policy = read_policy(&project);
    policy
        .environment
        .insert("PAKRO_PYTHON_VERSION".into(), "3.14.0".into());
    let observed = observe_project(&project, policy.clone()).unwrap();
    assert_eq!(
        effective_python_declaration(&observed).as_deref(),
        Some("3.14.0")
    );
    drop(observed);
    policy.scope = ReadScope::LocalOnly;
    let observed = observe_project(&project, policy).unwrap();
    assert_eq!(
        effective_python_declaration(&observed).as_deref(),
        Some("3.15.0")
    );
    drop(observed);
    std::fs::write(
        project.join("pakro.toml"),
        "[alias.python]\nbackend='asdf:other'\n[tools]\npython='3.15.0'\n",
    )
    .unwrap();
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    assert!(effective_python_declaration(&observed).is_none());
    drop(observed);
    for (aliases, expected) in [
        (
            "[alias.py]\nbackend='asdf:other'\n[tool_alias.py]\nbackend='core:python'\n",
            Some("3.15.0"),
        ),
        (
            "[alias.py]\nbackend='core:python'\n[tool_alias.py]\nbackend='asdf:other'\n",
            None,
        ),
        // Current entries replace the whole deprecated entry; a missing
        // backend must not recover the old Python backend from that entry.
        (
            "[alias.py]\nbackend='core:python'\n[tool_alias.py.versions]\nstable='3.15.0'\n",
            None,
        ),
    ] {
        std::fs::write(
            project.join("pakro.toml"),
            format!("{aliases}[tools]\npy='3.15.0'\n"),
        )
        .unwrap();
        let observed = observe_project(&project, read_policy(&project)).unwrap();
        assert_eq!(effective_python_declaration(&observed).as_deref(), expected);
    }
}

#[tokio::test]
async fn p1_local_prefix_ambiguity_and_invalid_filters_never_choose_a_build() {
    use crate::programs::query::snapshot::select_target;
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let context = operation_context(&stage, true, Duration::from_secs(60));
    let target = crate::plugins::core::python::native::RuntimeArchiveTarget::new(
        "aarch64",
        crate::plugins::core::python::native::RuntimePlatform::Macos,
    )
    .unwrap();
    for (build, marker) in [("20261013", "first"), ("20261014", "second")] {
        let bytes = archive_for("3.15.0", &target, marker);
        let spec = install_spec_for("3.15.0", &bytes, &target, build);
        drop(prepare(&spec, &context, &bytes).await.unwrap());
    }
    let project = workspace.project("no-declaration");
    let observed = observe_project(&project, read_policy(&project)).unwrap();
    let query = open_query_context(&context, &observed).unwrap();
    for prefix in ["3", "3.15", "3.15.0"] {
        assert!(
            matches!(select_target(&query, Some(prefix)).unwrap(), SelectionReport::Ambiguous { candidates, .. } if candidates.len() == 2)
        );
    }
    for invalid in ["latest", "03", "3.", "3.15.*", "prefix:3.15"] {
        assert!(matches!(
            select_target(&query, Some(invalid)).unwrap(),
            SelectionReport::InvalidDeclaration { .. }
        ));
    }
    assert!(matches!(
        select_target(&query, Some("3.15.0+20261013")).unwrap(),
        SelectionReport::Selected(_)
    ));
}

#[test]
fn p1_backend_option_alias_remains_python_and_is_rejected_without_inventory() {
    use crate::programs::query::snapshot::{
        QueryError, effective_python_declaration, python_tool_name,
    };
    let workspace = Workspace::new();
    let stage = workspace.stage();
    let context = operation_context(&stage, false, Duration::from_secs(30));
    let project = workspace.project("alias-options");
    for section in ["alias", "tool_alias"] {
        std::fs::write(
            project.join("pakro.toml"),
            format!("[{section}.py]\nbackend='core:python[compile=true]'\n[tools]\npy='3.15.0'\n"),
        )
        .unwrap();
        let observed = observe_project(&project, read_policy(&project)).unwrap();
        assert!(python_tool_name(&observed, "py"));
        assert!(
            effective_python_declaration(&observed).is_some(),
            "unsupported options must not erase the Python declaration"
        );
        let query = open_query_context(&context, &observed).unwrap();
        assert!(matches!(
            select_existing(&query),
            Err(QueryError::UnsupportedBackendOptions(_))
        ));
        let mut policy = read_policy(&project);
        policy
            .environment
            .insert("PAKRO_PY_VERSION".into(), "3.15.0".into());
        let overridden = observe_project(&project, policy).unwrap();
        let declaration = effective_python_declaration(&overridden).unwrap();
        assert!(
            crate::programs::shared::exact_version::exact_python_request(&declaration).is_err(),
            "installation preparation must reject option-bearing environment aliases before runtime acquisition"
        );
        assert!(declaration.contains("unsupported Python backend options"));
        let query = open_query_context(&context, &overridden).unwrap();
        assert!(matches!(
            select_existing(&query),
            Err(QueryError::UnsupportedBackendOptions(_))
        ));
    }
    assert!(
        !context.requested_data_root().exists(),
        "option rejection must precede library access or initialization"
    );
}
