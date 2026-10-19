use super::*;

// The same 15 runtime behavior fixtures exercise the extracted shared kernel.
// Remove the old native test registration when the runtime switches to aliases.
type VerifiedRuntimeArtifact = VerifiedArtifact;
async fn acquire_runtime_artifact(
    source: &SourceArtifact,
    cache_root: &Path,
    transport: &ArtifactTransport,
    policy: NetworkPolicy,
) -> Result<VerifiedArtifact> {
    acquire_verified_artifact(source, cache_root, transport, policy, u64::MAX).await
}

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

fn transport() -> ArtifactTransport {
    ArtifactTransport::new(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5)),
    )
    .unwrap()
}

fn source(base: &str, filename: &str, digest: Digest) -> SourceArtifact {
    SourceArtifact::new(filename, &format!("{base}/archive"), digest).unwrap()
}

fn cached_path(root: &Path, source: &SourceArtifact) -> PathBuf {
    root.join("artifacts")
        .join(source.digest().to_string())
        .join(source.filename())
}

fn seed(root: &Path, source: &SourceArtifact, bytes: &[u8]) -> PathBuf {
    let path = cached_path(root, source);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

fn contents(artifact: &mut VerifiedRuntimeArtifact) -> Vec<u8> {
    artifact.rewind().unwrap();
    let mut bytes = Vec::new();
    artifact.read_to_end(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn online_verified_bytes_are_published_for_both_digest_algorithms() {
    let payload = b"verified runtime archive fixture";
    for digest in [
        Digest::from_sha256_bytes(payload),
        Digest::from_sha512_bytes(payload),
    ] {
        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/archive")
            .with_body(payload)
            .expect(1)
            .create_async()
            .await;
        let root = tempfile::tempdir().unwrap();
        let source = source(&server.url(), "python.tar.gz", digest);
        let mut artifact =
            acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Allowed)
                .await
                .unwrap();
        assert_eq!(artifact.filename(), "python.tar.gz");
        assert_eq!(artifact.digest(), digest);
        assert_eq!(artifact.size(), payload.len() as u64);
        assert_eq!(contents(&mut artifact), payload);
        assert_eq!(
            std::fs::read(cached_path(root.path(), &source)).unwrap(),
            payload
        );
        assert_eq!(
            contents(&mut artifact),
            payload,
            "the verified reader is seekable"
        );
        request.assert_async().await;
    }
}

#[tokio::test]
async fn complete_cache_is_verified_without_network_in_either_mode() {
    let payload = b"cached runtime archive";
    for policy in [NetworkPolicy::Offline, NetworkPolicy::Allowed] {
        let mut server = mockito::Server::new_async().await;
        let no_http = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let root = tempfile::tempdir().unwrap();
        let source = source(
            &server.url(),
            "python.tar.gz",
            Digest::from_sha256_bytes(payload),
        );
        seed(root.path(), &source, payload);
        let mut artifact = acquire_runtime_artifact(&source, root.path(), &transport(), policy)
            .await
            .unwrap();
        assert_eq!(contents(&mut artifact), payload);
        no_http.assert_async().await;
    }
}

#[tokio::test]
async fn multi_chunk_cached_archive_is_verified_without_truncation() {
    let payload: Vec<u8> = (0..2_000_000).map(|index| (index % 251) as u8).collect();
    let mut server = mockito::Server::new_async().await;
    let no_http = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(&payload),
    );
    seed(root.path(), &source, &payload);
    let mut artifact =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Offline)
            .await
            .unwrap();
    assert_eq!(artifact.size(), payload.len() as u64);
    assert_eq!(contents(&mut artifact), payload);
    no_http.assert_async().await;
}

#[tokio::test]
async fn offline_missing_cache_fails_before_network_and_directory_creation() {
    let mut server = mockito::Server::new_async().await;
    let no_http = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let absent = root.path().join("missing-cache");
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(b"expected"),
    );
    let error = acquire_runtime_artifact(&source, &absent, &transport(), NetworkPolicy::Offline)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("offline"), "{error}");
    assert!(error.to_string().contains("missing"), "{error}");
    assert!(!absent.exists());
    no_http.assert_async().await;
}

