use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use std::path::Path;
use std::process::Command;

const CHILD: &str = "programs::python::p3_recovery_tests::update_install_crash_child_helper";

fn context(root: &Path) -> OperationContext {
    let mut settings = crate::config::Settings::default();
    settings.http_timeout = "20s".into();
    settings.http_download_timeout = "60s".into();
    settings.python.github_attestations = Some(false);
    capture_install_context_using(&settings, root.join("data"), root.join("cache")).unwrap()
}

fn observe(project: &Path, context: &OperationContext) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(
        project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "subprocess-only update crash/recovery helper; not an acceptance case"]
async fn update_install_crash_child_helper() {
    let root = std::path::PathBuf::from(std::env::var_os("PAKRO_TEST_P3_UPDATE_ROOT").unwrap());
    let mode = std::env::var("PAKRO_TEST_P3_UPDATE_MODE").unwrap();
    let context = context(&root);
    let project = root.join("project");
    if mode == "commit" {
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
        std::fs::write(
            project.join("pakro.lock"),
            "# original native input\nlockfile_version=4\n[future]\nstate='preserved'\n",
        )
        .unwrap();
        let bytes = archive("3.15.0");
        drop(
            prepare(&install_spec(&bytes), &context, &bytes)
                .await
                .unwrap(),
        );
    } else {
        assert_eq!(mode, "recover");
        assert!(
            super::super::shared::lock_transaction::own_pending_lock_commit(
                &context,
                &observe(&project, &context)
            )
            .unwrap()
        );
    }
    let observed = observe(&project, &context);
    let plan = project_plan_with_intent(context.clone(), observed, &[], PythonLockIntent::Update)
        .unwrap()
        .unwrap();
    if mode == "recover" {
        assert!(
            plan.preview_update().is_err(),
            "a pending preview must remain readonly and report recovery"
        );
    }
    let config = crate::config::Config::load_from_config_files(Default::default(), false)
        .await
        .unwrap();
    plan.install(config).await.unwrap();
    assert_ne!(
        mode, "commit",
        "the requested durable cutpoint was not reached"
    );
    let observed = observe(&project, &context);
    assert!(
        !super::super::shared::lock_transaction::own_pending_lock_commit(&context, &observed)
            .unwrap()
    );
    super::super::shared::lock_transaction::ensure_project_consistent(&context, &observed).unwrap();
    let model = crate::packages::python_resolution::lock_session::decode_root(
        &std::fs::read(project.join("pakro.lock")).unwrap(),
        Default::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(model.graphs().len(), 1);
    assert!(model.graphs()[0].nodes().is_empty());
    assert!(
        std::fs::read_to_string(project.join("pakro.lock"))
            .unwrap()
            .contains("state='preserved'")
    );
    assert_eq!(
        crate::libraries::project::repository::ProjectRepository::open_readonly(&context)
            .unwrap()
            .model()
            .registered_projects()
            .count(),
        1
    );
    println!("P3_UPDATE_RECOVERY_COMPLETE");
}

#[test]
fn p3_update_lock_real_interruption_reaches_recovery_and_revalidates_owned_results() {
    for stage in ["documents-staged", "before-catalog", "after-catalog"] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let command = |mode: &str| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", CHILD, "--ignored", "--nocapture"])
                .env("PAKRO_TEST_P3_UPDATE_ROOT", &root)
                .env("PAKRO_TEST_P3_UPDATE_MODE", mode)
                .env("PAKRO_TEST_P2_LOCK_CHILD", mode);
            command
        };
        let output = command("commit")
            .env("PAKRO_TEST_P2_LOCK_CUTPOINT", stage)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(86),
            "cutpoint {stage}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let output = command("recover")
            .env_remove("PAKRO_TEST_P2_LOCK_CUTPOINT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "recovery {stage}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("P3_UPDATE_RECOVERY_COMPLETE"));
        println!("P3_UPDATE_DURABLE_WINDOW_RECOVERED stage={stage}");
    }
}
