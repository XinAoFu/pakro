//! Application owners come from real Simple and file HTTP requests. No test
//! constructs BoundRegistryProject, SelectedRegistryFile or acquired owners.

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
struct LocalCredentials {
    calls: Mutex<usize>,
}
impl SourceCredentials for LocalCredentials {
    fn authorization_for(
        &self,
        reference: &CredentialRef,
        origin: &HttpOrigin,
        policy: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        assert_eq!(reference.as_str(), "index-ref");
        assert_eq!(policy, NetworkPolicy::Allowed);
        *self.calls.lock().unwrap() += 1;
        Ok(Some(
            OriginAuthorization::new(
                origin.clone(),
                reqwest::header::HeaderValue::from_static("Bearer synthetic-index-secret"),
            )
            .unwrap(),
        ))
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
    async fn acquire(
        &self,
        project: &BoundRegistryProject,
    ) -> SourceResolutionResult<AcquiredRegistryFile> {
        let selected = project.select_exact_file(FILENAME)?;
        acquire_registry_file(
            &selected,
            &self.client,
            self.file_session(&NoCredentials, &IDENTITY),
            NetworkPolicy::Allowed,
            SourceObservationLimits::default(),
            &self.root.path().join("scratch"),
        )
        .await
    }
}
fn bound(value: SourceResolutionResult<RegistryProjectLookup>) -> BoundRegistryProject {
    match value.unwrap() {
        RegistryProjectLookup::Present(project) => project,
        RegistryProjectLookup::Missing404 => panic!("a valid project was lost"),
    }
}
fn observed(value: SourceResolutionResult<AcquiredRegistryFile>) -> BoundSourceObservation {
    match value.unwrap() {
        AcquiredRegistryFile::Observed(source) => source,
        AcquiredRegistryFile::Expected(_) => {
            panic!("a missing hash was presented as independent expected integrity")
        }
    }
}

#[tokio::test]
async fn actual_simple_and_file_requests_mint_a_private_readonly_source_observation() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/opaque", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    assert_eq!(project.registry_id(), fixture.registry.id());
    assert_eq!(project.name(), &fixture.name);
    assert_ne!(project.source_key().as_bytes(), &[0; 32]);
    let selected = project.select_exact_file(FILENAME).unwrap();
    assert_eq!(selected.filename(), FILENAME);
    assert_eq!(selected.version(), Some("1.0"));
    let mut source = observed(fixture.acquire(&project).await);
    assert_eq!(source.registry_id(), fixture.registry.id());
    assert_eq!(source.project(), &fixture.name);
    assert_eq!(source.filename(), FILENAME);
    assert_eq!(source.version(), Some("1.0"));
    assert!(matches!(source.kind(), PythonDistributionKind::Wheel(_)));
    assert_eq!(source.size(), 3);
    assert_eq!(
        source.observed_sha256(),
        <[u8; 32]>::from(Sha256::digest(b"abc"))
    );
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abc");
    source.rewind().unwrap();
    bytes.clear();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abc");
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    assert!(
        !fixture
            .root
            .path()
            .join("scratch")
            .read_dir()
            .unwrap()
            .any(|_| true)
    );
}

#[tokio::test]
async fn same_registry_id_at_different_bases_never_aliases_the_source_owner() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let body = page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    );
    let left = index_page(body.clone());
    let right = index_page(body);
    let a = Fixture::new(&left);
    let b = Fixture::new(&right);
    let pa = a.fetch().await;
    let pb = b.fetch().await;
    assert_eq!(pa.registry_id(), pb.registry_id());
    assert_ne!(pa.source_key(), pb.source_key());
    let oa = observed(a.acquire(&pa).await);
    let ob = observed(b.acquire(&pb).await);
    assert_ne!(oa.source_key(), ob.source_key());
    assert_eq!(oa.filename(), ob.filename());
    assert_eq!(oa.observed_sha256(), ob.observed_sha256());
    assert_eq!(left.requests().len(), 1);
    assert_eq!(right.requests().len(), 1);
    assert_eq!(files.requests().len(), 2);
}