#[tokio::test]
async fn corrupt_cache_never_becomes_a_new_expected_digest_or_network_miss() {
    for policy in [NetworkPolicy::Offline, NetworkPolicy::Allowed] {
        let mut server = mockito::Server::new_async().await;
        let no_http = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let root = tempfile::tempdir().unwrap();
        let source = source(
            &server.url(),
            "python.tar.gz",
            Digest::from_sha256_bytes(b"expected"),
        );
        let path = seed(root.path(), &source, b"corrupt cache");
        let error = acquire_runtime_artifact(&source, root.path(), &transport(), policy)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("cache"), "{error:#}");
        assert!(format!("{error:#}").contains("digest"), "{error:#}");
        assert_eq!(std::fs::read(path).unwrap(), b"corrupt cache");
        no_http.assert_async().await;
    }
}

#[tokio::test]
async fn capability_bytes_survive_cache_overwrite_and_path_replacement() {
    let payload = b"private verified snapshot";
    let mut server = mockito::Server::new_async().await;
    let no_http = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(payload),
    );
    let path = seed(root.path(), &source, payload);
    let mut artifact =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Offline)
            .await
            .unwrap();
    std::fs::write(&path, b"same inode was overwritten").unwrap();
    assert_eq!(contents(&mut artifact), payload);
    let replacement = root.path().join("replacement");
    std::fs::write(&replacement, b"pathname was replaced").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(contents(&mut artifact), payload);
    assert_eq!(artifact.digest(), source.digest());
    let error =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Offline)
            .await
            .unwrap_err();
    assert!(format!("{error:#}").contains("digest"), "{error:#}");
    no_http.assert_async().await;
}

#[tokio::test]
async fn cache_keys_separate_filename_digest_and_algorithm() {
    let root = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let mut sources = Vec::new();
    for (filename, bytes, digest) in [
        (
            "python.tar.gz",
            b"original".as_slice(),
            Digest::from_sha256_bytes(b"original"),
        ),
        (
            "renamed.tar.gz",
            b"original".as_slice(),
            Digest::from_sha256_bytes(b"original"),
        ),
        (
            "python.tar.gz",
            b"changed".as_slice(),
            Digest::from_sha256_bytes(b"changed"),
        ),
        (
            "python.tar.gz",
            b"original".as_slice(),
            Digest::from_sha512_bytes(b"original"),
        ),
    ] {
        let source = source(&server.url(), filename, digest);
        seed(root.path(), &source, bytes);
        sources.push((source, bytes));
    }
    let no_http = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let paths: std::collections::BTreeSet<_> = sources
        .iter()
        .map(|(s, _)| cached_path(root.path(), s))
        .collect();
    assert_eq!(paths.len(), 4);
    for (source, expected) in sources {
        let mut artifact =
            acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Offline)
                .await
                .unwrap();
        assert_eq!(contents(&mut artifact), expected);
    }
    no_http.assert_async().await;
}

#[tokio::test]
async fn wrong_http_digest_publishes_no_cache_or_staging_file() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/archive")
        .with_body("wrong bytes")
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(b"expected"),
    );
    let error =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Allowed)
            .await
            .unwrap_err();
    assert!(format!("{error:#}").contains("digest"), "{error:#}");
    assert!(!cached_path(root.path(), &source).exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[tokio::test]
async fn http_failure_does_not_publish_and_redacts_source_query() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/archive")
        .match_query(mockito::Matcher::Any)
        .with_status(404)
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = SourceArtifact::new(
        "python.tar.gz",
        &format!("{}/archive?token=synthetic-runtime-secret", server.url()),
        Digest::from_sha256_bytes(b"expected"),
    )
    .unwrap();
    let error =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Allowed)
            .await
            .unwrap_err();
    assert!(!format!("{error:#}").contains("synthetic-runtime-secret"));
    assert!(!format!("{error:?}").contains("synthetic-runtime-secret"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[tokio::test]
async fn content_encoding_preserves_exact_locked_bytes() {
    let payload = b"\x1f\x8braw locked archive bytes";
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("GET", "/archive")
        .with_header("content-encoding", "gzip")
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(payload),
    );
    let transport =
        ArtifactTransport::new(reqwest::Client::builder().no_proxy().gzip(true)).unwrap();
    let mut artifact =
        acquire_runtime_artifact(&source, root.path(), &transport, NetworkPolicy::Allowed)
            .await
            .unwrap();
    assert_eq!(contents(&mut artifact), payload);
    assert_eq!(
        std::fs::read(cached_path(root.path(), &source)).unwrap(),
        payload
    );
    request.assert_async().await;
}

async fn concurrent_publication_case(corrupt: bool) {
    let payload = b"concurrently verified archive";
    let root = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(payload),
    );
    let path = cached_path(root.path(), &source);
    let competing_path = path.clone();
    let request = server
        .mock("GET", "/archive")
        .with_chunked_body(move |writer| {
            std::fs::create_dir_all(competing_path.parent().unwrap())?;
            std::fs::write(
                &competing_path,
                if corrupt {
                    b"corrupt".as_slice()
                } else {
                    payload.as_slice()
                },
            )?;
            writer.write_all(payload)
        })
        .expect(1)
        .create_async()
        .await;
    let result =
        acquire_runtime_artifact(&source, root.path(), &transport(), NetworkPolicy::Allowed).await;
    if corrupt {
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains("cache"), "{error:#}");
        assert!(format!("{error:#}").contains("digest"), "{error:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
    } else {
        let mut artifact = result.unwrap();
        assert_eq!(contents(&mut artifact), payload);
        assert_eq!(std::fs::read(&path).unwrap(), payload);
    }
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
    request.assert_async().await;
}

