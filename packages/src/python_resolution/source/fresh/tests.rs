//! Every successful prelude uses real Simple/file HTTP and an actually read
//! project lock. No fixture fabricates source owners, slots or content caps.

use super::*;
use pakro_index::python_source::{
    AuthScopeKey, CredentialRef, TransportRulesFingerprint, transport_rules_fingerprint,
};
use pakro_index::simple::{
    AuthOriginGrants, HttpOrigin, OriginAuthorization, SourceCredentials, SourceRequestRouter,
};
use reqwest::Url;
use sha2::{Sha256, Sha512};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

const FILENAME: &str = "demo_pkg-1.0-py3-none-any.whl";
const MEDIA: &str = "application/vnd.pypi.simple.v1+json";

struct NoCredentials;
impl SourceCredentials for NoCredentials {
    fn authorization_for(
        &self,
        _: &CredentialRef,
        _: &HttpOrigin,
        _: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        panic!("anonymous/offline fixture consulted credentials")
    }
}
struct Route {
    query: Option<&'static str>,
    fingerprint: u8,
}
impl SourceRequestRouter for Route {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        if self.fingerprint == 0 {
            transport_rules_fingerprint(None)
        } else {
            transport_rules_fingerprint(Some(&[("route", "different-explicit-route")]))
        }
    }
    fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
        let mut request = canonical.clone();
        if let Some(query) = self.query {
            request.set_query(Some(query));
        }
        Ok(request)
    }
}
const IDENTITY: Route = Route {
    query: None,
    fingerprint: 0,
};

#[derive(Clone)]
struct Request {
    target: String,
    headers: BTreeMap<String, String>,
}
struct Reply {
    status: u16,
    content_type: Option<&'static str>,
    body: Vec<u8>,
}
impl Reply {
    fn raw(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: None,
            body: body.into(),
        }
    }
}
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(handler: impl Fn(usize) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                // Darwin inherits O_NONBLOCK from the listener. Keep the
                // actual 250ms timeout rather than dropping a zero-byte peer.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") && header.len() < 64 * 1024 {
                    let mut byte = [0u8; 1];
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        break;
                    }
                    header.push(byte[0]);
                }
                if !header.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let Ok(header) = std::str::from_utf8(&header) else {
                    continue;
                };
                let mut lines = header.split("\r\n");
                let Some(target) = lines.next().and_then(|line| line.split_whitespace().nth(1))
                else {
                    continue;
                };
                let request = Request {
                    target: target.to_owned(),
                    headers: lines
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                        .collect(),
                };
                let count = {
                    let mut rows = captured.lock().unwrap();
                    let count = rows.len();
                    rows.push(request);
                    count
                };
                let reply = handler(count);
                let result = (|| -> io::Result<()> {
                    write!(
                        stream,
                        "HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n",
                        reply.status,
                        reply.body.len()
                    )?;
                    if let Some(media) = reply.content_type {
                        write!(stream, "Content-Type: {media}\r\n")?;
                    }
                    stream.write_all(b"\r\n")?;
                    stream.write_all(&reply.body)?;
                    stream.flush()
                })();
                if let Err(error) = result {
                    eprintln!(
                        "source owner fixture peer kind={:?} errno={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn page(locator: &str, hashes: serde_json::Value, size: Option<u64>) -> Vec<u8> {
    let mut file = serde_json::json!({
        "filename":FILENAME,"url":locator,"hashes":hashes,"requires-python":">=99"
    });
    if let Some(size) = size {
        file["size"] = serde_json::json!(size);
    }
    serde_json::to_vec(&serde_json::json!({
        "meta":{"api-version":"1.0"},"name":"demo-pkg","files":[file]
    }))
    .unwrap()
}
fn index_page(body: Vec<u8>) -> Peer {
    Peer::new(move |_| Reply {
        status: 200,
        content_type: Some(MEDIA),
        body: body.clone(),
    })
}
struct Fixture {
    client: SimpleIndexClient,
    registry: PythonRegistry,
    name: NormalizedPythonName,
    scope: AuthScopeKey,
    grants: AuthOriginGrants,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new(index: &Peer) -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            client: SimpleIndexClient::new(root.path().join("metadata"), Default::default())
                .unwrap(),
            registry: PythonRegistry::new("corp", &format!("{}/simple/", index.url), None).unwrap(),
            name: NormalizedPythonName::new("demo_pkg").unwrap(),
            scope: AuthScopeKey::from_local_revision([7; 32]),
            grants: AuthOriginGrants::default(),
            root,
        }
    }
    fn source_session<'a>(
        &'a self,
        credentials: &'a dyn SourceCredentials,
        route: &'a dyn SourceRequestRouter,
    ) -> PythonSourceSession<'a> {
        PythonSourceSession {
            registry: &self.registry,
            auth_scope: &self.scope,
            credentials,
            auth_origins: &self.grants,
            transport: route,
        }
    }
    fn file_session<'a>(
        &'a self,
        credentials: &'a dyn SourceCredentials,
        route: &'a dyn SourceRequestRouter,
    ) -> SourceFileSession<'a> {
        SourceFileSession {
            credential_ref: None,
            auth_scope: &self.scope,
            credentials,
            auth_origins: &self.grants,
            transport: route,
        }
    }
    fn policy(network: NetworkPolicy, refresh: bool) -> SimpleFetchPolicy {
        SimpleFetchPolicy {
            network,
            refresh,
            fresh_for: Duration::from_secs(60),
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    }
    async fn fetch(&self) -> BoundRegistryProject {
        bound(
            fetch_registry_project(
                &self.client,
                self.source_session(&NoCredentials, &IDENTITY),
                &self.name,
                Self::policy(NetworkPolicy::Allowed, true),
                Default::default(),
            )
            .await,
        )
    }
}
fn bound(value: SourceResolutionResult<RegistryProjectLookup>) -> BoundRegistryProject {
    match value.unwrap() {
        RegistryProjectLookup::Present(project) => project,
        RegistryProjectLookup::Missing404 => panic!("a valid project was lost"),
    }
}