#[tokio::test]
async fn source_keys_cover_auth_scope_routing_and_actual_initial_request_uri() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let mut fixture = Fixture::new(&index);
    let original = fixture.fetch().await.source_key();
    assert_eq!(fixture.fetch().await.source_key(), original);
    fixture.scope = AuthScopeKey::from_local_revision([8; 32]);
    assert_ne!(fixture.fetch().await.source_key(), original);
    fixture.scope = AuthScopeKey::from_local_revision([7; 32]);
    let changed_rules = Route {
        query: None,
        fingerprint: 1,
    };
    let changed = bound(
        fetch_registry_project(
            &fixture.client,
            fixture.source_session(&NoCredentials, &changed_rules),
            &fixture.name,
            Fixture::policy(NetworkPolicy::Allowed, true),
            Default::default(),
        )
        .await,
    );
    assert_ne!(changed.source_key(), original);
    let changed_request = Route {
        query: Some("ticket=synthetic-project-secret"),
        fingerprint: 0,
    };
    let changed = bound(
        fetch_registry_project(
            &fixture.client,
            fixture.source_session(&NoCredentials, &changed_request),
            &fixture.name,
            Fixture::policy(NetworkPolicy::Allowed, true),
            Default::default(),
        )
        .await,
    );
    assert_ne!(changed.source_key(), original);
    assert!(!format!("{changed:?}").contains("synthetic-"));
    assert!(
        index
            .requests()
            .last()
            .unwrap()
            .target
            .contains("ticket=synthetic-project-secret")
    );
    assert!(files.requests().is_empty());
}

#[tokio::test]
async fn exact_filename_selection_keeps_incompatible_facts_and_never_falls_back() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/opaque", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let selected = project.select_exact_file(FILENAME).unwrap();
    assert_eq!(selected.version(), Some("1.0"));
    assert_eq!(
        project
            .select_exact_file("demo_pkg-2.0-py3-none-any.whl")
            .unwrap_err(),
        SourceResolutionError::FileMissing
    );
    assert_eq!(
        project
            .select_exact_file("demo_pkg-1.0.tar.gz")
            .unwrap_err(),
        SourceResolutionError::FileMissing
    );
    assert!(files.requests().is_empty());
    assert_eq!(observed(fixture.acquire(&project).await).size(), 3);
    assert_eq!(files.requests().len(), 1);
}

#[tokio::test]
async fn declared_artifact_sizes_are_bound_to_complete_observed_bytes() {
    for declared in [3u64, 4] {
        let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
        let index = index_page(page(
            &format!("{}/body", files.url),
            serde_json::json!({}),
            Some(declared),
        ));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let result = fixture.acquire(&project).await;
        if declared == 3 {
            assert_eq!(observed(result).size(), 3);
        } else {
            assert_eq!(result.unwrap_err(), SourceResolutionError::ByteIdentity);
        }
        assert_eq!(files.requests().len(), 1);
        assert!(
            !fixture
                .root
                .path()
                .join("scratch")
                .read_dir()
                .unwrap()
                .any(|_| true)
        );
    }
}

#[tokio::test]
async fn advertised_strong_digests_use_expected_verification_without_observed_fallback() {
    for sha512 in [false, true] {
        for corrupt in [false, true] {
            let files = Peer::new(move |_| {
                Reply::raw(if corrupt {
                    b"abd".to_vec()
                } else {
                    b"abc".to_vec()
                })
            });
            let expected = if sha512 {
                Digest::Sha512(Sha512::digest(b"abc").into())
            } else {
                Digest::Sha256(Sha256::digest(b"abc").into())
            };
            let algorithm = if sha512 { "sha512" } else { "sha256" };
            let index = index_page(page(
                &format!("{}/body", files.url),
                serde_json::json!({(algorithm):expected.hex()}),
                Some(3),
            ));
            let fixture = Fixture::new(&index);
            let project = fixture.fetch().await;
            let result = fixture.acquire(&project).await;
            if corrupt {
                assert_eq!(result.unwrap_err(), SourceResolutionError::ByteIdentity);
            } else {
                let AcquiredRegistryFile::Expected(mut source) = result.unwrap() else {
                    panic!("known expected digest was downgraded to observation")
                };
                assert_eq!(source.digest(), expected);
                assert_eq!(source.filename(), FILENAME);
                assert_eq!(source.size(), 3);
                assert_ne!(source.source_key().as_bytes(), &[0; 32]);
                let mut body = Vec::new();
                source.read_to_end(&mut body).unwrap();
                assert_eq!(body, b"abc");
            }
            assert_eq!(files.requests().len(), 1);
        }
    }
}

