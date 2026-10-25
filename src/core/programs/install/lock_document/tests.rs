use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::programs::*;
use std::time::{Duration, Instant};

#[tokio::test]
async fn p2_runtime_overlay_preserves_native_comments_unknown_fields_and_provenance() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let context = capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(60),
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
        data_root: root.join("data"),
        cache_root: root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let archive = archive("3.15.0");
    let spec = install_spec(&archive);
    let language = prepare(&spec, &context, &archive).await.unwrap();
    let inventories_before_mapping = crate::libraries::language::lease::full_inventory_checks();
    let source = language.source_pin().unwrap();
    assert_eq!(language.lock_version().unwrap(), "3.15.0+20261013");
    crate::programs::install::lock::group_key(&language).unwrap();
    crate::programs::install::lock::runtime_pin(&language).unwrap();
    assert_eq!(
        crate::libraries::language::lease::full_inventory_checks(),
        inventories_before_mapping,
        "portable lock descriptors must not repeat a complete runtime hash walk"
    );
    let target = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let original = format!(
        "# native header\nlockfile_version=4 # version comment\n\n[[tools.python]]\nversion='3.15.0' # runtime comment\nbackend='core:python' # backend comment\nspecifiers=['3.15.0'] # specifier comment\nfuture_runtime='runtime stays'\n\n[tools.python.platforms.{target}]\nurl='{}' # URL comment\nchecksum='{}' # checksum comment\nprovenance='github-attestations'\nfuture_target='target stays'\n\n[future]\nwhen=2026-10-25T00:00:00Z # future comment\n",
        source.url.unwrap(),
        source.checksum.unwrap(),
    );
    let path = project.join("pakro.lock");
    std::fs::write(&path, &original).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let observed = observe_project(
        &project,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let model = PythonLockModel::from_parts(&[], &[], Default::default()).unwrap();
    let bytes = prepare_document(&bound, &observed, &language, model).unwrap();
    assert_eq!(
        crate::libraries::language::lease::full_inventory_checks(),
        inventories_before_mapping,
        "preparing document data must not re-run the surrounding lease boundary checks"
    );
    let output = std::str::from_utf8(&bytes).unwrap();
    let parsed = crate::lockfile::Lockfile::parse_observed(&bytes).unwrap();
    assert_eq!(parsed.tools()["python"][0].version, "3.15.0+20261013");
    assert!(output.contains("# runtime comment"));
    for retained in [
        "# native header",
        "# version comment",
        "# backend comment",
        "# specifier comment",
        "# URL comment",
        "# checksum comment",
        "provenance='github-attestations'",
        "future_runtime='runtime stays'",
        "future_target='target stays'",
        "when=2026-10-25T00:00:00Z # future comment",
    ] {
        assert!(
            output.contains(retained),
            "missing retained field/comment {retained}"
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let model =
        crate::packages::python_resolution::lock_session::decode_root(&bytes, Default::default())
            .unwrap()
            .unwrap();
    validate_declarations(&observed, &model).unwrap();
}