use crate::packages::python_resolution::lock_model::PinEvidence;
use crate::packages::python_resolution::lock_session::{
    BoundPythonLock, FilePinLookup, ResolutionIntent,
};
use std::fs;
use std::time::Instant;

fn open_lock(fixture: &Fixture) -> BoundPythonLock {
    BoundPythonLock::acquire(
        &fixture.root.path().join("pakro.lock"),
        Default::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap()
}
fn exact_ref(project: &BoundRegistryProject) -> RegistryFileRef {
    selected_file_ref(
        &project.select_exact_file(FILENAME).unwrap(),
        Default::default(),
    )
    .unwrap()
}
fn vacant<'lock>(lock: &'lock BoundPythonLock, file: &RegistryFileRef) -> FreshFileSlot<'lock> {
    match lock.lookup(file, ResolutionIntent::Resolve).unwrap() {
        FilePinLookup::Fresh(slot) => {
            slot.recheck().unwrap();
            slot
        }
        other => panic!("real missing exact pin did not yield vacancy: {other:?}"),
    }
}
async fn pin<'lock>(
    slot: FreshFileSlot<'lock>,
    project: &BoundRegistryProject,
    fixture: &Fixture,
    limits: SourceObservationLimits,
) -> FreshPinResult<FreshPinnedArtifact<'lock>> {
    acquire_fresh_registry_file(
        slot,
        project.select_exact_file(FILENAME).unwrap(),
        &fixture.client,
        fixture.file_session(&NoCredentials, &IDENTITY),
        NetworkPolicy::Allowed,
        limits,
        Default::default(),
        &fixture.root.path().join("fresh-scratch"),
    )
    .await
}
fn assert_unpublished(fixture: &Fixture) {
    for path in [
        "fresh-scratch",
        "scratch",
        "artifact-cache",
        "refs",
        "environment",
    ] {
        let path = fixture.root.path().join(path);
        assert!(!path.exists() || path.read_dir().unwrap().next().is_none());
    }
}