#[tokio::test]
async fn offline_cache_precedes_credentials_and_missing_artifacts_do_not_stage_bytes() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let mut fixture = Fixture::new(&index);
    fixture.registry = PythonRegistry::new(
        "corp",
        &format!("{}/simple/", index.url),
        Some(CredentialRef::new("index-ref").unwrap()),
    )
    .unwrap();
    let credentials = LocalCredentials {
        calls: Mutex::new(0),
    };
    let project = bound(
        fetch_registry_project(
            &fixture.client,
            fixture.source_session(&credentials, &IDENTITY),
            &fixture.name,
            Fixture::policy(NetworkPolicy::Allowed, true),
            Default::default(),
        )
        .await,
    );
    assert_eq!(*credentials.calls.lock().unwrap(), 1);
    assert_eq!(
        index.requests()[0].headers.get("authorization").unwrap(),
        "Bearer synthetic-index-secret"
    );
    let offline = bound(
        fetch_registry_project(
            &fixture.client,
            fixture.source_session(&NoCredentials, &IDENTITY),
            &fixture.name,
            Fixture::policy(NetworkPolicy::Offline, false),
            Default::default(),
        )
        .await,
    );
    assert_eq!(offline.source_key(), project.source_key());
    let selected = offline.select_exact_file(FILENAME).unwrap();
    let scratch = fixture.root.path().join("never-created");
    let error = acquire_registry_file(
        &selected,
        &fixture.client,
        fixture.file_session(&NoCredentials, &IDENTITY),
        NetworkPolicy::Offline,
        Default::default(),
        &scratch,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error,
        SourceResolutionError::Transport(SimpleClientError::OfflineMissing)
    );
    assert!(!scratch.exists());
    assert!(files.requests().is_empty());
    assert_eq!(index.requests().len(), 1);
    assert_eq!(*credentials.calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn observation_limits_reserve_both_spools_before_filesystem_or_http() {
    let files = Peer::new(|_| Reply::raw(b"abc".to_vec()));
    let index = index_page(page(
        &format!("{}/body", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let selected = project.select_exact_file(FILENAME).unwrap();
    let valid = SourceObservationLimits {
        transfer: SourceFileLimits {
            max_wire_bytes: 3,
            ..Default::default()
        },
        max_spool_bytes: 6,
        ..Default::default()
    };
    let cases = [
        (
            SourceObservationLimits {
                transfer: SourceFileLimits {
                    max_wire_bytes: 0,
                    ..valid.transfer
                },
                ..valid
            },
            SourceResolutionError::InvalidLimits,
        ),
        (
            SourceObservationLimits {
                max_spool_bytes: 0,
                ..valid
            },
            SourceResolutionError::InvalidLimits,
        ),
        (
            SourceObservationLimits {
                max_identity_bytes: 0,
                ..valid
            },
            SourceResolutionError::InvalidLimits,
        ),
        (
            SourceObservationLimits {
                max_spool_bytes: 5,
                ..valid
            },
            SourceResolutionError::SpoolBudget,
        ),
        (
            SourceObservationLimits {
                transfer: SourceFileLimits {
                    max_wire_bytes: u64::MAX,
                    ..valid.transfer
                },
                max_spool_bytes: u64::MAX,
                ..valid
            },
            SourceResolutionError::SpoolBudget,
        ),
        (
            SourceObservationLimits {
                max_identity_bytes: 1,
                ..valid
            },
            SourceResolutionError::IdentityBudget,
        ),
    ];
    for (limits, expected) in cases {
        let scratch = fixture.root.path().join("preflight-must-not-exist");
        let result = acquire_registry_file(
            &selected,
            &fixture.client,
            fixture.file_session(&NoCredentials, &IDENTITY),
            NetworkPolicy::Allowed,
            limits,
            &scratch,
        )
        .await;
        assert_eq!(result.unwrap_err(), expected);
        assert!(!scratch.exists());
        assert!(files.requests().is_empty());
    }
    let source = observed(
        acquire_registry_file(
            &selected,
            &fixture.client,
            fixture.file_session(&NoCredentials, &IDENTITY),
            NetworkPolicy::Allowed,
            valid,
            &fixture.root.path().join("scratch"),
        )
        .await,
    );
    assert_eq!(source.size(), 3);
    assert_eq!(files.requests().len(), 1);
}

#[tokio::test]
async fn failed_file_acquisition_publishes_no_owner_lock_reference_or_environment() {
    let files = Peer::new(|_| Reply {
        status: 206,
        content_type: None,
        body: b"abc".to_vec(),
    });
    let index = index_page(page(
        &format!("{}/partial", files.url),
        serde_json::json!({}),
        Some(3),
    ));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    for path in ["pakro.lock", "refs", "environment"] {
        std::fs::write(fixture.root.path().join(path), b"old").unwrap();
    }
    let error = fixture.acquire(&project).await.unwrap_err();
    assert_eq!(
        error,
        SourceResolutionError::Transport(SimpleClientError::HttpStatus(206))
    );
    for path in ["pakro.lock", "refs", "environment"] {
        assert_eq!(
            std::fs::read(fixture.root.path().join(path)).unwrap(),
            b"old"
        );
    }
    assert!(!fixture.root.path().join("artifacts").exists());
    assert_eq!(files.requests().len(), 1);
    if fixture.root.path().join("scratch").exists() {
        assert!(
            !fixture
                .root
                .path()
                .join("scratch")
                .read_dir()
                .unwrap()
                .any(|_| true)
        );
    }
}

#[tokio::test]
async fn renewed_signed_locators_keep_stable_file_facts_and_distinct_local_observations() {
    let files = Peer::new(|count| {
        Reply::raw(if count == 0 {
            b"old".to_vec()
        } else {
            b"new".to_vec()
        })
    });
    let first = format!("{}/fetch?ticket=synthetic-first-secret", files.url);
    let next = format!("{}/fetch?ticket=synthetic-renewed-secret", files.url);
    let index = Peer::new(move |count| Reply {
        status: 200,
        content_type: Some(MEDIA),
        body: page(
            if count == 0 { &first } else { &next },
            serde_json::json!({"unknown-future-hash":"a0"}),
            Some(3),
        ),
    });
    let fixture = Fixture::new(&index);
    let a = fixture.fetch().await;
    let mut old = observed(fixture.acquire(&a).await);
    let b = fixture.fetch().await;
    let mut new = observed(fixture.acquire(&b).await);
    assert_eq!(old.registry_id(), new.registry_id());
    assert_eq!(old.filename(), new.filename());
    assert_eq!(old.version(), new.version());
    assert_ne!(old.source_key(), new.source_key());
    assert_ne!(old.observed_sha256(), new.observed_sha256());
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"old");
    bytes.clear();
    new.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"new");
    assert!(!format!("{old:?} {new:?} {a:?} {b:?}").contains("synthetic-"));
    assert!(
        files.requests()[0]
            .target
            .contains("synthetic-first-secret")
    );
    assert!(
        files.requests()[1]
            .target
            .contains("synthetic-renewed-secret")
    );
    assert_eq!(index.requests().len(), 2);
    assert_eq!(files.requests().len(), 2);
}
