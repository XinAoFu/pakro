use mise_index::artifact::{ArtifactError, SourceArtifact};
use mise_store::Digest;

use mise_index::artifact::{ArtifactTransport, NetworkPolicy, download_verified};

#[tokio::test]
async fn correct_digest_returns_a_verified_file_for_both_algorithms() {
    let payload = b"fixed artifact contents";
    for digest in [
        Digest::from_sha256_bytes(payload),
        Digest::from_sha512_bytes(payload),
    ] {
        let mut server = mockito::Server::new_async().await;
        let response = server
            .mock("GET", "/object")
            .with_status(200)
            .with_body(payload)
            .expect(1)
            .create_async()
            .await;
        let temp = tempfile::tempdir().unwrap();
        let source =
            SourceArtifact::new("payload.bin", &format!("{}/object", server.url()), digest)
                .unwrap();
        let verified = download_verified(
            &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
            &source,
            NetworkPolicy::Allowed,
            temp.path(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(verified.path()).unwrap(), payload);
        assert_eq!(verified.digest(), digest);
        assert_eq!(verified.size(), payload.len() as u64);
        let verified_path = verified.path().to_path_buf();
        drop(verified);
        assert!(!verified_path.exists());
        response.assert_async().await;
    }
}

#[tokio::test]
async fn offline_refuses_before_network_or_temp_file_creation() {
    let mut server = mockito::Server::new_async().await;
    let response = server.mock("GET", "/object").expect(0).create_async().await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(b"expected"),
    )
    .unwrap();
    let result = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Offline,
        temp.path(),
    )
    .await;
    assert!(matches!(result, Err(ArtifactError::Offline)));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    response.assert_async().await;
}

#[tokio::test]
async fn changed_response_bytes_never_escape_as_a_verified_artifact() {
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .with_status(200)
        .with_body(b"changed")
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(b"expected"),
    )
    .unwrap();
    let result = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await;
    assert!(matches!(result, Err(ArtifactError::DigestMismatch { .. })));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    response.assert_async().await;
}

#[tokio::test]
async fn error_status_fails_even_when_body_digest_matches() {
    let payload = b"expected";
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .with_status(404)
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(payload),
    )
    .unwrap();
    let result = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await;
    assert!(matches!(result, Err(ArtifactError::Http(_))));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    response.assert_async().await;
}

#[tokio::test]
async fn complete_large_response_is_verified() {
    let payload: Vec<u8> = (0..2_000_000).map(|i| (i % 251) as u8).collect();
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .with_status(200)
        .with_body(payload.clone())
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(&payload),
    )
    .unwrap();
    let verified = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(verified.path()).unwrap(), payload);
    assert_eq!(verified.size(), 2_000_000);
    response.assert_async().await;
}

#[tokio::test]
async fn redirects_without_a_location_are_not_successful_artifacts() {
    let payload = b"expected";
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .with_status(302)
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(payload),
    )
    .unwrap();
    let result = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await;
    assert!(matches!(result, Err(ArtifactError::HttpStatus(status)) if status.as_u16() == 302));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    response.assert_async().await;
}

#[tokio::test]
async fn content_encoding_cannot_transform_the_locked_artifact_bytes() {
    let payload = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xffK\xce\xcf-(J-.NMQH,*\xc9LKL.QH\xce\xcf+I\xcd+)\x06\x00\x7f\xf0A\x95\x1c\x00\x00\x00";
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .with_status(200)
        .with_header("content-encoding", "gzip")
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.gz",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(payload),
    )
    .unwrap();
    let verified = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder().gzip(true)).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(verified.path()).unwrap(), payload);
    response.assert_async().await;
}

#[tokio::test]
async fn http_failure_diagnostics_do_not_reveal_request_queries() {
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/object")
        .match_query(mockito::Matcher::Any)
        .with_status(404)
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object?token=synthetic-secret", server.url()),
        Digest::from_sha256_bytes(b"expected"),
    )
    .unwrap();
    let error = download_verified(
        &ArtifactTransport::new(reqwest::Client::builder()).unwrap(),
        &source,
        NetworkPolicy::Allowed,
        temp.path(),
    )
    .await
    .unwrap_err();
    assert!(!format!("{error}").contains("synthetic-secret"));
    assert!(!format!("{error:?}").contains("synthetic-secret"));
    let mut cause = std::error::Error::source(&error);
    while let Some(inner) = cause {
        assert!(!format!("{inner}").contains("synthetic-secret"));
        assert!(!format!("{inner:?}").contains("synthetic-secret"));
        cause = inner.source();
    }
    response.assert_async().await;
}

