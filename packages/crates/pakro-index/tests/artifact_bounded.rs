use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pakro_index::artifact::{
    ArtifactError, ArtifactTransport, NetworkPolicy, SourceArtifact, download_verified_bounded,
};
use pakro_store::Digest;

fn transport() -> ArtifactTransport {
    ArtifactTransport::new(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5)),
    )
    .unwrap()
}

fn source(base: &str, payload: &[u8]) -> SourceArtifact {
    SourceArtifact::new(
        "payload.whl",
        &format!("{base}/object?token=synthetic-limit-secret"),
        Digest::from_sha256_bytes(payload),
    )
    .unwrap()
}

#[tokio::test]
async fn zero_limit_precedes_http_and_temporary_directory_creation() {
    let mut server = mockito::Server::new_async().await;
    let no_request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("absent");
    let error = download_verified_bounded(
        &transport(),
        &source(&server.url(), b""),
        NetworkPolicy::Allowed,
        &missing,
        0,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ArtifactError::InvalidLimit), "{error:?}");
    assert!(!missing.exists());
    no_request.assert_async().await;
}

#[tokio::test]
async fn exact_limit_matches_both_digest_algorithms_and_returns_complete_bytes() {
    let payload = b"bounded archive contents";
    for digest in [
        Digest::from_sha256_bytes(payload),
        Digest::from_sha512_bytes(payload),
    ] {
        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/object")
            .with_body(payload)
            .expect(1)
            .create_async()
            .await;
        let root = tempfile::tempdir().unwrap();
        let source =
            SourceArtifact::new("payload.whl", &format!("{}/object", server.url()), digest)
                .unwrap();
        let artifact = download_verified_bounded(
            &transport(),
            &source,
            NetworkPolicy::Allowed,
            root.path(),
            payload.len() as u64,
        )
        .await
        .unwrap();
        assert_eq!(artifact.size(), payload.len() as u64);
        assert_eq!(artifact.digest(), digest);
        assert_eq!(std::fs::read(artifact.path()).unwrap(), payload);
        drop(artifact);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        request.assert_async().await;
    }
}

#[tokio::test]
async fn declared_oversized_response_never_returns_or_retains_an_artifact() {
    let mut server = mockito::Server::new_async().await;
    let payload = b"too many artifact bytes";
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let error = download_verified_bounded(
        &transport(),
        &source(&server.url(), payload),
        NetworkPolicy::Allowed,
        root.path(),
        3,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ArtifactError::TooLarge { limit: 3 }),
        "{error:?}"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert!(!format!("{error:#?} {error}").contains("synthetic-limit-secret"));
    request.assert_async().await;
}

struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
    }
}

