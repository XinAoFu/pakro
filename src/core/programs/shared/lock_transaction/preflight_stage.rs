//! Explicit ignored producer for the CLI readonly-preflight regression.
use super::*;
use crate::config::{FrozenConfigDiscovery, Settings};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::*;
use std::time::{Duration, Instant};

#[ignore = "owned pending lock transaction for P2 CLI E2E"]
#[test]
fn stage_pending_preflight_readonly() {
    let project = PathBuf::from(
        std::env::var_os("PAKRO_P2_PENDING_PROJECT").expect("set owned project path"),
    )
    .canonicalize()
    .unwrap();
    // The libtest constructor resets the ordinary PAKRO_DATA_DIR/CACHE_DIR.
    // Dedicated inputs retain the actual CLI fixture's owned store locations.
    let data = PathBuf::from(
        std::env::var_os("PAKRO_P2_PENDING_DATA_ROOT").expect("set isolated CLI data root"),
    );
    let cache = PathBuf::from(
        std::env::var_os("PAKRO_P2_PENDING_CACHE_ROOT").expect("set isolated CLI cache root"),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let context = capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline,
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 64,
                max_runtime_candidates: 16,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: true,
                lockfile: true,
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
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_path_buf()];
    let observed = observe_project(&project, ReadPolicy::new(discovery, deadline)).unwrap();
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let lock = super::super::commit::open_project_lock(&writer, &identity).unwrap();
    let _guard = writer
        .control(ControlLimits::default())
        .unwrap()
        .acquire_project(&lock)
        .unwrap();
    let pending = PendingLockCommit::apply(&context, &observed, writer.clone(), identity.as_uuid(), Uuid::now_v7(),
        0, project_state(repository.model(), identity.as_uuid()), vec![pakro_store::Digest::Sha256([7;32]).to_string()],
        &project.join("pakro.lock"), b"# applied by the owned interrupted transaction\nlockfile_version=4\n[future]\nstate='new-but-not-committed'\n").unwrap();
    drop(pending);
    println!("P2_PENDING_PREFLIGHT_STAGED");
    println!(
        "P2_PENDING_PREFLIGHT_DATA_ROOT={}",
        context.requested_data_root().display()
    );
    println!(
        "P2_PENDING_PREFLIGHT_CACHE_ROOT={}",
        context.requested_cache_root().display()
    );
}
