use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use crate::libraries::language::repository::tests::{archive, install_spec, operation_context};
use crate::plugins::core::python::native::extract::install_test_read_hook;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

#[tokio::test]
async fn r06_existing_revalidation_runs_once_while_the_shared_lease_is_held() {
    use crate::libraries::language::repository::tests::prepare;
    use pakro_store::control::lease::{LeaseMode, StableLock};
    use pakro_store::control::{
        FileIdentity, open_existing_readonly_regular, open_trusted_store_with_deadline,
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let first = prepare(&spec, &context, &bytes).await.unwrap();
    let expected = first.root().to_path_buf();
    drop(first);
    let calls = AtomicUsize::new(0);
    let decisions = AtomicUsize::new(0);
    let reused = prepare_language_with_revalidation(
        &spec,
        &context,
        || {
            decisions.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        },
        || async {
            calls.fetch_add(1, Ordering::SeqCst);
            let parent = open_trusted_store_with_deadline(
                &context
                    .requested_data_root()
                    .join("languages/.leases/build"),
                nix::unistd::geteuid().as_raw(),
                false,
                context.policy().deadline,
            )
            .unwrap();
            let name = format!("{}.lock", spec.key().filename());
            let file =
                open_existing_readonly_regular(&parent, &name, nix::unistd::geteuid().as_raw())
                    .unwrap();
            let lock = StableLock::open_existing(
                &parent,
                &name,
                nix::unistd::geteuid().as_raw(),
                FileIdentity::of(&file).unwrap(),
            )
            .unwrap();
            assert!(
                lock.try_acquire(LeaseMode::Exclusive, context.policy().deadline)
                    .unwrap()
                    .is_none(),
                "source revalidation lost the existing payload's SH protection"
            );
            spool_verified_reader(
                bytes.as_slice(),
                spec.plan().source().filename(),
                spec.plan().source().digest(),
                1024 * 1024,
                context.requested_cache_root(),
            )
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(decisions.load(Ordering::SeqCst), 1);
    assert_eq!(reused.root(), expected);
}

#[tokio::test]
async fn r06_existing_revalidation_failure_cannot_be_bypassed_by_ready_state() {
    use crate::libraries::language::repository::tests::prepare;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let retained = prepare(&spec, &context, &bytes).await.unwrap();
    let result = prepare_language_with_revalidation(
        &spec,
        &context,
        || Ok(true),
        || async { Err(eyre::eyre!("source policy refused this invocation")) },
    )
    .await;
    let error = result
        .err()
        .expect("Ready cannot bypass a new source policy refusal");
    assert!(error.to_string().contains("source policy refused"));
    retained.check_binding().unwrap();
    context.checkpoint().unwrap();
}

#[tokio::test]
async fn r06_existing_revalidation_rejects_another_verified_artifact() {
    use crate::libraries::language::repository::tests::prepare;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let retained = prepare(&spec, &context, &bytes).await.unwrap();
    let result = prepare_language_with_revalidation(
        &spec,
        &context,
        || Ok(true),
        || async {
            spool_verified_reader(
                b"other verified content".as_slice(),
                spec.plan().source().filename(),
                pakro_store::Digest::from_sha256_bytes(b"other verified content"),
                1024,
                context.requested_cache_root(),
            )
        },
    )
    .await;
    assert!(
        result.is_err(),
        "revalidation must bind its verified bytes to the selected spec"
    );
    retained.check_binding().unwrap();
}

#[tokio::test]
async fn r06_existing_revalidation_decision_is_not_run_for_a_first_build() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let calls = AtomicUsize::new(0);
    prepare_language_with_revalidation(
        &spec,
        &context,
        || panic!("a first builder must not run the existing-only policy"),
        || async {
            calls.fetch_add(1, Ordering::SeqCst);
            spool_verified_reader(
                bytes.as_slice(),
                spec.plan().source().filename(),
                spec.plan().source().digest(),
                1024 * 1024,
                context.requested_cache_root(),
            )
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r06_extract_read_does_not_block_runtime_cancellation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let path = context
        .requested_data_root()
        .join("languages")
        .join(spec.key().filename())
        .join("payload");
    let ready = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::new(AtomicUsize::new(0));
    let (permit, receive) = mpsc::channel();
    let receive = Mutex::new(receive);
    let notification = ready.clone();
    let count = observed.clone();
    let _hook = install_test_read_hook(
        path,
        Arc::new(move |bytes| {
            if count.fetch_add(bytes, Ordering::SeqCst) == 0 {
                notification.notify_one();
                receive
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| {
                        std::io::Error::other(
                            "runtime could not schedule cancellation during extraction",
                        )
                    })?;
            }
            Ok(())
        }),
    );
    let preparing = prepare_language(&spec, &context, || async {
        spool_verified_reader(
            bytes.as_slice(),
            spec.plan().source().filename(),
            spec.plan().source().digest(),
            1024 * 1024,
            context.requested_cache_root(),
        )
    });
    let cancelling = async {
        ready.notified().await;
        context.cancellation_token().cancel();
        let _ = permit.send(());
    };
    let (result, ()) = tokio::join!(preparing, cancelling);
    assert!(
        observed.load(Ordering::SeqCst) > 0,
        "the actual compressed archive must have been read"
    );
    let error = result.err().expect("cancelled extraction must fail");
    assert!(
        format!("{error:#}").contains("cancelled"),
        "cancellation must be scheduled while extraction is in progress: {error:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r06_dropping_preparation_cancels_its_inflight_work() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(10));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let started = Arc::new(tokio::sync::Notify::new());
    let ready = started.clone();
    let operation = context.clone();
    let task = tokio::spawn(async move {
        prepare_language(&spec, &operation, || async move {
            ready.notify_one();
            std::future::pending::<Result<VerifiedArtifact>>().await
        })
        .await
    });
    started.notified().await;
    task.abort();
    let _ = task.await;
    assert!(
        context.cancellation_token().is_cancelled(),
        "dropping an active preparation must revoke workers sharing its context"
    );
}
