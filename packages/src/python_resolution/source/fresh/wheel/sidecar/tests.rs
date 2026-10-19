//! Actual A/C1 + Simple/T + scoped lock/O2 + C-basic owners precede every
//! sidecar target. No source, candidate, snapshot or lock cap is fabricated.

use super::*;
use pakro_index::python_source::{
    AuthScopeKey, CredentialRef, TransportRulesFingerprint, transport_rules_fingerprint,
};
use pakro_index::simple::{
    AuthOriginGrants, HttpOrigin, OriginAuthorization, SourceCredentials, SourceRequestRouter,
};
use reqwest::Url;
use sha2::Sha256;
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
    headers: Vec<(String, String)>,
    delay: Duration,
    chunked: bool,
}
impl Reply {
    fn raw(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: None,
            body: body.into(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            chunked: false,
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
                if !reply.delay.is_zero() {
                    thread::sleep(reply.delay);
                }
                let result = (|| -> io::Result<()> {
                    write!(
                        stream,
                        "HTTP/1.1 {} Fixture\r\nConnection: close\r\n",
                        reply.status
                    )?;
                    if reply.chunked {
                        write!(stream, "Transfer-Encoding: chunked\r\n")?;
                    } else {
                        write!(stream, "Content-Length: {}\r\n", reply.body.len())?;
                    }
                    if let Some(media) = reply.content_type {
                        write!(stream, "Content-Type: {media}\r\n")?;
                    }
                    for (name, value) in &reply.headers {
                        write!(stream, "{name}: {value}\r\n")?;
                    }
                    stream.write_all(b"\r\n")?;
                    if reply.chunked {
                        write!(stream, "{:x}\r\n", reply.body.len())?;
                        stream.write_all(&reply.body)?;
                        stream.write_all(b"\r\n0\r\n\r\n")?;
                    } else {
                        stream.write_all(&reply.body)?;
                    }
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

fn index_page(body: Vec<u8>) -> Peer {
    Peer::new(move |_| Reply {
        status: 200,
        content_type: Some(MEDIA),
        body: body.clone(),
        headers: Vec::new(),
        delay: Duration::ZERO,
        chunked: false,
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

use crate::packages::python_resolution::lock_session::{
    BoundPythonLock, FilePinLookup, ResolutionIntent,
};
use crate::packages::python_wheel::WheelExpectation;
use crate::packages::python_wheel_source::inspect_owned_wheel;
use base64::Engine;
use pakro_index::{SupportedWheelTags, WheelTag};
use std::fs;
use std::io::Cursor;
use std::time::Instant;

fn tags() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap()
}
fn metadata(name: &str, version: &str, dependencies: &[&str], extra_lines: &str) -> String {
    let mut raw = format!("Metadata-Version: 2.6\nName: {name}\nVersion: {version}\n{extra_lines}");
    for dependency in dependencies {
        raw.push_str(&format!("Requires-Dist: {dependency}\n"));
    }
    raw
}
fn wheel(
    name: &str,
    version: &str,
    raw_metadata: &str,
    build: Option<&str>,
    corrupt_record: bool,
) -> Vec<u8> {
    let info = format!("{name}-{version}.dist-info");
    let mut wheel = "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_owned();
    if let Some(build) = build {
        wheel.push_str(&format!("Build: {build}\n"));
    }
    let mut entries = vec![
        (format!("{name}/__init__.py"), b"VALUE=42\n".to_vec()),
        (format!("{info}/WHEEL"), wheel.into_bytes()),
        (format!("{info}/METADATA"), raw_metadata.as_bytes().to_vec()),
    ];
    let mut record = String::new();
    for (path, bytes) in &entries {
        record.push_str(&format!(
            "{path},sha256={},{}\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    if corrupt_record {
        entries[0].1[0] = b'X';
    }
    entries.push((format!("{info}/RECORD"), record.into_bytes()));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn a_c1_prelude(bytes: &[u8], filename: &str, name: &str, version: &str) -> WheelManifest {
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::Sha256(Sha256::digest(bytes).into());
    let artifact = spool_verified_reader(
        Cursor::new(bytes),
        filename,
        digest,
        bytes.len() as u64,
        root.path(),
    )
    .unwrap();
    let supported = tags();
    let expectation = WheelExpectation {
        filename,
        name,
        version,
        supported: &supported,
    };
    let archive = inspect_owned_wheel(artifact, digest, &expectation, Default::default())
        .expect("actual A/C1 prelude must pass before the new C target");
    archive.manifest().clone()
}
fn wire_file(
    filename: &str,
    locator: &str,
    size: usize,
    core: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut file = serde_json::json!({"filename":filename,"url":locator,"hashes":{},"size":size,"requires-python":">=99"});
    if let Some(core) = core {
        file["core-metadata"] = core;
    }
    file
}
fn wire_page(files: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"meta":{"api-version":"1.0"},"name":"demo-pkg","files":files}),
    )
    .unwrap()
}
fn lock(fixture: &Fixture) -> BoundPythonLock {
    BoundPythonLock::acquire(
        &fixture.root.path().join("pakro.lock"),
        Default::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap()
}
async fn acquire<'lock>(
    lock: &'lock BoundPythonLock,
    fixture: &Fixture,
    project: &BoundRegistryProject,
    filename: &str,
) -> FreshPinnedArtifact<'lock> {
    let selected = project.select_exact_file(filename).unwrap();
    let file = selected_file_ref(&selected, Default::default()).unwrap();
    let slot = match lock.lookup(&file, ResolutionIntent::Resolve).unwrap() {
        FilePinLookup::Fresh(slot) => slot,
        other => panic!("actual absent exact pin did not authorize fresh lookup: {other:?}"),
    };
    let source = acquire_fresh_registry_file(
        slot,
        selected,
        &fixture.client,
        fixture.file_session(&NoCredentials, &IDENTITY),
        NetworkPolicy::Allowed,
        Default::default(),
        Default::default(),
        &fixture.root.path().join("scratch"),
    )
    .await
    .expect("actual O2 prerequisite must be GREEN");
    source.recheck().unwrap();
    source
}
fn candidate(result: FreshWheelResult<FreshMetadataState<'_>>) -> FreshWheelCandidate<'_> {
    match result.unwrap() {
        FreshMetadataState::Wheel(wheel) => wheel,
        FreshMetadataState::NeedsBuild(_) => panic!("wheel was replaced by build work"),
    }
}
fn unchanged(fixture: &Fixture) {
    assert!(!fixture.root.path().join("pakro.lock").exists());
    for path in [
        "scratch",
        "sidecar-scratch",
        "artifact-cache",
        "refs",
        "environment",
    ] {
        let path = fixture.root.path().join(path);
        assert!(!path.exists() || path.read_dir().unwrap().next().is_none());
    }
}

fn sidecar_limits() -> SidecarLimits {
    SidecarLimits {
        transfer: SourceFileLimits {
            max_wire_bytes: 1024 * 1024,
            ..Default::default()
        },
        max_spool_bytes: 2 * 1024 * 1024,
        max_identity_bytes: 64 * 1024,
        deadline: Instant::now() + Duration::from_secs(5),
    }
}
async fn closed<'lock>(
    lock: &'lock BoundPythonLock,
    fixture: &Fixture,
    project: &BoundRegistryProject,
) -> FreshWheelCandidate<'lock> {
    candidate(route_fresh_metadata(
        acquire(lock, fixture, project, FILENAME).await,
        &tags(),
        Default::default(),
        Default::default(),
    ))
}
async fn checked<'lock>(
    wheel: FreshWheelCandidate<'lock>,
    fixture: &Fixture,
    network: NetworkPolicy,
    limits: SidecarLimits,
) -> FreshSidecarResult<CheckedFreshWheel<'lock>> {
    verify_advertised_core_metadata(
        wheel,
        &fixture.client,
        fixture.file_session(&NoCredentials, &IDENTITY),
        network,
        limits,
        &fixture.root.path().join("sidecar-scratch"),
    )
    .await
}
fn core_hash(raw: &str, sha512: bool) -> (Digest, serde_json::Value) {
    let (algorithm, digest) = if sha512 {
        (
            "sha512",
            Digest::Sha512(sha2::Sha512::digest(raw.as_bytes()).into()),
        )
    } else {
        (
            "sha256",
            Digest::Sha256(Sha256::digest(raw.as_bytes()).into()),
        )
    };
    (digest, serde_json::json!({(algorithm):digest.hex()}))
}
fn fixture_raw() -> String {
    metadata(
        "demo_pkg",
        "1.0",
        &["actual-dependency[docs]>=1"],
        "Requires-Python: >=3.8\n",
    )
}

#[tokio::test]
async fn both_advertised_strong_hashes_fetch_and_match_complete_actual_metadata() {
    for sha512 in [false, true] {
        let raw = fixture_raw();
        let (digest, hashes) = core_hash(&raw, sha512);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let sidecar = raw.as_bytes().to_vec();
        let files = Peer::new(move |index| {
            Reply::raw(if index == 0 {
                bytes.clone()
            } else {
                sidecar.clone()
            })
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let pin = source.pin().clone();
        let key = source.source_key();
        let checked = checked(source, &fixture, NetworkPolicy::Allowed, sidecar_limits())
            .await
            .unwrap();
        assert_eq!(
            checked.evidence(),
            CoreMetadataEvidence::FetchedSidecarMatchesWheel {
                digest,
                size: raw.len() as u64
            }
        );
        assert_eq!(checked.wheel().pin(), &pin);
        assert_eq!(checked.wheel().source_key(), key);
        assert_eq!(
            checked.wheel().facts().requirements()[0].name(),
            "actual-dependency"
        );
        assert_eq!(
            checked.wheel().manifest().core_metadata().raw(),
            raw.as_bytes()
        );
        checked.recheck().unwrap();
        let requests = files.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].target, "/wheel.metadata");
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn a_real_sidecar_hash_mismatch_is_fatal_and_never_falls_back_to_wheel_only() {
    for sha512 in [false, true] {
        let raw = fixture_raw();
        let (_, hashes) = core_hash(&raw, sha512);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let mut corrupt = raw.as_bytes().to_vec();
        corrupt[0] = b'X';
        let files = Peer::new(move |index| {
            Reply::raw(if index == 0 {
                bytes.clone()
            } else {
                corrupt.clone()
            })
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        assert_eq!(
            checked(source, &fixture, NetworkPolicy::Allowed, sidecar_limits())
                .await
                .unwrap_err(),
            FreshSidecarError::DigestMismatch
        );
        assert_eq!(files.requests().len(), 2);
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn valid_sidecar_hashes_cannot_hide_byte_differences_outside_dependency_fields() {
    let raw = fixture_raw();
    for sidecar in [
        raw.replace('\n', "\r\n"),
        format!("{raw}\nsynthetic-description-secret\n"),
    ] {
        let (_, hashes) = core_hash(&sidecar, false);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let sidecar = sidecar.into_bytes();
        let files = Peer::new(move |index| {
            Reply::raw(if index == 0 {
                bytes.clone()
            } else {
                sidecar.clone()
            })
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let error = checked(source, &fixture, NetworkPolicy::Allowed, sidecar_limits())
            .await
            .unwrap_err();
        assert_eq!(error, FreshSidecarError::MetadataMismatch);
        assert!(!format!("{error:?} {error}").contains("synthetic-"));
        assert_eq!(files.requests().len(), 2);
        unchanged(&fixture);
    }
}

struct Credentials {
    reference: &'static str,
    calls: Mutex<Vec<HttpOrigin>>,
}
impl SourceCredentials for Credentials {
    fn authorization_for(
        &self,
        reference: &CredentialRef,
        origin: &HttpOrigin,
        policy: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        assert_eq!(reference.as_str(), self.reference);
        assert_eq!(policy, NetworkPolicy::Allowed);
        let mut calls = self.calls.lock().unwrap();
        calls.push(origin.clone());
        let value = if calls.len() == 1 {
            "Bearer synthetic-first-secret"
        } else {
            "Bearer synthetic-second-secret"
        };
        Ok(Some(
            OriginAuthorization::new(
                origin.clone(),
                reqwest::header::HeaderValue::from_static(value),
            )
            .unwrap(),
        ))
    }
}

#[tokio::test]
async fn metadata_locator_keeps_opaque_signed_queries_without_inheriting_index_auth() {
    let raw = fixture_raw();
    let (_, hashes) = core_hash(&raw, false);
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let sidecar = raw.into_bytes();
    let files = Peer::new(move |index| {
        Reply::raw(if index == 0 {
            bytes.clone()
        } else {
            sidecar.clone()
        })
    });
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!(
            "{}/%64ownload;param?ticket=synthetic-signed-secret",
            files.url
        ),
        size,
        Some(hashes),
    )]));
    let mut fixture = Fixture::new(&index);
    fixture.registry = PythonRegistry::new(
        "corp",
        &format!("{}/simple/", index.url),
        Some(CredentialRef::new("index-ref").unwrap()),
    )
    .unwrap();
    let provider = Credentials {
        reference: "index-ref",
        calls: Mutex::new(Vec::new()),
    };
    let project = bound(
        fetch_registry_project(
            &fixture.client,
            fixture.source_session(&provider, &IDENTITY),
            &fixture.name,
            Fixture::policy(NetworkPolicy::Allowed, true),
            Default::default(),
        )
        .await,
    );
    let lock = lock(&fixture);
    let source = closed(&lock, &fixture, &project).await;
    let checked = checked(source, &fixture, NetworkPolicy::Allowed, sidecar_limits())
        .await
        .unwrap();
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    let requests = files.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].target,
        "/%64ownload;param.metadata?ticket=synthetic-signed-secret"
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.headers.contains_key("authorization"))
    );
    assert!(!format!("{checked:?}").contains("synthetic-"));
    unchanged(&fixture);
}

#[tokio::test]
async fn explicit_metadata_credentials_are_reauthorized_per_redirect_origin() {
    for allow_redirect in [false, true] {
        let raw = fixture_raw();
        let (_, hashes) = core_hash(&raw, false);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let sidecar = raw.into_bytes();
        let target = Peer::new(move |_| Reply::raw(sidecar.clone()));
        let location = format!("{}/metadata", target.url);
        let files = Peer::new(move |index| {
            if index == 0 {
                Reply::raw(bytes.clone())
            } else {
                Reply {
                    status: 302,
                    content_type: None,
                    body: Vec::new(),
                    headers: vec![("Location".into(), location.clone())],
                    delay: Duration::ZERO,
                    chunked: false,
                }
            }
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let reference = CredentialRef::new("metadata-ref").unwrap();
        let credentials = Credentials {
            reference: "metadata-ref",
            calls: Mutex::new(Vec::new()),
        };
        let grants = if allow_redirect {
            AuthOriginGrants::with_additional(vec![
                HttpOrigin::new(&Url::parse(&target.url).unwrap()).unwrap(),
            ])
            .unwrap()
        } else {
            AuthOriginGrants::default()
        };
        let result = verify_advertised_core_metadata(
            source,
            &fixture.client,
            SourceFileSession {
                credential_ref: Some(&reference),
                auth_scope: &fixture.scope,
                credentials: &credentials,
                auth_origins: &grants,
                transport: &IDENTITY,
            },
            NetworkPolicy::Allowed,
            sidecar_limits(),
            &fixture.root.path().join("sidecar-scratch"),
        )
        .await;
        if allow_redirect {
            let checked = result.unwrap();
            checked.recheck().unwrap();
            assert_eq!(credentials.calls.lock().unwrap().len(), 2);
            assert_eq!(target.requests().len(), 1);
            assert_eq!(
                target.requests()[0].headers.get("authorization").unwrap(),
                "Bearer synthetic-second-secret"
            );
        } else {
            assert_eq!(
                result.unwrap_err(),
                FreshSidecarError::Transport(SimpleClientError::Authentication)
            );
            assert_eq!(credentials.calls.lock().unwrap().len(), 1);
            assert_eq!(target.requests().len(), 0);
        }
        assert_eq!(files.requests().len(), 2);
        assert_eq!(
            files.requests()[1].headers.get("authorization").unwrap(),
            "Bearer synthetic-first-secret"
        );
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn limits_cover_whole_raw_bytes_spools_chunked_transfer_and_one_deadline() {
    let raw = fixture_raw();
    let (_, hashes) = core_hash(&raw, false);
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        Some(hashes),
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let initial_lock = lock(&fixture);
    // Each budget preflight consumes a genuine candidate, without contacting
    // the metadata endpoint. New artifact requests must still get wheel bytes.
    // Use independent peers below for the network-only matrix.
    let source = closed(&initial_lock, &fixture, &project).await;
    let invalid = SidecarLimits {
        max_spool_bytes: 0,
        ..sidecar_limits()
    };
    assert_eq!(
        checked(source, &fixture, NetworkPolicy::Allowed, invalid)
            .await
            .unwrap_err(),
        FreshSidecarError::InvalidLimits
    );
    assert_eq!(files.requests().len(), 1);
    unchanged(&fixture);
    drop(initial_lock);
    for case in ["whole-raw", "overflow", "chunked", "deadline", "expired"] {
        let raw = fixture_raw();
        let (_, hashes) = core_hash(&raw, false);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let raw_size = raw.len() as u64;
        let body = raw.into_bytes();
        let files = Peer::new(move |index| {
            if index == 0 {
                Reply::raw(bytes.clone())
            } else {
                let mut reply = Reply::raw(body.clone());
                if case == "chunked" {
                    reply.body.push(b'!');
                    reply.chunked = true;
                }
                if case == "deadline" {
                    reply.delay = Duration::from_millis(200);
                }
                reply
            }
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let mut limits = sidecar_limits();
        let expected = match case {
            "whole-raw" => {
                limits.transfer.max_wire_bytes = raw_size - 1;
                FreshSidecarError::LimitExceeded(SimpleClientLimitKind::WireBytes)
            }
            "overflow" => {
                limits.transfer.max_wire_bytes = u64::MAX;
                limits.max_spool_bytes = u64::MAX;
                FreshSidecarError::InvalidLimits
            }
            "chunked" => {
                limits.transfer.max_wire_bytes = raw_size;
                limits.max_spool_bytes = raw_size * 2;
                FreshSidecarError::Transport(SimpleClientError::LimitExceeded(
                    SimpleClientLimitKind::WireBytes,
                ))
            }
            "deadline" => {
                limits.deadline = Instant::now() + Duration::from_millis(60);
                FreshSidecarError::Deadline
            }
            "expired" => {
                limits.deadline = Instant::now() - Duration::from_millis(1);
                FreshSidecarError::Deadline
            }
            _ => unreachable!(),
        };
        assert_eq!(
            checked(source, &fixture, NetworkPolicy::Allowed, limits)
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(
            files.requests().len(),
            if matches!(case, "chunked" | "deadline") {
                2
            } else {
                1
            }
        );
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn actual_slot_revocation_before_and_during_sidecar_transfer_never_publishes() {
    for during in [false, true] {
        let raw = fixture_raw();
        let (_, hashes) = core_hash(&raw, false);
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let body = raw.into_bytes();
        let mutation = Arc::new(Mutex::new(None::<std::path::PathBuf>));
        let peer_mutation = Arc::clone(&mutation);
        let files = Peer::new(move |index| {
            if index == 0 {
                Reply::raw(bytes.clone())
            } else {
                if let Some(path) = peer_mutation.lock().unwrap().as_ref() {
                    fs::write(path, "lockfile_version=4\n").unwrap();
                }
                Reply::raw(body.clone())
            }
        });
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(hashes),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let path = fixture.root.path().join("pakro.lock");
        if during {
            *mutation.lock().unwrap() = Some(path.clone());
        } else {
            fs::write(&path, "lockfile_version=4\n").unwrap();
        }
        assert_eq!(
            checked(source, &fixture, NetworkPolicy::Allowed, sidecar_limits())
                .await
                .unwrap_err(),
            FreshSidecarError::Revoked
        );
        assert_eq!(files.requests().len(), if during { 2 } else { 1 });
        assert_eq!(fs::read_to_string(path).unwrap(), "lockfile_version=4\n");
        assert!(
            fixture
                .root
                .path()
                .join("scratch")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!fixture.root.path().join("refs").exists());
        assert!(!fixture.root.path().join("environment").exists());
    }
}

#[tokio::test]
async fn offline_known_raw_hashes_and_no_trustworthy_sidecars_require_no_http_or_provider() {
    for mode in ["sha256", "sha512", "mismatch", "missing", "false", "weak"] {
        let raw = fixture_raw();
        let (digest, hashes) = core_hash(&raw, mode == "sha512");
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let files = Peer::new(move |_| Reply::raw(bytes.clone()));
        let core = match mode {
            "missing" => None,
            "false" => Some(serde_json::json!(false)),
            "weak" => Some(serde_json::json!({"md5":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})),
            "mismatch" => Some(serde_json::json!({"sha256":"ab".repeat(32)})),
            _ => Some(hashes),
        };
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel?ticket=synthetic-signed-secret", files.url),
            size,
            core,
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = closed(&lock, &fixture, &project).await;
        let reference = CredentialRef::new("unused-metadata-ref").unwrap();
        let result = verify_advertised_core_metadata(
            source,
            &fixture.client,
            SourceFileSession {
                credential_ref: Some(&reference),
                auth_scope: &fixture.scope,
                credentials: &NoCredentials,
                auth_origins: &fixture.grants,
                transport: &IDENTITY,
            },
            NetworkPolicy::Offline,
            sidecar_limits(),
            &fixture.root.path().join("sidecar-scratch"),
        )
        .await;
        if mode == "mismatch" {
            assert_eq!(result.unwrap_err(), FreshSidecarError::DigestMismatch);
        } else {
            let checked = result.unwrap();
            assert_eq!(
                checked.evidence(),
                if matches!(mode, "sha256" | "sha512") {
                    CoreMetadataEvidence::WheelBytesMatchAdvertisedHash { digest }
                } else {
                    CoreMetadataEvidence::WholeWheel
                }
            );
            checked.recheck().unwrap();
            assert!(!format!("{checked:?}").contains("synthetic-"));
        }
        assert_eq!(files.requests().len(), 1);
        unchanged(&fixture);
    }
}

// Finite local routing work returns the exact same route, but exhausts the
// caller's common deadline on the T kernel's second forward lookup.
#[tokio::test]
async fn caller_deadline_is_checked_before_metadata_credentials_after_finite_routing_work() {
    use std::sync::atomic::AtomicUsize;
    struct FiniteRouteWork(AtomicUsize);
    impl SourceRequestRouter for FiniteRouteWork {
        fn fingerprint(&self) -> TransportRulesFingerprint {
            transport_rules_fingerprint(None)
        }
        fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
                thread::sleep(Duration::from_millis(120));
            }
            Ok(canonical.clone())
        }
    }
    struct DeadlineCredentials {
        deadline: Instant,
        calls: AtomicUsize,
        late: AtomicUsize,
    }
    impl SourceCredentials for DeadlineCredentials {
        fn authorization_for(
            &self,
            reference: &CredentialRef,
            origin: &HttpOrigin,
            policy: NetworkPolicy,
        ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
            assert_eq!(reference.as_str(), "metadata-ref");
            assert_eq!(policy, NetworkPolicy::Allowed);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if Instant::now() >= self.deadline {
                self.late.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Some(
                OriginAuthorization::new(
                    origin.clone(),
                    reqwest::header::HeaderValue::from_static("Bearer synthetic-private-value"),
                )
                .unwrap(),
            ))
        }
    }
    let raw = fixture_raw();
    let (_, hashes) = core_hash(&raw, false);
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let sidecar = raw.into_bytes();
    let files = Peer::new(move |index| {
        Reply::raw(if index == 0 {
            bytes.clone()
        } else {
            sidecar.clone()
        })
    });
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        Some(hashes),
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let wheel = closed(&lock, &fixture, &project).await;
    let mut limits = sidecar_limits();
    limits.deadline = Instant::now() + Duration::from_millis(60);
    let route = FiniteRouteWork(AtomicUsize::new(0));
    let credentials = DeadlineCredentials {
        deadline: limits.deadline,
        calls: AtomicUsize::new(0),
        late: AtomicUsize::new(0),
    };
    let reference = CredentialRef::new("metadata-ref").unwrap();
    let result = verify_advertised_core_metadata(
        wheel,
        &fixture.client,
        SourceFileSession {
            credential_ref: Some(&reference),
            auth_scope: &fixture.scope,
            credentials: &credentials,
            auth_origins: &fixture.grants,
            transport: &route,
        },
        NetworkPolicy::Allowed,
        limits,
        &fixture.root.path().join("sidecar-scratch"),
    )
    .await;
    assert_eq!(result.unwrap_err(), FreshSidecarError::Deadline);
    assert_eq!(
        route.0.load(Ordering::SeqCst),
        2,
        "target must reach actual T routing after the app preflight"
    );
    assert_eq!(
        credentials.late.load(Ordering::SeqCst),
        0,
        "provider ran after the caller's absolute deadline"
    );
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
    unchanged(&fixture);
}
