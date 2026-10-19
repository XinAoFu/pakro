//! Closed capabilities are produced only through actual successful prerequisites.

use super::super::installed::{EnvironmentVerificationReceipt, verify_materialized_environment};
use super::wheels::Environment;
use super::*;
use crate::packages::python_wheel_install::WheelMaterializationReceipt;
use std::io::Write;
use std::os::unix::fs::symlink;

async fn verify(
    environment: &Environment,
    receipt: &WheelMaterializationReceipt,
) -> Result<EnvironmentVerificationReceipt> {
    verify_materialized_environment(
        &environment.layout,
        receipt,
        &environment.prefix,
        RuntimeStartupLimits::default(),
        Duration::from_secs(5),
    )
    .await
}
async fn installed(distinct: bool, traps: bool) -> (Environment, WheelMaterializationReceipt) {
    let mut environment = Environment::new(distinct).await;
    let receipt = environment.install("alpha-demo", true, traps);
    assert!(!receipt.files().is_empty());
    assert_eq!(receipt.sources().len(), 1);
    assert!(matches!(
        receipt.sources()[0].digest(),
        pakro_store::Digest::Sha512(_)
    ));
    (environment, receipt)
}

#[tokio::test]
async fn complete_install_preserves_outer_sources_header_data_csv_and_runtime_identity() {
    let (environment, receipt) = installed(false, false).await;
    let validated = verify(&environment, &receipt).await.unwrap();
    assert_eq!(
        validated.allocation_id(),
        environment.prefix.allocation_id()
    );
    assert_eq!(validated.final_prefix(), environment.prefix.final_path());
    assert_eq!(validated.sources(), receipt.sources());
    assert_eq!(validated.files(), receipt.files());
    assert_eq!(validated.directories(), receipt.directories());
    assert_ne!(validated.inventory_sha256(), &[0; 32]);
    assert_eq!(
        validated.observed_runtime().implementation,
        environment
            .layout
            .runtime
            .observed_site_disabled()
            .implementation
    );
    for suffix in ["demo.h", "data.txt", "pakro-demo", "资源/a,\"b\n.txt"] {
        assert!(
            validated
                .files()
                .iter()
                .any(|file| file.relative_path().to_string_lossy().ends_with(suffix)),
            "missing installed fact {suffix}"
        );
    }
    environment.prefix.check_binding().unwrap();
    environment.assert_unpublished();
}

#[tokio::test]
async fn installed_pth_and_customize_are_valid_payloads_but_never_executed_by_verification() {
    let (environment, receipt) = installed(false, true).await;
    let normal_runs = fs::read_to_string(environment.probes.parent().unwrap().join("runs"))
        .unwrap_or_default()
        .lines()
        .filter(|line| *line == "normal")
        .count();
    verify(&environment, &receipt).await.unwrap();
    let after = fs::read_to_string(environment.probes.parent().unwrap().join("runs"))
        .unwrap_or_default()
        .lines()
        .filter(|line| *line == "normal")
        .count();
    assert_eq!(
        after, normal_runs,
        "post verification launched a normal site-enabled child"
    );
    environment.assert_unpublished();
}

