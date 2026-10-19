//! Protocol fixtures exercise the owned extraction boundary, not real Python.
use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use crate::libraries::language::publication::verify_language;
use crate::libraries::language::repository::{
    LanguageBuildOwner, LanguageInstallSpec, LanguageRepository, LanguageReservation,
};
use crate::programs::{
    AllowedWrites, OperationContext, OperationContextInputs, OperationPolicy, RequestBudget,
    capture_operation_context,
};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

fn context(fixture: &Fixture) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + Duration::from_secs(10),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 8,
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
        data_root: fixture.root.join("data"),
        cache_root: fixture.root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}

fn dynamic_script(fixture: &Fixture) -> Vec<u8> {
    fixture.script(&fixture.observed(false, "3.15.0", "3.15.0", "python"))
}

fn populated_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.make_archive(vec![
        executable("bundle/bin/python", dynamic_script(&fixture)),
        Entry::File(
            "bundle/lib/python3.15/marker".into(),
            b"owned fixture marker".to_vec(),
            0o644,
        ),
        Entry::Link("bundle/bin/python3".into(), PathBuf::from("python")),
    ]);
    fixture
}

fn reserve(
    fixture: &Fixture,
    context: &OperationContext,
) -> (RuntimeInstallPlan, LanguageBuildOwner) {
    let plan = fixture.plan(false);
    let spec = LanguageInstallSpec::from_plan(&plan, false).unwrap();
    let LanguageReservation::Build(owner) = LanguageRepository::open_writer(context)
        .unwrap()
        .reserve_install(&spec)
        .unwrap()
    else {
        panic!("fresh fixture must reserve a build")
    };
    (plan, owner)
}

fn verified(
    fixture: &Fixture,
    plan: &RuntimeInstallPlan,
    context: &OperationContext,
) -> crate::artifact_snapshot::VerifiedArtifact {
    spool_verified_reader(
        fs::File::open(&fixture.archive).unwrap(),
        plan.source().filename(),
        plan.source().digest(),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap()
}

#[tokio::test]
async fn r06_owned_extraction_preserves_internal_links_and_complete_content() {
    let fixture = populated_fixture();
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut archive = verified(&fixture, &plan, &context);
    let receipt = install_into_owner(&plan, &mut archive, &mut owner, false)
        .await
        .unwrap();
    assert_eq!(
        fs::read_link(owner.final_path().join("bin/python3")).unwrap(),
        Path::new("python")
    );
    assert_eq!(
        fs::read(owner.final_path().join("lib/python3.15/marker")).unwrap(),
        b"owned fixture marker"
    );
    receipt.validate_owner(&owner).unwrap();
    fixture.assert_only_fixed_probe();
}

#[tokio::test]
async fn r06_wrong_verified_artifact_is_rejected_before_any_payload_write() {
    let fixture = populated_fixture();
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut archive = spool_verified_reader(
        b"different verified bytes".as_slice(),
        plan.source().filename(),
        Digest::from_sha256_bytes(b"different verified bytes"),
        1024,
        context.requested_cache_root(),
    )
    .unwrap();
    let error = install_into_owner(&plan, &mut archive, &mut owner, false)
        .await
        .err()
        .expect("different verified artifact must fail");
    assert!(format!("{error:#}").contains("artifact"), "{error:#}");
    assert_eq!(fs::read_dir(owner.final_path()).unwrap().count(), 0);
    assert!(!fixture.calls.exists());
}

#[tokio::test]
async fn r06_extraction_receipt_rejects_changed_nonentrypoint_bytes() {
    let fixture = populated_fixture();
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut archive = verified(&fixture, &plan, &context);
    let receipt = install_into_owner(&plan, &mut archive, &mut owner, false)
        .await
        .unwrap();
    owner
        .replace_file(
            Path::new("lib/python3.15/marker"),
            b"owned fixture marker",
            b"changed after extraction",
        )
        .unwrap();
    let error = verify_language(owner, receipt)
        .await
        .err()
        .expect("a receipt must bind the exact extracted content");
    assert!(format!("{error:#}").contains("inventory"), "{error:#}");
}

#[tokio::test]
async fn r06_cancelled_owned_extraction_creates_nothing() {
    let fixture = populated_fixture();
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut archive = verified(&fixture, &plan, &context);
    context.cancellation_token().cancel();
    let error = install_into_owner(&plan, &mut archive, &mut owner, false)
        .await
        .err()
        .expect("cancelled extraction must fail");
    assert!(format!("{error:#}").contains("cancel"), "{error:#}");
    assert_eq!(fs::read_dir(owner.final_path()).unwrap().count(), 0);
    assert!(!fixture.calls.exists());
}

#[tokio::test]
async fn r06_escaping_stdlib_link_chain_is_rejected_before_probe() {
    let fixture = Fixture::new();
    fixture.make_archive(vec![
        executable("bundle/bin/python", dynamic_script(&fixture)),
        Entry::Link("bundle/inside".into(), PathBuf::from(".")),
        Entry::Link("bundle/lib".into(), PathBuf::from("inside/../outside")),
    ]);
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut archive = verified(&fixture, &plan, &context);
    assert!(
        install_into_owner(&plan, &mut archive, &mut owner, false)
            .await
            .is_err()
    );
    assert!(
        !fixture.calls.exists(),
        "a runtime with an escaping stdlib chain was launched before inventory validation"
    );
}

#[tokio::test]
async fn r06_native_extraction_requires_an_empty_owned_payload() {
    use std::io::Write;
    let fixture = populated_fixture();
    let context = context(&fixture);
    let (plan, mut owner) = reserve(&fixture, &context);
    let mut extra = owner
        .create_file_exclusive(Path::new("not-from-the-archive"), 0o644)
        .unwrap();
    extra.write_all(b"unrelated pre-existing input").unwrap();
    extra.finish().unwrap();
    let mut artifact = verified(&fixture, &plan, &context);
    assert!(
        install_into_owner(&plan, &mut artifact, &mut owner, false)
            .await
            .is_err(),
        "prewritten unrelated bytes must not be sealed under this archive identity"
    );
    assert!(
        !fixture.calls.exists(),
        "nonempty owners must be rejected before probe"
    );
    assert!(
        !owner.final_path().join("bin").exists(),
        "nonempty owners must be rejected before extracting additional entries"
    );
    assert_eq!(
        fs::read(owner.final_path().join("not-from-the-archive")).unwrap(),
        b"unrelated pre-existing input"
    );
}