#[tokio::test]
async fn concurrent_valid_cache_publication_is_verified_and_accepted() {
    concurrent_publication_case(false).await;
}

#[tokio::test]
async fn concurrent_corrupt_cache_publication_is_rejected_without_overwriting_it() {
    concurrent_publication_case(true).await;
}

struct ReleaseGate(Arc<(Mutex<bool>, Condvar)>);

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

async fn streamed_publication_case(interrupted: bool, cancelled: bool) {
    let prefix = b"archive first chunk";
    let suffix = b"archive last chunk";
    let payload = [prefix.as_slice(), suffix.as_slice()].concat();
    let root = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let started = Arc::new(AtomicBool::new(false));
    let observed_started = Arc::clone(&started);
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = ReleaseGate(Arc::clone(&state));
    let request = server
        .mock("GET", "/archive")
        .with_chunked_body(move |writer| {
            writer.write_all(prefix)?;
            writer.flush()?;
            observed_started.store(true, Ordering::SeqCst);
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
            if interrupted {
                Err(io::Error::other("fixture response interrupted"))
            } else {
                writer.write_all(suffix)
            }
        })
        .expect(1)
        .create_async()
        .await;
    let source = source(
        &server.url(),
        "python.tar.gz",
        Digest::from_sha256_bytes(&payload),
    );
    let final_path = cached_path(root.path(), &source);
    let cache_root = root.path().to_path_buf();
    let mut acquisition = tokio::spawn(async move {
        acquire_runtime_artifact(&source, &cache_root, &transport(), NetworkPolicy::Allowed).await
    });
    let saw_prefix = tokio::time::timeout(Duration::from_secs(2), async {
        while !started.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let published_early = final_path.exists();
    if cancelled {
        acquisition.abort();
        let error = acquisition.await.unwrap_err();
        gate.release();
        assert!(error.is_cancelled());
        assert!(
            saw_prefix.is_ok(),
            "the HTTP prefix must arrive before cancellation"
        );
        assert!(!published_early);
        assert!(!final_path.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        request.assert_async().await;
        return;
    }
    gate.release();
    let result = match tokio::time::timeout(Duration::from_secs(3), &mut acquisition).await {
        Ok(result) => result.unwrap(),
        Err(_) => {
            acquisition.abort();
            let _ = acquisition.await;
            panic!("artifact acquisition did not finish after releasing response");
        }
    };
    assert!(saw_prefix.is_ok(), "the HTTP prefix must arrive before EOF");
    assert!(
        !published_early,
        "partial bytes must never appear at the final cache path"
    );
    if interrupted {
        assert!(result.is_err());
        assert!(!final_path.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    } else {
        let mut artifact = result.unwrap();
        assert_eq!(contents(&mut artifact), payload);
        assert_eq!(std::fs::read(final_path).unwrap(), payload);
    }
    request.assert_async().await;
}

#[tokio::test]
async fn partial_response_is_not_published_before_eof() {
    streamed_publication_case(false, false).await;
}

#[tokio::test]
async fn interrupted_response_cleans_up_without_publishing() {
    streamed_publication_case(true, false).await;
}

#[tokio::test]
async fn cancelled_acquisition_cleans_up_without_publishing() {
    streamed_publication_case(false, true).await;
}

#[tokio::test]
async fn zero_acquisition_limit_precedes_cache_http_and_directory_creation() {
    let mut server = mockito::Server::new_async().await;
    let no_http = server
        .mock("GET", "/archive")
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created");
    let source = source(&server.url(), "archive.whl", Digest::from_sha256_bytes(b""));
    let error =
        acquire_verified_artifact(&source, &missing, &transport(), NetworkPolicy::Allowed, 0)
            .await
            .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
    assert!(!missing.exists());
    no_http.assert_async().await;
}

#[tokio::test]
async fn exact_limit_cache_works_but_oversized_cache_never_falls_back_to_http() {
    let mut server = mockito::Server::new_async().await;
    let no_http = server
        .mock("GET", "/archive")
        .expect(0)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let payload = b"held cached wheel";
    let source = source(
        &server.url(),
        "archive.whl",
        Digest::from_sha256_bytes(payload),
    );
    let path = seed(root.path(), &source, payload);
    let mut exact = acquire_verified_artifact(
        &source,
        root.path(),
        &transport(),
        NetworkPolicy::Offline,
        payload.len() as u64,
    )
    .await
    .unwrap();
    assert_eq!(contents(&mut exact), payload);
    let error = acquire_verified_artifact(
        &source,
        root.path(),
        &transport(),
        NetworkPolicy::Allowed,
        payload.len() as u64 - 1,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("limit"), "{error:#}");
    assert_eq!(std::fs::read(path).unwrap(), payload);
    no_http.assert_async().await;
}

#[tokio::test]
async fn bounded_cold_http_never_publishes_an_oversized_archive() {
    let mut server = mockito::Server::new_async().await;
    let payload = b"an archive larger than the configured bound";
    let request = server
        .mock("GET", "/archive")
        .with_body(payload)
        .expect(1)
        .create_async()
        .await;
    let root = tempfile::tempdir().unwrap();
    let source = source(
        &server.url(),
        "archive.whl",
        Digest::from_sha256_bytes(payload),
    );
    let error = acquire_verified_artifact(
        &source,
        root.path(),
        &transport(),
        NetworkPolicy::Allowed,
        3,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("limit"), "{error:#}");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    request.assert_async().await;
}

#[test]
fn local_reader_exact_limit_binds_both_digests_and_is_rewindable() {
    let payload = b"local built-wheel artifact bytes";
    let root = tempfile::tempdir().unwrap();
    for digest in [
        Digest::from_sha256_bytes(payload),
        Digest::from_sha512_bytes(payload),
    ] {
        let mut artifact = spool_verified_reader(
            std::io::Cursor::new(payload),
            "demo-1.0-py3-none-any.whl",
            digest,
            payload.len() as u64,
            root.path(),
        )
        .unwrap();
        assert_eq!(artifact.filename(), "demo-1.0-py3-none-any.whl");
        assert_eq!(artifact.digest(), digest);
        assert_eq!(artifact.size(), payload.len() as u64);
        assert_eq!(contents(&mut artifact), payload);
        assert_eq!(contents(&mut artifact), payload);
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

struct UnreadableFixture;
impl Read for UnreadableFixture {
    fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
        panic!("invalid request must fail before reading its input")
    }
}

#[test]
fn local_reader_validates_zero_limit_and_filename_before_any_io() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created");
    let digest = Digest::from_sha256_bytes(b"");
    let error =
        spool_verified_reader(UnreadableFixture, "demo.whl", digest, 0, &missing).unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
    for name in [
        "",
        ".",
        "..",
        "../outside.whl",
        "one/two.whl",
        "one\\two.whl",
        "bad\0.whl",
        "bad\n.whl",
    ] {
        let error =
            spool_verified_reader(UnreadableFixture, name, digest, 1, &missing).unwrap_err();
        assert!(error.to_string().contains("filename"), "{error:#}");
    }
    assert!(!missing.exists());
}

struct CountingReader {
    input: std::io::Cursor<Vec<u8>>,
    observed: Arc<std::sync::atomic::AtomicUsize>,
}
impl Read for CountingReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(output)?;
        self.observed.fetch_add(count, Ordering::SeqCst);
        Ok(count)
    }
}

#[test]
fn local_reader_over_limit_consumes_only_the_limit_plus_one_sentinel() {
    let root = tempfile::tempdir().unwrap();
    let payload = vec![b'x'; 1_000_000];
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader = CountingReader {
        input: std::io::Cursor::new(payload.clone()),
        observed: Arc::clone(&observed),
    };
    let error = spool_verified_reader(
        reader,
        "archive.whl",
        Digest::from_sha256_bytes(&payload),
        17,
        root.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
    assert_eq!(observed.load(Ordering::SeqCst), 18);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

struct EndlessReader(Arc<std::sync::atomic::AtomicUsize>);
impl Read for EndlessReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        output.fill(b'x');
        self.0.fetch_add(output.len(), Ordering::SeqCst);
        Ok(output.len())
    }
}

#[test]
fn local_endless_reader_is_bounded_without_waiting_for_eof() {
    let root = tempfile::tempdir().unwrap();
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = spool_verified_reader(
        EndlessReader(Arc::clone(&observed)),
        "archive.whl",
        Digest::from_sha256_bytes(b"x"),
        5,
        root.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
    assert_eq!(observed.load(Ordering::SeqCst), 6);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn wrong_local_digest_never_returns_a_capability_or_published_file() {
    let root = tempfile::tempdir().unwrap();
    let error = spool_verified_reader(
        std::io::Cursor::new(b"different bytes"),
        "archive.whl",
        Digest::from_sha256_bytes(b"expected bytes"),
        100,
        root.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("digest mismatch"), "{error:#}");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

struct InterruptedReader(bool);
impl Read for InterruptedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.0 {
            return Err(io::Error::other("fixture reader interrupted"));
        }
        self.0 = true;
        output[0] = b'x';
        Ok(1)
    }
}

#[test]
fn local_read_error_after_exact_limit_is_not_successful_eof() {
    let root = tempfile::tempdir().unwrap();
    let error = spool_verified_reader(
        InterruptedReader(false),
        "archive.whl",
        Digest::from_sha256_bytes(b"x"),
        1,
        root.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("I/O"), "{error:#}");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn returned_local_snapshot_survives_source_overwrite_and_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mutable-built.whl");
    let scratch = root.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let payload = b"held authentic wheel bytes";
    for replacement in [false, true] {
        std::fs::write(&path, payload).unwrap();
        let mut held = spool_verified_reader(
            File::open(&path).unwrap(),
            "archive.whl",
            Digest::from_sha256_bytes(payload),
            payload.len() as u64,
            &scratch,
        )
        .unwrap();
        if replacement {
            std::fs::rename(&path, root.path().join("previous-built.whl")).unwrap();
        }
        std::fs::write(&path, b"mutated source path").unwrap();
        assert_eq!(contents(&mut held), payload);
    }
}

struct MutatingFileReader {
    file: File,
    path: PathBuf,
    first: bool,
}
impl Read for MutatingFileReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = if self.first {
            self.first = false;
            self.file.read(&mut output[..1])?
        } else {
            self.file.read(output)?
        };
        if self.file.stream_position()? == 1 {
            std::fs::write(&self.path, b"changed source bytes")?;
        }
        Ok(count)
    }
}

#[test]
fn a_source_file_changed_mid_read_cannot_match_the_old_expected_digest() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.whl");
    let payload = b"original file bytes";
    std::fs::write(&path, payload).unwrap();
    let reader = MutatingFileReader {
        file: File::open(&path).unwrap(),
        path,
        first: true,
    };
    let error = spool_verified_reader(
        reader,
        "archive.whl",
        Digest::from_sha256_bytes(payload),
        100,
        root.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("digest mismatch"), "{error:#}");
}

#[test]
fn local_snapshot_debug_does_not_disclose_input_bytes_or_scratch_path() {
    let root = tempfile::tempdir().unwrap();
    let payload = b"https://user:secret@private.invalid/local-built-wheel";
    let artifact = spool_verified_reader(
        std::io::Cursor::new(payload),
        "archive.whl",
        Digest::from_sha256_bytes(payload),
        payload.len() as u64,
        root.path(),
    )
    .unwrap();
    let debug = format!("{artifact:?}");
    for secret in ["secret", "private.invalid", root.path().to_str().unwrap()] {
        assert!(!debug.contains(secret), "{debug}");
    }
}
