//! Real lock snapshots, cache entries and loopback HTTP; no private slot/source caps.

use super::*;
use crate::artifact_snapshot::cached_verified_artifact;
use crate::packages::python_resolution::lock_model::*;
use crate::packages::python_resolution::lock_session::{
    BoundPythonLock, FilePinLookup, ResolutionIntent,
};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[path = "tests/support.rs"]
mod support;
use support::*;
#[path = "tests/deadline.rs"]
mod deadline;

async fn invoke<'lock>(
    fixture: &Fixture,
    lock: &'lock BoundPythonLock,
    file: &RegistryFileRef,
    network: NetworkPolicy,
) -> LockedReplayResult<LockedRegistryArtifact<'lock>> {
    acquire_locked_registry_file(slot(lock, file), fixture.context(network, &ROUTE)).await
}

#[tokio::test]
async fn warm_expected_cache_needs_no_simple_locator_router_or_credentials() {
    for digest in [
        Digest::from_sha256_bytes(PAYLOAD),
        Digest::from_sha512_bytes(PAYLOAD),
    ] {
        for network in [NetworkPolicy::Allowed, NetworkPolicy::Offline] {
            let peer = Peer::new(|_| Reply::denied());
            let fixture = Fixture::new(&peer);
            let file = fixture.file(FILENAME);
            let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
            fixture.seed_cache(FILENAME, digest, PAYLOAD);
            // Actual shared-cache prelude, independent of the new replay.
            let oracle = cached_verified_artifact(FILENAME, digest, &fixture.cache, 1024)
                .unwrap()
                .unwrap();
            assert_eq!(oracle.digest(), digest);
            let before = fs::read(&fixture.lock_path).unwrap();
            let mut result = acquire_locked_registry_file(
                slot(&lock, &file),
                fixture.context(network, &NO_ROUTE),
            )
            .await
            .unwrap();
            assert_eq!(result.origin(), LockedReplayOrigin::Cache);
            assert_eq!(result.source_key(), None);
            assert_eq!(result.digest(), digest);
            assert_eq!(result.pin().digest(), digest);
            assert_eq!(result.filename(), FILENAME);
            assert_eq!(result.size(), PAYLOAD.len() as u64);
            assert_eq!(read(&mut result), PAYLOAD);
            result.recheck().unwrap();
            assert!(peer.requests().is_empty());
            assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
            assert!(!fixture.scratch.exists());
            fixture.assert_no_authority_writes();
        }
    }
}