#[tokio::test]
async fn real_fresh_vacancy_and_unknown_hash_facts_close_actual_observed_content() {
    for hashes in [
        serde_json::json!({}),
        serde_json::json!({"future-hash":"012345"}),
    ] {
        let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
        let index = index_page(page(&format!("{}/opaque", files.url), hashes, Some(3)));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = open_lock(&fixture);
        let file = exact_ref(&project);
        let mut closed = pin(vacant(&lock, &file), &project, &fixture, Default::default())
            .await
            .unwrap();
        assert_eq!(closed.pin().file(), &file);
        assert_eq!(
            closed.pin().evidence(),
            PinEvidence::ObservedDuringFreshResolution
        );
        assert_eq!(
            closed.digest(),
            Digest::Sha256(Sha256::digest(b"abc").into())
        );
        assert_eq!(closed.pin().digest(), closed.digest());
        assert_eq!(closed.pin().size(), 3);
        assert_eq!(closed.size(), 3);
        assert_eq!(closed.filename(), FILENAME);
        assert_ne!(closed.source_key().as_bytes(), &[0; 32]);
        closed.recheck().unwrap();
        let mut bytes = Vec::new();
        closed.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
        let requests = files.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, "/opaque");
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(!fixture.root.path().join("pakro.lock").exists());
        assert_unpublished(&fixture);
    }
}

#[tokio::test]
async fn exact_limit_same_snapshot_remains_readable_after_server_bytes_change() {
    let served = Arc::new(Mutex::new(b"abc".to_vec()));
    let current = Arc::clone(&served);
    let files = Peer::new(move |_| Reply::raw(current.lock().unwrap().clone()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    let limits = SourceObservationLimits {
        transfer: SourceFileLimits {
            max_wire_bytes: 3,
            ..Default::default()
        },
        max_spool_bytes: 6,
        ..Default::default()
    };
    let mut closed = pin(
        vacant(&lock, &exact_ref(&project)),
        &project,
        &fixture,
        limits,
    )
    .await
    .unwrap();
    *served.lock().unwrap() = b"different".to_vec();
    closed.seek(SeekFrom::End(0)).unwrap();
    closed.rewind().unwrap();
    let mut bytes = Vec::new();
    closed.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abc");
    assert_eq!(
        closed.digest(),
        Digest::Sha256(Sha256::digest(&bytes).into())
    );
    assert_eq!(files.requests().len(), 1);
    assert_unpublished(&fixture);
}

#[tokio::test]
async fn dropping_a_started_future_cancels_its_unique_slot_and_publishes_nothing() {
    let entered = Arc::new(AtomicBool::new(false));
    let entered_peer = Arc::clone(&entered);
    let release = Arc::new(AtomicBool::new(false));
    let release_peer = Arc::clone(&release);
    let files = Peer::new(move |_| {
        entered_peer.store(true, Ordering::Release);
        let start = Instant::now();
        while !release_peer.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(2));
        }
        Reply::raw(b"abc".to_vec())
    });
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    let future = pin(
        vacant(&lock, &exact_ref(&project)),
        &project,
        &fixture,
        Default::default(),
    );
    let mut future = Box::pin(future);
    let (completed, reached) = tokio::select! {
        _result = &mut future => (true, entered.load(Ordering::Acquire)),
        result = tokio::time::timeout(Duration::from_secs(2), async {
            while !entered.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }) => (false, result.is_ok()),
    };
    drop(future);
    release.store(true, Ordering::Release);
    assert!(
        !completed,
        "fresh acquisition completed before cancellation"
    );
    assert!(reached, "cancellation never reached actual file transfer");
    assert_eq!(files.requests().len(), 1);
    assert!(!fixture.root.path().join("pakro.lock").exists());
    assert_unpublished(&fixture);
}

#[tokio::test]
async fn complete_file_identity_is_checked_before_the_artifact_request() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    let exact = exact_ref(&project);
    for altered in [
        RegistryFileRefInput {
            registry_id: "another",
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: exact.project(),
            version: exact.version(),
            filename: exact.filename(),
            kind: exact.kind(),
        },
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: [9; 32],
            project: exact.project(),
            version: exact.version(),
            filename: exact.filename(),
            kind: exact.kind(),
        },
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: "other",
            version: "1.0",
            filename: "other-1.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: exact.project(),
            version: "2.0",
            filename: "demo_pkg-2.0-py3-none-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: exact.project(),
            version: "1.0",
            filename: "demo_pkg-1.0-cp313-cp313-any.whl",
            kind: RegistryArtifactKind::Wheel,
        },
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: exact.project(),
            version: "1.0",
            filename: "demo_pkg-1.0.tar.gz",
            kind: RegistryArtifactKind::Sdist,
        },
    ] {
        let altered = RegistryFileRef::new(altered, Default::default()).unwrap();
        assert_eq!(
            pin(
                vacant(&lock, &altered),
                &project,
                &fixture,
                Default::default()
            )
            .await
            .unwrap_err(),
            FreshPinError::IdentityMismatch
        );
        assert_eq!(files.requests().len(), 0);
        assert_unpublished(&fixture);
    }
}