#[tokio::test]
async fn another_true_allocation_receipt_is_rejected_without_probe_or_prefix_writes() {
    let (environment, _) = installed(false, false).await;
    let (foreign, receipt) = installed(false, false).await;
    let before = environment.probe_log();
    let foreign_before = foreign.probe_log();
    error(verify(&environment, &receipt).await, "installed allocation");
    assert_eq!(environment.probe_log(), before);
    assert_eq!(foreign.probe_log(), foreign_before);
    environment.prefix.check_binding().unwrap();
    foreign.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn a_second_independent_graph_receipt_cannot_omit_existing_first_graph_outputs() {
    let mut environment = Environment::new(false).await;
    let first = environment.archive("first", vec![("first.py", b"FIRST = 1\n", 0o644)], false);
    let first = environment.prepare(vec![first]).unwrap();
    crate::packages::python_wheel_install::materialize_wheel_graph(first, &mut environment.prefix)
        .unwrap();
    let second = environment.archive("second", vec![("second.py", b"SECOND = 2\n", 0o644)], true);
    let second = environment.prepare(vec![second]).unwrap();
    let receipt = crate::packages::python_wheel_install::materialize_wheel_graph(
        second,
        &mut environment.prefix,
    )
    .unwrap();
    let before = environment.probe_log();
    error(verify(&environment, &receipt).await, "installed inventory");
    assert_eq!(environment.probe_log(), before);
    environment.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn cfg_or_executable_corruption_is_rejected_before_any_post_probe() {
    for relative in ["pyvenv.cfg", "bin/python"] {
        let (environment, receipt) = installed(false, false).await;
        let before = environment.probe_log();
        let path = environment.prefix.final_path().join(relative);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(path, bytes).unwrap();
        error(verify(&environment, &receipt).await, "installed inventory");
        assert_eq!(environment.probe_log(), before);
    }
}

#[tokio::test]
async fn first_and_last_package_file_corruption_is_not_hidden_by_successful_c4_receipts() {
    for last in [false, true] {
        let (environment, receipt) = installed(false, false).await;
        let file = if last {
            receipt.files().last().unwrap()
        } else {
            receipt.files().first().unwrap()
        };
        let path = environment.prefix.final_path().join(file.relative_path());
        let mut bytes = fs::read(&path).unwrap();
        if bytes.is_empty() {
            bytes.push(1);
        } else {
            bytes[0] ^= 1;
        }
        fs::write(path, bytes).unwrap();
        let before = environment.probe_log();
        error(verify(&environment, &receipt).await, "installed inventory");
        assert_eq!(environment.probe_log(), before);
    }
}

#[tokio::test]
async fn installed_record_self_row_does_not_waive_its_actual_file_hash() {
    let (environment, receipt) = installed(false, false).await;
    let record = receipt
        .files()
        .iter()
        .find(|file| file.relative_path().ends_with("RECORD"))
        .unwrap();
    let path = environment.prefix.final_path().join(record.relative_path());
    let bytes = fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/RECORD,,"));
    let mut changed = bytes;
    changed[0] ^= 1;
    fs::write(path, changed).unwrap();
    error(verify(&environment, &receipt).await, "installed inventory");
}

#[tokio::test]
async fn installed_modes_and_privilege_bits_are_actual_inventory_facts() {
    for mode in [0o600, 0o4644] {
        let (environment, receipt) = installed(false, false).await;
        let file = receipt
            .files()
            .iter()
            .find(|file| file.mode() == 0o644)
            .unwrap();
        fs::set_permissions(
            environment.prefix.final_path().join(file.relative_path()),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        error(verify(&environment, &receipt).await, "installed inventory");
    }
}

#[tokio::test]
async fn installed_external_hardlink_or_symlink_cannot_be_verified_or_written_through() {
    for link in [false, true] {
        let (environment, receipt) = installed(false, false).await;
        let file = &receipt.files()[0];
        let path = environment.prefix.final_path().join(file.relative_path());
        let original = fs::read(&path).unwrap();
        let outside = environment.scratch.join("outside");
        if link {
            fs::write(&outside, &original).unwrap();
            fs::remove_file(&path).unwrap();
            symlink(&outside, &path).unwrap();
        } else {
            fs::hard_link(&path, &outside).unwrap();
        }
        error(verify(&environment, &receipt).await, "installed inventory");
        assert_eq!(fs::read(outside).unwrap(), original);
    }
}

#[tokio::test]
async fn extra_tracked_and_untracked_outputs_are_not_hidden_by_receipt_coverage() {
    for tracked in [false, true] {
        let (mut environment, receipt) = installed(false, false).await;
        if tracked {
            let mut file = environment
                .prefix
                .create_file_exclusive(Path::new("extra"), 0o644)
                .unwrap();
            file.write_all(b"unplanned").unwrap();
            file.finish().unwrap();
        } else {
            fs::write(environment.prefix.final_path().join("extra"), b"unplanned").unwrap();
        }
        let before = environment.probe_log();
        error(verify(&environment, &receipt).await, "installed inventory");
        assert_eq!(environment.probe_log(), before);
        if tracked {
            environment.prefix.check_binding().unwrap();
        }
    }
}

#[tokio::test]
async fn missing_unused_normal_site_directory_is_not_reconstructed_from_only_package_files() {
    let (environment, receipt) = installed(true, false).await;
    let empty = environment.prefix.final_path().join("unused-platlib");
    assert!(fs::read_dir(&empty).unwrap().next().is_none());
    fs::remove_dir(empty).unwrap();
    error(verify(&environment, &receipt).await, "installed inventory");
}

#[tokio::test]
async fn tainted_prefix_and_zero_verification_budget_cannot_produce_local_success() {
    let (mut environment, receipt) = installed(false, false).await;
    error(
        verify_materialized_environment(
            &environment.layout,
            &receipt,
            &environment.prefix,
            RuntimeStartupLimits {
                max_paths: 0,
                ..Default::default()
            },
            Duration::from_secs(5),
        )
        .await,
        "installed limit",
    );
    environment.prefix.check_binding().unwrap();
    environment.prefix.mark_failed();
    error(verify(&environment, &receipt).await, "installed inventory");
}

#[tokio::test]
async fn all_shared_owners_and_both_outer_digest_algorithms_survive_verification() {
    let mut environment = Environment::new(false).await;
    let alpha = environment.archive(
        "first",
        vec![("namespace/shared.py", b"VALUE=42\n", 0o644)],
        false,
    );
    let beta = environment.archive(
        "second",
        vec![("namespace/shared.py", b"VALUE=42\n", 0o644)],
        true,
    );
    let graph = environment.prepare(vec![alpha, beta]).unwrap();
    let receipt = crate::packages::python_wheel_install::materialize_wheel_graph(
        graph,
        &mut environment.prefix,
    )
    .unwrap();
    assert!(
        receipt
            .files()
            .iter()
            .any(|file| file.relative_path().ends_with("namespace/shared.py")
                && file.owners().len() == 2)
    );
    let verified = verify(&environment, &receipt).await.unwrap();
    assert_eq!(verified.sources(), receipt.sources());
    assert_eq!(verified.files(), receipt.files());
    assert!(
        verified
            .sources()
            .iter()
            .any(|source| matches!(source.digest(), pakro_store::Digest::Sha256(_)))
    );
    assert!(
        verified
            .sources()
            .iter()
            .any(|source| matches!(source.digest(), pakro_store::Digest::Sha512(_)))
    );
    environment.assert_unpublished();
}

#[tokio::test]
async fn projected_stdlib_landmark_or_zip_is_rejected_before_starting_a_post_probe() {
    let mut failures = Vec::new();
    for relative in [
        "lib/python3.11/os.py",
        "lib/python311.zip",
        "bin/python._pth",
        "bin/pyvenv.cfg",
    ] {
        let mut environment = Environment::new(false).await;
        let data = format!("shadow-1.0.data/data/{relative}");
        let archive = environment.archive(
            "shadow",
            vec![(
                data.as_str(),
                b"known temporary startup-candidate fixture\n",
                0o644,
            )],
            false,
        );
        let graph = environment
            .prepare(vec![archive])
            .expect("true complete graph prerequisite for the D4 pre-exec boundary");
        let receipt = crate::packages::python_wheel_install::materialize_wheel_graph(
            graph,
            &mut environment.prefix,
        )
        .expect("true C4 copy prerequisite for the D4 pre-exec boundary");
        assert!(environment.prefix.final_path().join(relative).is_file());
        let before = environment.probe_log();
        match verify(&environment, &receipt).await {
            Err(failure) if format!("{failure:#}").contains("installed startup protection") => {}
            Err(failure) => failures.push(format!("{relative}: wrong error {failure:#}")),
            Ok(_) => failures.push(format!("{relative}: unsafe startup candidate was accepted")),
        }
        assert_eq!(environment.probe_log(), before);
        environment.prefix.check_binding().unwrap();
        environment.assert_unpublished();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