#[tokio::test]
async fn offline_true_miss_is_missing_inputs_without_any_source_or_scratch_effects() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
    );
    assert_eq!(
        acquire_locked_registry_file(
            slot(&lock, &file),
            fixture.context(NetworkPolicy::Offline, &NO_ROUTE)
        )
        .await
        .unwrap_err(),
        LockedReplayError::MissingInputs
    );
    assert!(peer.requests().is_empty());
    assert!(!fixture.cache.exists());
    assert!(!fixture.scratch.exists());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn corrupt_expected_cache_never_falls_back_to_simple_or_repair() {
    for network in [NetworkPolicy::Allowed, NetworkPolicy::Offline] {
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let digest = Digest::from_sha256_bytes(PAYLOAD);
        let file = fixture.file(FILENAME);
        let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
        let path = fixture.seed_cache(FILENAME, digest, b"private-corrupt-cache-body");
        assert_eq!(
            acquire_locked_registry_file(slot(&lock, &file), fixture.context(network, &NO_ROUTE))
                .await
                .unwrap_err(),
            LockedReplayError::Cache
        );
        assert_eq!(fs::read(path).unwrap(), b"private-corrupt-cache-body");
        assert!(peer.requests().is_empty());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn actual_size_must_match_the_old_pin_for_both_cached_and_cold_bytes() {
    for warm in [true, false] {
        let files = Peer::new(|_| Reply::file(PAYLOAD));
        let index = index(page(vec![file_json(
            FILENAME,
            &format!("{}/file", files.url),
            serde_json::json!({}),
        )]));
        let fixture = Fixture::new(&index);
        let digest = Digest::from_sha256_bytes(PAYLOAD);
        let file = fixture.file(FILENAME);
        let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64 + 1);
        if warm {
            fixture.seed_cache(FILENAME, digest, PAYLOAD);
        }
        assert_eq!(
            invoke(&fixture, &lock, &file, NetworkPolicy::Allowed)
                .await
                .unwrap_err(),
            LockedReplayError::SizeMismatch
        );
        assert_eq!(index.requests().len(), usize::from(!warm));
        assert_eq!(files.requests().len(), usize::from(!warm));
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn cold_exact_file_keeps_the_old_pin_and_populates_the_next_offline_replay() {
    let files = Peer::new(|target| {
        assert_eq!(target, "/exact?signature=private-new-token");
        Reply::file(PAYLOAD)
    });
    let index = index(page(vec![
        file_json(
            OTHER,
            &format!("{}/wrong-build", files.url),
            serde_json::json!({}),
        ),
        file_json(
            FILENAME,
            &format!("{}/exact?signature=private-new-token", files.url),
            serde_json::json!({}),
        ),
    ]));
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha512_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    let before = fs::read(&fixture.lock_path).unwrap();
    let mut cold = invoke(&fixture, &lock, &file, NetworkPolicy::Allowed)
        .await
        .unwrap();
    assert_eq!(cold.origin(), LockedReplayOrigin::Registry);
    assert!(cold.source_key().is_some());
    assert_eq!(cold.pin().file().version(), "1.0.0"); // Simple filename spells 1.0.
    assert_eq!(cold.digest(), digest);
    assert_eq!(read(&mut cold), PAYLOAD);
    assert!(!format!("{cold:?}").contains("private-new-token"));
    assert_eq!(index.requests(), vec!["/simple/demo-pkg/"]);
    assert_eq!(files.requests(), vec!["/exact?signature=private-new-token"]);
    let mut warm = acquire_locked_registry_file(
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Offline, &NO_ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(warm.origin(), LockedReplayOrigin::Cache);
    assert_eq!(warm.source_key(), None);
    assert_eq!(warm.pin().id(), cold.pin().id());
    assert_eq!(read(&mut warm), PAYLOAD);
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn nohash_weak_and_unknown_hash_pages_still_verify_the_old_expected_bytes() {
    for hashes in [
        serde_json::json!({}),
        serde_json::json!({"md5":"1".repeat(32)}),
        serde_json::json!({"future-hash":"012345"}),
    ] {
        for good in [true, false] {
            let files = Peer::new(move |_| {
                Reply::file(if good {
                    PAYLOAD
                } else {
                    b"tampered locked bytes"
                })
            });
            let index = index(page(vec![file_json(
                FILENAME,
                &format!("{}/signed?token=private", files.url),
                hashes.clone(),
            )]));
            let fixture = Fixture::new(&index);
            let file = fixture.file(FILENAME);
            let digest = Digest::from_sha512_bytes(PAYLOAD);
            let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
            let result = invoke(&fixture, &lock, &file, NetworkPolicy::Allowed).await;
            if good {
                let mut result = result.unwrap();
                assert_eq!(result.digest(), digest);
                assert_eq!(read(&mut result), PAYLOAD);
                assert!(result.source_key().is_some());
            } else {
                assert_eq!(result.unwrap_err(), LockedReplayError::ByteIdentity);
                assert!(!fixture.cache.exists());
            }
            assert_eq!(files.requests().len(), 1);
            fixture.assert_no_authority_writes();
        }
    }
}

#[tokio::test]
async fn same_algorithm_page_conflicts_fail_before_file_transport() {
    for sha512 in [false, true] {
        let files = Peer::new(|_| Reply::file(PAYLOAD));
        let (digest, algorithm, conflicting) = if sha512 {
            (
                Digest::from_sha512_bytes(PAYLOAD),
                "sha512",
                Digest::from_sha512_bytes(b"different").hex(),
            )
        } else {
            (
                Digest::from_sha256_bytes(PAYLOAD),
                "sha256",
                Digest::from_sha256_bytes(b"different").hex(),
            )
        };
        let index = index(page(vec![file_json(
            FILENAME,
            &format!("{}/must-not-fetch", files.url),
            serde_json::json!({(algorithm):conflicting}),
        )]));
        let fixture = Fixture::new(&index);
        let file = fixture.file(FILENAME);
        let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
        assert_eq!(
            invoke(&fixture, &lock, &file, NetworkPolicy::Allowed)
                .await
                .unwrap_err(),
            LockedReplayError::HashConflict
        );
        assert_eq!(index.requests().len(), 1);
        assert!(files.requests().is_empty());
        assert!(!fixture.cache.exists());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn different_hash_algorithms_are_compatible_but_every_actual_strong_claim_is_checked() {
    for case in ["valid-cross", "bad-page", "bad-old", "bad-nonpreferred"] {
        let old = Digest::from_sha512_bytes(PAYLOAD);
        let (hashes, served, old) = match case {
            "valid-cross" => (
                serde_json::json!({"sha256":Digest::from_sha256_bytes(PAYLOAD).hex()}),
                PAYLOAD,
                old,
            ),
            "bad-page" => (
                serde_json::json!({"sha256":Digest::from_sha256_bytes(b"not these bytes").hex()}),
                PAYLOAD,
                old,
            ),
            "bad-old" => (
                serde_json::json!({"sha256":Digest::from_sha256_bytes(b"tampered").hex()}),
                &b"tampered"[..],
                old,
            ),
            "bad-nonpreferred" => (
                serde_json::json!({"sha256":Digest::from_sha256_bytes(PAYLOAD).hex(),"sha512":Digest::from_sha512_bytes(b"different").hex()}),
                PAYLOAD,
                Digest::from_sha256_bytes(PAYLOAD),
            ),
            _ => unreachable!(),
        };
        let files = Peer::new(move |_| Reply::file(served));
        let index = index(page(vec![file_json(
            FILENAME,
            &format!("{}/file", files.url),
            hashes,
        )]));
        let fixture = Fixture::new(&index);
        let file = fixture.file(FILENAME);
        let lock = fixture.lock(&file, old, PAYLOAD.len() as u64);
        let result = invoke(&fixture, &lock, &file, NetworkPolicy::Allowed).await;
        if case == "valid-cross" {
            let mut result = result.unwrap();
            assert_eq!(result.digest(), old);
            assert_eq!(read(&mut result), PAYLOAD);
        } else if case == "bad-page" {
            assert_eq!(
                result.unwrap_err(),
                LockedReplayError::Source(SourceResolutionError::ByteIdentity)
            );
        } else {
            assert_eq!(
                result.unwrap_err(),
                LockedReplayError::ByteIdentity,
                "{case}"
            );
        }
        assert_eq!(files.requests().len(), 1, "{case}");
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn registry_context_and_exact_filename_cannot_be_substituted() {
    let files = Peer::new(|_| Reply::file(PAYLOAD));
    let index = index(page(vec![file_json(
        OTHER,
        &format!("{}/wrong", files.url),
        serde_json::json!({}),
    )]));
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    fixture.seed_cache(FILENAME, digest, PAYLOAD);
    for registry in [
        PythonRegistry::new(
            "another",
            fixture.registry.canonical_base().as_url().as_str(),
            None,
        )
        .unwrap(),
        PythonRegistry::new("corp", &format!("{}/another-base/", index.url), None).unwrap(),
    ] {
        let mut context = fixture.context(NetworkPolicy::Allowed, &NO_ROUTE);
        context.project_session.registry = &registry;
        assert_eq!(
            acquire_locked_registry_file(slot(&lock, &file), context)
                .await
                .unwrap_err(),
            LockedReplayError::RegistryMismatch
        );
        assert!(index.requests().is_empty());
    }
    fs::remove_dir_all(&fixture.cache).unwrap();
    assert_eq!(
        invoke(&fixture, &lock, &file, NetworkPolicy::Allowed)
            .await
            .unwrap_err(),
        LockedReplayError::FileMissing
    );
    assert_eq!(index.requests().len(), 1);
    assert!(files.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn an_external_lock_edit_revokes_replay_before_cache_and_during_actual_transfer() {
    for late in [false, true] {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let hit = Arc::clone(&entered);
        let released = Arc::clone(&release);
        let files = Peer::new(move |_| {
            hit.store(true, Ordering::Release);
            let limit = Instant::now() + Duration::from_secs(3);
            while !released.load(Ordering::Acquire) && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(2));
            }
            Reply::file(PAYLOAD)
        });
        let index = index(page(vec![file_json(
            FILENAME,
            &format!("{}/file", files.url),
            serde_json::json!({}),
        )]));
        let fixture = Fixture::new(&index);
        let file = fixture.file(FILENAME);
        let lock = fixture.lock(
            &file,
            Digest::from_sha256_bytes(PAYLOAD),
            PAYLOAD.len() as u64,
        );
        let existing = slot(&lock, &file);
        let external = b"lockfile_version=4\n# external edit\n";
        if !late {
            fs::write(&fixture.lock_path, external).unwrap();
        }
        let mut future = Box::pin(acquire_locked_registry_file(
            existing,
            fixture.context(NetworkPolicy::Allowed, &ROUTE),
        ));
        let result = if late {
            tokio::select! {
                result = &mut future => { release.store(true, Ordering::Release); result },
                reached = tokio::time::timeout(Duration::from_secs(2), async {
                    while !entered.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(2)).await; }
                }) => {
                    release.store(true, Ordering::Release);
                    reached.unwrap();
                    fs::write(&fixture.lock_path, external).unwrap();
                    future.await
                }
            }
        } else {
            future.await
        };
        release.store(true, Ordering::Release);
        assert_eq!(result.unwrap_err(), LockedReplayError::Revoked);
        assert_eq!(fs::read(&fixture.lock_path).unwrap(), external);
        assert_eq!(index.requests().len(), usize::from(late));
        assert_eq!(files.requests().len(), usize::from(late));
        assert!(!fixture.cache.exists());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn resource_limits_and_a_single_total_deadline_cannot_be_bypassed_by_cache_or_two_requests() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    fixture.seed_cache(FILENAME, digest, PAYLOAD);
    for axis in 0..10 {
        let mut context = fixture.context(NetworkPolicy::Allowed, &NO_ROUTE);
        let expected = match axis {
            0 => {
                context.source_limits.transfer.max_wire_bytes = 0;
                LockedReplayError::InvalidLimits
            }
            1 => {
                context.source_limits.transfer.max_headers = 0;
                LockedReplayError::InvalidLimits
            }
            2 => {
                context.source_limits.max_spool_bytes = 0;
                LockedReplayError::InvalidLimits
            }
            3 => {
                context.source_limits.max_identity_bytes = 0;
                LockedReplayError::InvalidLimits
            }
            4 => {
                context.source_limits.transfer.max_wire_bytes = u64::MAX;
                LockedReplayError::InvalidLimits
            }
            5 => {
                context.source_limits.max_spool_bytes = 1;
                LockedReplayError::InvalidLimits
            }
            6 => {
                context.lock_limits.max_field_bytes = 1;
                LockedReplayError::InputLimits
            }
            7 => {
                context.lock_limits.max_artifact_bytes = PAYLOAD.len() as u64 - 1;
                LockedReplayError::InputLimits
            }
            8 => {
                context.source_limits.max_identity_bytes = 1;
                LockedReplayError::InputLimits
            }
            9 => {
                context.deadline = Instant::now() - Duration::from_millis(1);
                LockedReplayError::Deadline
            }
            _ => unreachable!(),
        };
        assert_eq!(
            acquire_locked_registry_file(slot(&lock, &file), context)
                .await
                .unwrap_err(),
            expected,
            "axis {axis}"
        );
        assert!(peer.requests().is_empty());
    }
    let files = Peer::new(|_| {
        std::thread::sleep(Duration::from_millis(650));
        Reply::file(PAYLOAD)
    });
    let page_body = page(vec![file_json(
        FILENAME,
        &format!("{}/delayed", files.url),
        serde_json::json!({}),
    )]);
    let index = Peer::new(move |_| {
        std::thread::sleep(Duration::from_millis(650));
        Reply::page(page_body.clone())
    });
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    let mut context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    context.deadline = Instant::now() + Duration::from_millis(1000);
    let started = Instant::now();
    assert_eq!(
        acquire_locked_registry_file(slot(&lock, &file), context)
            .await
            .unwrap_err(),
        LockedReplayError::Deadline
    );
    assert!(started.elapsed() < Duration::from_millis(1600));
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    assert!(!fixture.cache.exists());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn a_legal_locked_sdist_replays_the_same_archive_without_inventing_wheel_metadata() {
    const SDIST: &str = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz").to_vec();
    let served = bytes.clone();
    let files = Peer::new(move |_| Reply::file(&served));
    let index = index(page(vec![file_json(
        SDIST,
        &format!("{}/source", files.url),
        serde_json::json!({}),
    )]));
    let fixture = Fixture::new(&index);
    let file = fixture.file(SDIST);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    let mut result = invoke(&fixture, &lock, &file, NetworkPolicy::Allowed)
        .await
        .unwrap();
    assert_eq!(result.pin().file().kind(), RegistryArtifactKind::Sdist);
    assert_eq!(result.filename(), SDIST);
    assert_eq!(result.digest(), digest);
    assert_eq!(read(&mut result), bytes);
    assert_eq!(result.origin(), LockedReplayOrigin::Registry);
    assert_eq!(files.requests(), vec!["/source"]);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn cancelling_after_actual_file_transfer_begins_publishes_no_partial_artifact() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let hit = Arc::clone(&entered);
    let released = Arc::clone(&release);
    let files = Peer::new(move |_| {
        hit.store(true, Ordering::Release);
        let limit = Instant::now() + Duration::from_secs(3);
        while !released.load(Ordering::Acquire) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        Reply::file(PAYLOAD)
    });
    let index = index(page(vec![file_json(
        FILENAME,
        &format!("{}/file", files.url),
        serde_json::json!({}),
    )]));
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
    );
    let before = fs::read(&fixture.lock_path).unwrap();
    let mut future = Box::pin(invoke(&fixture, &lock, &file, NetworkPolicy::Allowed));
    let (completed, reached) = tokio::select! {
        _result = &mut future => (true, entered.load(Ordering::Acquire)),
        reached = tokio::time::timeout(Duration::from_secs(2), async {
            while !entered.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(2)).await; }
        }) => (false, reached.is_ok()),
    };
    drop(future);
    release.store(true, Ordering::Release);
    assert!(
        !completed,
        "replay completed before cancellation reached actual T"
    );
    assert!(reached, "cancellation fixture did not reach actual T");
    assert_eq!(files.requests().len(), 1);
    assert!(!fixture.cache.exists());
    assert!(!fixture.scratch.exists() || fs::read_dir(&fixture.scratch).unwrap().next().is_none());
    assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn actual_warm_snapshot_cannot_escape_the_original_deadline_during_final_synchronous_work() {
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(PAYLOAD);
    let lock = fixture.lock(&file, digest, PAYLOAD.len() as u64);
    fixture.seed_cache(FILENAME, digest, PAYLOAD);
    let mut control = acquire_locked_registry_file(
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Offline, &NO_ROUTE),
    )
    .await
    .expect("the actual warm-cache and live-slot prerequisites must succeed");
    assert_eq!(control.origin(), LockedReplayOrigin::Cache);
    assert_eq!(read(&mut control), PAYLOAD);
    let mut context = fixture.context(NetworkPolicy::Offline, &NO_ROUTE);
    context.deadline = Instant::now() + Duration::from_millis(100);
    let original_deadline = context.deadline;
    let reached = std::cell::Cell::new(false);
    let result = acquire_with_finish(slot(&lock, &file), context, || {
        reached.set(true);
        // Finite real final work, after the production replay has obtained its
        // genuine source capability. No fake future or success cap is injected.
        std::thread::sleep(
            original_deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
        );
    })
    .await;
    assert!(
        reached.get(),
        "fixture must reach the actual final-work seam"
    );
    assert!(Instant::now() >= original_deadline);
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
    assert_eq!(result.unwrap_err(), LockedReplayError::Deadline);
}

#[tokio::test]
async fn mixed_locked_url_cold_fetch_never_reads_simple() {
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{PackageName, PackageRecord, RepoDataRecord, Version};
    use std::str::FromStr;

    let peer = Peer::new(|path| {
        if path.ends_with(FILENAME) {
            Reply::file(PAYLOAD)
        } else {
            Reply::denied()
        }
    });
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let limits = LockModelLimits::default();
    let pin = ContentPin::new(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
        PinEvidence::Advertised,
        limits,
    )
    .unwrap();
    let mut package = PackageRecord::new(
        PackageName::from_str("python").unwrap(),
        Version::from_str("3.13.13").unwrap(),
        "cp313_0".into(),
    );
    package.subdir = "osx-arm64".into();
    package.sha256 = Some([6u8; 32].into());
    package.size = Some(100);
    let python = RepoDataRecord {
        package_record: package,
        identifier: DistArchiveIdentifier::new(
            ArchiveIdentifier {
                name: "python".into(),
                version: "3.13.13".into(),
                build_string: "cp313_0".into(),
            },
            DistArchiveType::Conda(CondaArchiveType::Conda),
        ),
        url: "https://conda.anaconda.org/conda-forge/osx-arm64/python-3.13.13-cp313_0.conda"
            .parse()
            .unwrap(),
        channel: Some("https://conda.anaconda.org/conda-forge/".into()),
    };
    let language =
        crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
            &python,
        )
        .unwrap()
        .language_key(false)
        .unwrap();
    let conda = CondaLockGraph {
        config_scope: String::new(),
        group: "default".into(),
        target: "osx-arm64".into(),
        target_fingerprint: language.target(),
        channels: vec!["conda-forge".into()],
        roots: vec!["python ==3.13.13".into()],
        virtual_packages: vec![],
        records: vec![python],
    };
    let graph = MixedLockGraph {
        config_scope: String::new(),
        group: "default".into(),
        target_fingerprint: language.target(),
        language: language.fingerprint(),
        marker_semantics: "current2026".into(),
        root_declarations: vec!["demo-pkg==1.0.0".into()],
        roots: vec![MixedLockRoot {
            declaration_index: 0,
            node: "demo-pkg".into(),
        }],
        nodes: vec![MixedLockNode {
            name: "demo-pkg".into(),
            version: "1.0.0".into(),
            source: MixedLockSource::Registry {
                pin: hex::encode(pin.id().as_bytes()),
                url: Some(format!("{}/files/{FILENAME}", peer.url)),
            },
        }],
        edges: vec![],
    };
    let model = PythonLockModel::from_parts(&[], &[], limits)
        .unwrap()
        .with_conda(conda, limits)
        .unwrap()
        .with_mixed(graph, &[pin], &[], limits)
        .unwrap();
    fs::write(
        &fixture.lock_path,
        format!(
            "lockfile_version = 4\n{}",
            model.encode_projection(limits).unwrap()
        ),
    )
    .unwrap();
    let lock = BoundPythonLock::acquire_conda(
        &fixture.lock_path,
        limits,
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap();
    let mut artifact = acquire_locked_url(
        &lock,
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Allowed, &ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(artifact.origin(), LockedReplayOrigin::Registry);
    assert_eq!(read(&mut artifact), PAYLOAD);
    assert_eq!(peer.requests(), vec![format!("/files/{FILENAME}")]);
}

#[tokio::test]
async fn schema7_pypi_lock_cold_fetch_uses_locked_file_url_without_simple() {
    let peer = Peer::new(|path| {
        if path.ends_with(FILENAME) {
            Reply::file(PAYLOAD)
        } else {
            Reply::denied()
        }
    });
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(
        &file,
        Digest::from_sha256_bytes(PAYLOAD),
        PAYLOAD.len() as u64,
    );
    let pin = lock.model().unwrap().pins()[0].id();
    let model = lock.model().unwrap().clone();
    drop(lock);
    let locators = [(
        hex::encode(pin.as_bytes()),
        format!("{}/files/{FILENAME}", peer.url),
    )]
    .into_iter()
    .collect();
    let model = model.with_locators(locators, Default::default()).unwrap();
    fs::write(
        &fixture.lock_path,
        format!(
            "lockfile_version = 4\n{}",
            model.encode_projection(Default::default()).unwrap()
        ),
    )
    .unwrap();
    let lock = BoundPythonLock::acquire(
        &fixture.lock_path,
        Default::default(),
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap();
    let mut artifact = acquire_locked_url(
        &lock,
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Allowed, &ROUTE),
    )
    .await
    .unwrap();
    assert_eq!(read(&mut artifact), PAYLOAD);
    assert_eq!(peer.requests(), vec![format!("/files/{FILENAME}")]);
}