#[tokio::test]
async fn equivalent_pep440_versions_do_not_create_an_identity_mismatch() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    let exact = exact_ref(&project);
    let equivalent = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: exact.registry_id(),
            canonical_base_fingerprint: *exact.canonical_base_fingerprint(),
            project: exact.project(),
            version: "1.0.0",
            filename: exact.filename(),
            kind: exact.kind(),
        },
        Default::default(),
    )
    .unwrap();
    let closed = pin(
        vacant(&lock, &equivalent),
        &project,
        &fixture,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(closed.pin().file(), &equivalent);
    assert_eq!(closed.size(), 3);
    assert_eq!(files.requests().len(), 1);
    closed.recheck().unwrap();
}

#[tokio::test]
async fn strong_advertised_hashes_never_fall_back_to_new_observed_pins() {
    for sha512 in [false, true] {
        for corrupt in [false, true] {
            let files = Peer::new(move |_| {
                Reply::raw(if corrupt {
                    b"abd".to_vec()
                } else {
                    b"abc".to_vec()
                })
            });
            let digest = if sha512 {
                Digest::Sha512(Sha512::digest(b"abc").into())
            } else {
                Digest::Sha256(Sha256::digest(b"abc").into())
            };
            let hash = if sha512 { "sha512" } else { "sha256" };
            let index = index_page(page(
                &format!("{}/body", files.url),
                serde_json::json!({(hash):digest.hex()}),
                Some(3),
            ));
            let fixture = Fixture::new(&index);
            let project = fixture.fetch().await;
            let lock = open_lock(&fixture);
            let result = pin(
                vacant(&lock, &exact_ref(&project)),
                &project,
                &fixture,
                Default::default(),
            )
            .await;
            if corrupt {
                assert_eq!(
                    result.unwrap_err(),
                    FreshPinError::Source(SourceResolutionError::ByteIdentity)
                );
            } else {
                let closed = result.unwrap();
                assert_eq!(closed.pin().evidence(), PinEvidence::Advertised);
                assert_eq!(closed.digest(), digest);
            }
            assert_eq!(files.requests().len(), 1);
            assert_unpublished(&fixture);
        }
    }
}

#[tokio::test]
async fn declared_size_and_preflight_budgets_fail_without_pin_or_publication() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    for limits in [
        SourceObservationLimits {
            max_spool_bytes: 0,
            ..Default::default()
        },
        SourceObservationLimits {
            transfer: SourceFileLimits {
                max_wire_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        },
        SourceObservationLimits {
            transfer: SourceFileLimits {
                max_wire_bytes: 3,
                ..Default::default()
            },
            max_spool_bytes: 5,
            ..Default::default()
        },
        SourceObservationLimits {
            transfer: SourceFileLimits {
                max_wire_bytes: u64::MAX,
                ..Default::default()
            },
            max_spool_bytes: u64::MAX,
            ..Default::default()
        },
    ] {
        assert_eq!(
            pin(
                vacant(&lock, &exact_ref(&project)),
                &project,
                &fixture,
                limits
            )
            .await
            .unwrap_err(),
            FreshPinError::InvalidLimits
        );
        assert_eq!(files.requests().len(), 0);
        assert_unpublished(&fixture);
    }
    drop(lock);
    let mismatched = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(4),
    ));
    let fixture = Fixture::new(&mismatched);
    let project = fixture.fetch().await;
    let lock = open_lock(&fixture);
    assert_eq!(
        pin(
            vacant(&lock, &exact_ref(&project)),
            &project,
            &fixture,
            Default::default()
        )
        .await
        .unwrap_err(),
        FreshPinError::Source(SourceResolutionError::ByteIdentity)
    );
    assert_eq!(files.requests().len(), 1);
    assert_unpublished(&fixture);
}

