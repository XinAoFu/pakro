use super::wheels::Environment;
use super::*;
use crate::packages::python_environment::runtime_startup::installed::{
    EnvironmentVerificationReceipt, verify_materialized_environment,
};
use crate::packages::python_environment::seal::seal_verified_environment;
use crate::packages::python_wheel_install::{WheelMaterializationReceipt, materialize_wheel_graph};

async fn complete() -> (
    Environment,
    WheelMaterializationReceipt,
    EnvironmentVerificationReceipt,
) {
    let mut environment = Environment::new(false).await;
    let wheels = environment.install("alpha-demo", true, false);
    let verified = verify_materialized_environment(
        &environment.layout,
        &wheels,
        &environment.prefix,
        RuntimeStartupLimits::default(),
        Duration::from_secs(5),
    )
    .await
    .expect("true D4 prerequisite before local seal assertions");
    (environment, wheels, verified)
}

#[tokio::test]
async fn seal_consumes_the_real_prefix_and_preserves_complete_local_facts() {
    let (environment, wheels, verified) = complete().await;
    let path = environment.prefix.final_path().to_path_buf();
    let allocation = environment.prefix.allocation_id().to_owned();
    let inventory = *verified.inventory_sha256();
    let sources = verified.sources().to_vec();
    let seal = seal_verified_environment(
        environment.layout,
        wheels,
        environment.prefix,
        verified,
        PrefixInventoryLimits::default(),
    )
    .unwrap();
    assert_eq!(seal.final_prefix(), path);
    assert_eq!(seal.allocation_id(), allocation);
    assert_eq!(seal.verification().sources(), sources);
    assert_eq!(seal.verification().inventory_sha256(), &inventory);
    assert_eq!(seal.identity_facts().unwrap().allocation, allocation);
    seal.recheck(PrefixInventoryLimits::default()).unwrap();
    assert_eq!(fs::read(&environment.active).unwrap(), b"old active");
    assert_eq!(fs::read(&environment.cas).unwrap(), b"old CAS");
}

#[tokio::test]
async fn a_real_verification_from_another_allocation_cannot_close_this_prefix() {
    let (environment, wheels, _verified) = complete().await;
    let (other, _other_wheels, other_verified) = complete().await;
    error(
        seal_verified_environment(
            environment.layout,
            wheels,
            environment.prefix,
            other_verified,
            PrefixInventoryLimits::default(),
        ),
        "seal allocation",
    );
    other.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn a_real_wheel_receipt_from_another_allocation_cannot_close_this_prefix() {
    let (environment, _wheels, verified) = complete().await;
    let (other, other_wheels, _other_verified) = complete().await;
    error(
        seal_verified_environment(
            environment.layout,
            other_wheels,
            environment.prefix,
            verified,
            PrefixInventoryLimits::default(),
        ),
        "seal allocation",
    );
    other.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn a_prior_d4_receipt_does_not_cover_later_real_c4_outputs() {
    let (mut environment, wheels, verified) = complete().await;
    let archive = environment.archive("second", vec![("second.py", b"VALUE=2\n", 0o644)], false);
    let graph = environment.prepare(vec![archive]).unwrap();
    let additional = materialize_wheel_graph(graph, &mut environment.prefix)
        .expect("true second C4 prerequisite");
    assert!(!additional.files().is_empty());
    error(
        seal_verified_environment(
            environment.layout,
            wheels,
            environment.prefix,
            verified,
            PrefixInventoryLimits::default(),
        ),
        "seal inventory",
    );
}

#[tokio::test]
async fn actual_file_bytes_modes_and_unknown_outputs_are_rechecked_when_closing() {
    let mut failures = Vec::new();
    for case in ["bytes", "mode", "unknown"] {
        let (environment, wheels, verified) = complete().await;
        let path = environment
            .prefix
            .final_path()
            .join(wheels.files()[0].relative_path());
        match case {
            "bytes" => fs::write(&path, b"changed after D4\n").unwrap(),
            "mode" => fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap(),
            "unknown" => {
                fs::write(environment.prefix.final_path().join("unknown"), b"external").unwrap()
            }
            _ => unreachable!(),
        }
        match seal_verified_environment(
            environment.layout,
            wheels,
            environment.prefix,
            verified,
            PrefixInventoryLimits::default(),
        ) {
            Err(error) if format!("{error:#}").contains("seal inventory") => {}
            Err(error) => failures.push(format!("{case}: wrong diagnostic {error:#}")),
            Ok(_) => failures.push(format!("{case}: changed inventory was accepted")),
        }
        assert_eq!(fs::read(&environment.active).unwrap(), b"old active");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn sealed_recheck_rejects_external_changes_without_repairing_them() {
    let (environment, wheels, verified) = complete().await;
    let path = environment
        .prefix
        .final_path()
        .join(wheels.files()[0].relative_path());
    let seal = seal_verified_environment(
        environment.layout,
        wheels,
        environment.prefix,
        verified,
        PrefixInventoryLimits::default(),
    )
    .unwrap();
    seal.recheck(PrefixInventoryLimits::default()).unwrap();
    fs::write(&path, b"external after seal\n").unwrap();
    error(
        seal.recheck(PrefixInventoryLimits::default()),
        "seal inventory",
    );
    assert_eq!(fs::read(&path).unwrap(), b"external after seal\n");
}

#[tokio::test]
async fn a_recheck_budget_error_does_not_reopen_or_damage_a_local_seal() {
    let (environment, wheels, verified) = complete().await;
    let seal = seal_verified_environment(
        environment.layout,
        wheels,
        environment.prefix,
        verified,
        PrefixInventoryLimits::default(),
    )
    .unwrap();
    let invalid = PrefixInventoryLimits {
        max_files: 0,
        ..Default::default()
    };
    assert!(seal.recheck(invalid).is_err());
    seal.recheck(PrefixInventoryLimits::default()).unwrap();
}

#[tokio::test]
async fn closing_cannot_ignore_a_zero_inventory_budget() {
    let (environment, wheels, verified) = complete().await;
    error(
        seal_verified_environment(
            environment.layout,
            wheels,
            environment.prefix,
            verified,
            PrefixInventoryLimits {
                max_files: 0,
                ..Default::default()
            },
        ),
        "seal inventory",
    );
}

#[tokio::test]
async fn dropping_an_unpublished_local_seal_releases_its_owned_tree() {
    let (environment, wheels, verified) = complete().await;
    let path = environment.prefix.final_path().to_path_buf();
    let seal = seal_verified_environment(
        environment.layout,
        wheels,
        environment.prefix,
        verified,
        PrefixInventoryLimits::default(),
    )
    .unwrap();
    assert!(path.is_dir());
    drop(seal);
    assert!(!path.exists());
    assert_eq!(fs::read(&environment.active).unwrap(), b"old active");
}

#[tokio::test]
async fn caller_deadline_is_not_reset_by_installed_verification() {
    let (environment, wheels, _verified) = complete().await;
    let probes = fs::read(&environment.probes).unwrap_or_default();
    let result = super::super::installed::verify_materialized_environment_at_deadline(
        &environment.layout,
        &wheels,
        &environment.prefix,
        RuntimeStartupLimits::default(),
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .await;
    error(result, "deadline");
    assert_eq!(fs::read(&environment.probes).unwrap_or_default(), probes);
    environment.prefix.check_binding().unwrap();
}