#[tokio::test]
async fn chunked_overflow_is_rejected_before_eof_or_another_body_poll() {
    let mut server = mockito::Server::new_async().await;
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Gate(Arc::clone(&state));
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_chunked_body(move |writer| {
            writer.write_all(b"123456")?;
            writer.flush()?;
            let (released, _) = state
                .1
                .wait_timeout_while(
                    state.0.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .map_err(|_| std::io::Error::other("fixture gate poisoned"))?;
            if !*released {
                return Err(std::io::Error::other("fixture gate timeout"));
            }
            writer.write_all(b"suffix")
        })
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(&server.url(), b"123456suffix");
    let scratch = root.path().to_path_buf();
    let mut task = tokio::spawn(async move {
        download_verified_bounded(&transport(), &source, NetworkPolicy::Allowed, &scratch, 5).await
    });
    let early = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
    gate.release();
    let result = match early {
        Ok(result) => result.unwrap(),
        Err(_) => {
            let _ = task.await;
            panic!("overflow must be rejected without waiting for response EOF");
        }
    };
    assert!(matches!(result, Err(ArtifactError::TooLarge { limit: 5 })));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[tokio::test]
async fn chunked_exact_limit_still_waits_for_eof_and_checks_the_digest() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_chunked_body(|writer| writer.write_all(b"12345"))
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let artifact = download_verified_bounded(
        &transport(),
        &source(&server.url(), b"12345"),
        NetworkPolicy::Allowed,
        root.path(),
        5,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(artifact.path()).unwrap(), b"12345");
    request.assert_async().await;
}

#[tokio::test]
async fn an_exact_limit_prefix_cannot_succeed_before_later_overflow() {
    let mut server = mockito::Server::new_async().await;
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Gate(Arc::clone(&state));
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_chunked_body(move |writer| {
            writer.write_all(b"12345")?;
            writer.flush()?;
            let (released, _) = state
                .1
                .wait_timeout_while(
                    state.0.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .map_err(|_| std::io::Error::other("fixture gate poisoned"))?;
            if !*released {
                return Err(std::io::Error::other("fixture gate timeout"));
            }
            writer.write_all(b"X")
        })
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(&server.url(), b"12345");
    let scratch = root.path().to_path_buf();
    let mut task = tokio::spawn(async move {
        download_verified_bounded(&transport(), &source, NetworkPolicy::Allowed, &scratch, 5).await
    });
    let premature = tokio::time::timeout(Duration::from_millis(150), &mut task).await;
    gate.release();
    assert!(
        premature.is_err(),
        "matching the limit prefix does not prove EOF"
    );
    let result = task.await.unwrap();
    assert!(matches!(result, Err(ArtifactError::TooLarge { limit: 5 })));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

fn eof_server(payload: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .unwrap();
        stream.write_all(&payload).unwrap();
    });
    (format!("http://{address}"), worker)
}

#[tokio::test]
async fn response_without_length_or_chunking_is_bounded_by_actual_eof_bytes() {
    for limit in [3, 5] {
        let (base, worker) = eof_server(b"12345".to_vec());
        let root = tempfile::tempdir().unwrap();
        let result = download_verified_bounded(
            &transport(),
            &source(&base, b"12345"),
            NetworkPolicy::Allowed,
            root.path(),
            limit,
        )
        .await;
        worker.join().unwrap();
        if limit == 3 {
            assert!(matches!(result, Err(ArtifactError::TooLarge { limit: 3 })));
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        } else {
            assert_eq!(std::fs::read(result.unwrap().path()).unwrap(), b"12345");
        }
    }
}

#[tokio::test]
async fn wrong_digest_below_the_limit_remains_a_digest_error_and_cleans_up() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_body(b"wrong")
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let error = download_verified_bounded(
        &transport(),
        &source(&server.url(), b"right"),
        NetworkPolicy::Allowed,
        root.path(),
        100,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ArtifactError::DigestMismatch { .. }));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[tokio::test]
async fn bounded_offline_miss_is_zero_http_and_zero_artifact_files() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let error = download_verified_bounded(
        &transport(),
        &source(&server.url(), b"expected"),
        NetworkPolicy::Offline,
        root.path(),
        100,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ArtifactError::Offline));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[tokio::test]
async fn cancelling_a_bounded_partial_response_removes_its_private_file() {
    let mut server = mockito::Server::new_async().await;
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Gate(Arc::clone(&state));
    let request = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_chunked_body(move |writer| {
            writer.write_all(b"123")?;
            writer.flush()?;
            let (_released, _timeout) = state
                .1
                .wait_timeout_while(
                    state.0.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .map_err(|_| std::io::Error::other("fixture gate poisoned"))?;
            Ok(())
        })
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(&server.url(), b"123");
    let scratch = root.path().to_path_buf();
    let task = tokio::spawn(async move {
        download_verified_bounded(&transport(), &source, NetworkPolicy::Allowed, &scratch, 100)
            .await
    });
    let partial = tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::read_dir(root.path()).unwrap().count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    task.abort();
    let cancelled = task.await.unwrap_err();
    gate.release();
    assert!(
        partial.is_ok(),
        "fixture must reach a real temporary artifact before cancellation"
    );
    assert!(cancelled.is_cancelled());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}