struct ReleaseGate(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl ReleaseGate {
    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.release();
    }
}

async fn partial_stream_case(abort: bool) {
    use std::io;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    let prefix = b"first chunk";
    let suffix = b"second chunk";
    let payload = [prefix.as_slice(), suffix.as_slice()].concat();
    let mut server = mockito::Server::new_async().await;
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = ReleaseGate(state.clone());
    let response = server
        .mock("GET", "/object")
        .with_status(200)
        .with_chunked_body(move |writer| {
            writer.write_all(prefix)?;
            writer.flush()?;
            let (released, _) = state
                .1
                .wait_timeout_while(
                    state.0.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .map_err(|_| io::Error::other("fixture gate poisoned"))?;
            if !*released {
                return Err(io::Error::other("fixture gate timed out"));
            }
            if abort {
                Err(io::Error::other("fixture response aborted"))
            } else {
                writer.write_all(suffix)
            }
        })
        .expect(1)
        .create_async()
        .await;
    let temp = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "payload.bin",
        &format!("{}/object", server.url()),
        Digest::from_sha256_bytes(&payload),
    )
    .unwrap();
    let transport = ArtifactTransport::new(reqwest::Client::builder()).unwrap();
    let temp_root = temp.path().to_path_buf();
    let mut download = tokio::spawn(async move {
        download_verified(&transport, &source, NetworkPolicy::Allowed, &temp_root).await
    });
    let observed_prefix = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let paths: Vec<_> = std::fs::read_dir(temp.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            if paths
                .iter()
                .any(|path| std::fs::read(path).unwrap() == prefix)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    gate.release();
    let result = match tokio::time::timeout(Duration::from_secs(3), &mut download).await {
        Ok(result) => result.unwrap(),
        Err(_) => {
            download.abort();
            let _ = download.await;
            panic!("download did not finish after releasing the fixture");
        }
    };
    assert!(
        observed_prefix.is_ok(),
        "prefix must reach disk before response EOF"
    );
    if abort {
        assert!(matches!(result, Err(ArtifactError::Http(_))));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    } else {
        let verified = result.unwrap();
        assert_eq!(std::fs::read(verified.path()).unwrap(), payload);
    }
    response.assert_async().await;
}

#[tokio::test]
async fn partial_response_is_written_before_eof() {
    partial_stream_case(false).await;
}

#[tokio::test]
async fn interrupted_response_cleans_up_partial_files() {
    partial_stream_case(true).await;
}

#[test]
fn valid_sources_accept_unicode_and_both_digest_algorithms() {
    for digest in [
        Digest::from_sha256_bytes(b"payload"),
        Digest::from_sha512_bytes(b"payload"),
    ] {
        let source = SourceArtifact::new(
            "示例.bin",
            "https://example.test/files/object?temporary_token=secret",
            digest,
        )
        .unwrap();
        assert_eq!(source.filename(), "示例.bin");
        assert_eq!(source.digest(), digest);
        assert!(!format!("{source:?}").contains("secret"));
        assert!(!format!("{source:?}").contains("example.test"));
    }
}

#[test]
fn invalid_sources_are_rejected_before_fetching() {
    for filename in ["", ".", "..", "../x", "dir/x", "dir\\x", "x\0y"] {
        assert!(matches!(
            SourceArtifact::new(
                filename,
                "https://example.test/object",
                Digest::from_sha256_bytes(b"payload"),
            ),
            Err(ArtifactError::InvalidSource(_)),
        ));
    }
    for url in [
        "not a url",
        "file:///tmp/object",
        "ftp://example.test/object",
        "https://user:password@example.test/object",
        "https://example.test/object#sha256=abc",
    ] {
        assert!(matches!(
            SourceArtifact::new("payload.bin", url, Digest::from_sha256_bytes(b"payload")),
            Err(ArtifactError::InvalidSource(_)),
        ));
    }
}