#[tokio::test]
async fn external_lock_changes_revoke_before_and_after_actual_source_transfer() {
    for during in [false, true] {
        let mutation = Arc::new(Mutex::new(None::<std::path::PathBuf>));
        let target = Arc::clone(&mutation);
        let files = Peer::new(move |_| {
            if let Some(path) = target.lock().unwrap().as_ref() {
                fs::write(path, "lockfile_version=4\n").unwrap();
            }
            Reply::raw(b"abc".to_vec())
        });
        let index = index_page(page(
            &format!("{}/body", files.url),
            serde_json::json!({}),
            Some(3),
        ));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = open_lock(&fixture);
        let slot = vacant(&lock, &exact_ref(&project));
        let path = fixture.root.path().join("pakro.lock");
        if during {
            *mutation.lock().unwrap() = Some(path.clone());
        } else {
            fs::write(&path, "lockfile_version=4\n").unwrap();
        }
        assert_eq!(
            pin(slot, &project, &fixture, Default::default())
                .await
                .unwrap_err(),
            FreshPinError::Revoked
        );
        assert_eq!(files.requests().len(), usize::from(during));
        assert_eq!(fs::read_to_string(&path).unwrap(), "lockfile_version=4\n");
        assert_unpublished(&fixture);
    }
}

#[tokio::test]
async fn secret_locators_remain_local_but_portable_pins_survive_locator_renewal() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let body = Arc::new(Mutex::new(page(
        &format!("{}/body?token=synthetic-source-secret-one", files.url),
        serde_json::json!({}),
        Some(3),
    )));
    let served = Arc::clone(&body);
    let index = Peer::new(move |_| Reply {
        status: 200,
        content_type: Some(MEDIA),
        body: served.lock().unwrap().clone(),
    });
    let fixture = Fixture::new(&index);
    let lock = open_lock(&fixture);
    let first_project = fixture.fetch().await;
    let first = pin(
        vacant(&lock, &exact_ref(&first_project)),
        &first_project,
        &fixture,
        Default::default(),
    )
    .await
    .unwrap();
    *body.lock().unwrap() = page(
        &format!("{}/body?token=synthetic-source-secret-two", files.url),
        serde_json::json!({}),
        Some(3),
    );
    let second_project = fixture.fetch().await;
    let second = pin(
        vacant(&lock, &exact_ref(&second_project)),
        &second_project,
        &fixture,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(first.pin(), second.pin());
    assert_ne!(first.source_key(), second.source_key());
    assert!(!format!("{first:?} {second:?} {:?}", first.pin()).contains("synthetic-source-secret"));
    assert_eq!(files.requests().len(), 2);
    assert_unpublished(&fixture);
}

#[tokio::test]
async fn successful_candidate_and_failed_bytes_both_leave_existing_lock_and_outputs_untouched() {
    for corrupt in [false, true] {
        let files = Peer::new(move |_| {
            Reply::raw(if corrupt {
                b"abd".to_vec()
            } else {
                b"abc".to_vec()
            })
        });
        let digest = Digest::Sha256(Sha256::digest(b"abc").into());
        let index = index_page(page(
            &format!("{}/body", files.url),
            serde_json::json!({"sha256":digest.hex()}),
            Some(3),
        ));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let path = fixture.root.path().join("pakro.lock");
        let original = "lockfile_version=4\n[tools]\n";
        fs::write(&path, original).unwrap();
        let lock = open_lock(&fixture);
        let result = pin(
            vacant(&lock, &exact_ref(&project)),
            &project,
            &fixture,
            Default::default(),
        )
        .await;
        if corrupt {
            assert_eq!(
                result.unwrap_err(),
                FreshPinError::Source(SourceResolutionError::ByteIdentity)
            );
        } else {
            let closed = result.unwrap();
            closed.recheck().unwrap();
            assert_eq!(closed.pin().digest(), digest);
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(files.requests().len(), 1);
        assert_unpublished(&fixture);
    }
}
