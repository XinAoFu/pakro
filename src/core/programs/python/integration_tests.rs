//! Exercise the production facade with the original real local HTTP producer.
use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::identity::ProjectIdentity;
use crate::libraries::project::repository::ProjectRepository;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_resolution::source::registry_depot::ProjectRegistryFixture;
use crate::programs::query::snapshot::{SelectionReport, open_query_context, select_existing};
use std::time::{Duration, Instant};

fn observe(project: &std::path::Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(
        project,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(120)),
    )
    .unwrap()
}
fn context(stage: &std::path::Path, fixture: &ProjectRegistryFixture) -> OperationContext {
    use crate::programs::*;
    use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};
    let registry = (*fixture.endpoint().project.registry).clone();
    let settings = crate::config::Settings {
        http_timeout: "30s".into(),
        http_download_timeout: "30m".into(),
        ..Default::default()
    };
    capture_operation_context(OperationContextInputs {
        settings: &settings,
        policy: OperationPolicy {
            locked: false,
            offline: false,
            deadline: Instant::now() + Duration::from_secs(120),
            budget: RequestBudget {
                max_arguments: 16,
                max_total_argument_bytes: 4096,
                max_declarations: 32,
                max_runtime_candidates: 16,
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
        python_sources: Some(PythonSourceInputs {
            registries: vec![registry],
            policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap()),
        }),
        python_target_description: None,
    })
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_project_facade_reuses_real_closure_and_preserves_failure() {
    const TEST: &str = "programs::python::integration_tests::f3_project_facade_reuses_real_closure_and_preserves_failure";
    const CHILD: &str = "PAKRO_F3_LOCAL_FACADE_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        // Exercise the application's real proxy policy in a child whose
        // loopback fixtures explicitly bypass inherited/system proxies.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1");
        for variable in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(variable);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "isolated facade fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let projects: [std::path::PathBuf; 3] = ["a", "b", "c"].map(|name| {
        let p = root.join(name);
        std::fs::create_dir(&p).unwrap();
        p
    });
    let fixture = ProjectRegistryFixture::reversed_name_dependency();
    let context = context(&stage, &fixture);
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    drop(prepare(&spec, &context, &bytes).await.unwrap());
    let declaration = "[tools]\npython='3.15.0'\n[packages.\"python@3.15.0\"]\nz-root='1.0'\n";
    for project in &projects[..2] {
        std::fs::write(project.join("pakro.toml"), declaration).unwrap();
    }
    let observed_a = observe(&projects[0]);
    let roots = project_roots(&observed_a, "3.15.0", &[]).unwrap();
    install_project_packages(&context, &observed_a, &roots)
        .await
        .unwrap();
    let requests = fixture.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    let identity_a = ProjectIdentity::from_observation(&observed_a).unwrap();
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let graph_a = repository
        .read_views()
        .package_graph(&identity_a)
        .unwrap()
        .unwrap();
    drop(repository);
    assert_eq!(graph_a.nodes().len(), 2);
    assert_eq!(graph_a.edges().len(), 1);
    let edge = &graph_a.edges()[0];
    assert_eq!(graph_a.nodes()[edge.from()].name(), "z-root");
    assert_eq!(graph_a.nodes()[edge.to()].name(), "a-dep");
    let observed_b = observe(&projects[1]);
    let roots = project_roots(&observed_b, "3.15.0", &[]).unwrap();
    install_project_packages(&context, &observed_b, &roots)
        .await
        .unwrap();
    assert_eq!(
        fixture.requests(),
        requests,
        "an existing complete compatible closure must not fetch again"
    );
    let identity_b = ProjectIdentity::from_observation(&observed_b).unwrap();
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let graph_b = repository
        .read_views()
        .package_graph(&identity_b)
        .unwrap()
        .unwrap();
    assert_eq!(graph_a.layout(), graph_b.layout());
    assert_eq!(graph_a.nodes(), graph_b.nodes());
    drop(repository);
    for project in &projects[..2] {
        assert_eq!(
            std::fs::read_to_string(project.join("pakro.toml")).unwrap(),
            declaration
        );
        assert!(!project.join("pakro.lock").exists());
    }

    let initial = "[tools]\npython='3.15.0'\n";
    std::fs::write(projects[2].join("pakro.toml"), initial).unwrap();
    let observed_c = observe(&projects[2]);
    let identity_c = ProjectIdentity::from_observation(&observed_c).unwrap();
    let query = open_query_context(&context, &observed_c).unwrap();
    let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
        panic!("language missing")
    };
    crate::programs::install::selection::apply_directory_selection(
        &context,
        &observed_c,
        &candidate,
    )
    .unwrap();
    drop(query);
    drop(observed_c);
    let invalid = declaration.replace("z-root='1.0'", "z-root='2.0'");
    std::fs::write(projects[2].join("pakro.toml"), &invalid).unwrap();
    let observed_c = observe(&projects[2]);
    let roots = project_roots(&observed_c, "3.15.0", &[]).unwrap();
    assert!(
        install_project_packages(&context, &observed_c, &roots)
            .await
            .is_err()
    );
    let repository = ProjectRepository::open_readonly(&context).unwrap();
    let old = repository
        .read_views()
        .language_selection(&identity_c)
        .unwrap();
    assert_eq!(old.language(), &spec.key().fingerprint());
    assert!(old.package_references().is_empty());
    assert_eq!(
        std::fs::read_to_string(projects[2].join("pakro.toml")).unwrap(),
        invalid
    );
    assert!(!projects[2].join("pakro.lock").exists());
}

/// E2E-only producer for the already authorized real PBS archive. It publishes
/// through the same R06 owner and never downloads or invokes a manager.
#[ignore = "isolated real-language stage for F3 CLI E2E"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_e2e_stage_real_language() {
    use crate::artifact_snapshot::spool_verified_reader;
    use crate::programs::*;
    let data = std::path::PathBuf::from(
        std::env::var_os("PAKRO_F3_STAGE_DATA_ROOT").expect("PAKRO_F3_STAGE_DATA_ROOT is required"),
    );
    let cache = std::path::PathBuf::from(
        std::env::var_os("PAKRO_CACHE_DIR").expect("PAKRO_CACHE_DIR is required"),
    );
    let context = capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(240),
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
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: data,
        cache_root: cache,
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let filename = "cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz";
    let archive = std::path::PathBuf::from(
        std::env::var_os("PAKRO_TEST_PYTHON_ARCHIVE")
            .expect("set PAKRO_TEST_PYTHON_ARCHIVE to the verified local Python 3.13.16 archive"),
    );
    let digest: pakro_store::Digest =
        "sha256-16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e"
            .parse()
            .unwrap();
    let (_, _, target, flavor) = crate::plugins::core::python::native::pbs_filename(filename)
        .unwrap()
        .unwrap();
    let info = crate::lockfile::PlatformInfo {
        url: Some(format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{filename}"
        )),
        checksum: Some(digest.to_string().replacen('-', ":", 1)),
        ..Default::default()
    };
    let plan = crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_locked(
        "3.13.16", &target, flavor, &info,
    )
    .unwrap();
    let spec = crate::libraries::language::repository::LanguageInstallSpec::from_plan(&plan, false)
        .unwrap();
    let lease = crate::programs::install::runtime::prepare_language(&spec, &context, || async {
        spool_verified_reader(
            std::fs::File::open(&archive)?,
            filename,
            digest,
            64 * 1024 * 1024,
            context.requested_cache_root(),
        )
    })
    .await
    .unwrap();
    lease.check_binding().unwrap();
    println!("F3_STAGED_PYTHON={}", lease.entrypoint().display());
}
