//! Real-entry installation flows with an externally changed project lock.
//!
//! Every test drives `PythonProjectInstall::install` (the actual execution
//! entry) through `project_plan_with_intent`, mutating the lock file only
//! after the plan — the initial observation — was constructed.

use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const NATIVE: &str = "# original native input\nlockfile_version=4\n[future]\nstate='preserved'\n";
const USER_EDIT: &str = "# user edit\nlockfile_version=4\n[future]\nstate='user'\n";

fn install_context(root: &Path, locked: bool) -> OperationContext {
    let mut settings = crate::config::Settings::default();
    settings.http_timeout = "20s".into();
    settings.http_download_timeout = "60s".into();
    settings.python.github_attestations = Some(false);
    let initial =
        capture_install_context_using(&settings, root.join("data"), root.join("cache")).unwrap();
    if !locked {
        return initial;
    }
    let mut policy = *initial.policy();
    policy.locked = true;
    crate::programs::capture_operation_context(crate::programs::OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: Some(crate::programs::python::public_sources().unwrap()),
        python_target_description: None,
    })
    .unwrap()
}

fn observe(project: &Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(
        project,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60)),
    )
    .unwrap()
}

fn project_with(root: &Path, label: &str, lock: bool) -> PathBuf {
    let project = root.join(label);
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    if lock {
        std::fs::write(project.join("pakro.lock"), NATIVE).unwrap();
    }
    project
}

async fn prepare_language(context: &OperationContext) {
    let bytes = archive("3.15.0");
    drop(
        prepare(&install_spec(&bytes), context, &bytes)
            .await
            .unwrap(),
    );
}

async fn real_config() -> std::sync::Arc<crate::config::Config> {
    crate::config::Config::load_from_config_files(Default::default(), false)
        .await
        .unwrap()
}

fn plan(
    context: &OperationContext,
    project: &Path,
    intent: PythonLockIntent,
) -> PythonProjectInstall {
    project_plan_with_intent(context.clone(), observe(project), &[], intent)
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn fixed_lock_install_real_entry_uses_initial_lock_after_external_rewrite() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = install_context(&root, false);
    prepare_language(&context).await;
    let project = project_with(&root, "rewrite", true);
    let plan = plan(&context, &project, PythonLockIntent::Preserve);
    // External rewrite after the initial observation, before installation.
    std::fs::write(project.join("pakro.lock"), "not a lock document\n").unwrap();
    plan.install(real_config().await).await.unwrap();
    let final_lock = std::fs::read_to_string(project.join("pakro.lock")).unwrap();
    assert!(
        final_lock.contains("state='preserved'"),
        "the new document builds on the initial native input"
    );
    assert!(
        final_lock.contains("[python]"),
        "the installation generates its own graph section"
    );
    assert!(
        !final_lock.contains("not a lock document"),
        "later disk content never becomes install input"
    );
}

async fn no_writeback_when_strict(locked: bool) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    // The generating pass needs an unlocked context; the strict pass under
    // test uses the locked one against the same data root.
    let generate = install_context(&root, false);
    prepare_language(&generate).await;
    let project = project_with(&root, "strict", true);
    plan(&generate, &project, PythonLockIntent::Update)
        .install(real_config().await)
        .await
        .unwrap();
    assert!(
        std::fs::read_to_string(project.join("pakro.lock"))
            .unwrap()
            .contains("[python]")
    );
    // The second installation is strict (an exact model, optionally under
    // --locked) and must not write the lock back.
    let context = install_context(&root, locked);
    let plan = plan(&context, &project, PythonLockIntent::Preserve);
    std::fs::write(project.join("pakro.lock"), USER_EDIT).unwrap();
    use std::os::unix::fs::MetadataExt;
    let before = std::fs::metadata(project.join("pakro.lock")).unwrap();
    plan.install(real_config().await).await.unwrap();
    let after = std::fs::metadata(project.join("pakro.lock")).unwrap();
    assert_eq!(
        std::fs::read(project.join("pakro.lock")).unwrap(),
        USER_EDIT.as_bytes(),
        "the user's edited bytes are preserved"
    );
    assert_eq!(
        (before.dev(), before.ino()),
        (after.dev(), after.ino()),
        "no write-back may change the file identity"
    );
    assert_eq!(
        (before.mtime(), before.mtime_nsec()),
        (after.mtime(), after.mtime_nsec()),
        "no write-back may change the file times"
    );
}

#[tokio::test]
async fn fixed_lock_install_strict_normal_path_never_writes_lock_back() {
    no_writeback_when_strict(false).await;
}

#[tokio::test]
async fn fixed_lock_install_strict_locked_path_never_writes_lock_back() {
    no_writeback_when_strict(true).await;
}

#[tokio::test]
async fn fixed_lock_install_first_generation_overrides_a_lock_appearing_after_observation() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = install_context(&root, false);
    prepare_language(&context).await;
    let project = project_with(&root, "appeared", false);
    let plan = plan(&context, &project, PythonLockIntent::Preserve);
    std::fs::write(project.join("pakro.lock"), USER_EDIT).unwrap();
    plan.install(real_config().await).await.unwrap();
    let final_lock = std::fs::read_to_string(project.join("pakro.lock")).unwrap();
    assert!(
        final_lock.contains("[python]"),
        "the installation generates its own document"
    );
    assert!(
        !final_lock.contains("state='user'"),
        "the appeared file is replaced by this installation's prepared result"
    );
}

#[tokio::test]
async fn fixed_lock_install_update_lock_ignores_mid_flight_new_content() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = install_context(&root, false);
    prepare_language(&context).await;
    let project = project_with(&root, "update", true);
    let plan = plan(&context, &project, PythonLockIntent::Update);
    std::fs::write(project.join("pakro.lock"), USER_EDIT).unwrap();
    plan.install(real_config().await).await.unwrap();
    let final_lock = std::fs::read_to_string(project.join("pakro.lock")).unwrap();
    assert!(final_lock.contains("[python]"));
    assert!(
        final_lock.contains("state='preserved'"),
        "native input comes from the initial observation"
    );
    assert!(
        !final_lock.contains("state='user'"),
        "mid-flight content is neither adopted nor a refusal"
    );
}
