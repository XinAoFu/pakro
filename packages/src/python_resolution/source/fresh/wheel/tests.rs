//! Real Simple/file HTTP -> actual lock slot -> O2 -> existing A/C1 fixtures.
//! No test constructs a source, fresh pin or archive capability by fields.

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

use crate::packages::python_resolution::lock_session::{
    BoundPythonLock, FilePinLookup, ResolutionIntent,
};
use crate::packages::python_wheel::{WheelExpectation, inspect_wheel};
use crate::packages::python_wheel_source::inspect_owned_wheel;
use base64::Engine;
use pakro_index::python_metadata::{
    PythonMarkerSemantics, PythonMetadataField, PythonMetadataLimitKind, SdistDynamicPolicy,
};
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
    for path in ["scratch", "artifact-cache", "refs", "environment"] {
        let path = fixture.root.path().join(path);
        assert!(!path.exists() || path.read_dir().unwrap().next().is_none());
    }
}

#[tokio::test]
async fn missing_sidecar_uses_actual_wheel_metadata_and_retains_the_bound_source_facts() {
    let raw = metadata(
        "demo_pkg",
        "1.0",
        &["alpha>=1", "never; python_version < '1'"],
        "Requires-Python: >=3.8\n",
    );
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel?token=synthetic-source-secret", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let source = acquire(&lock, &fixture, &project, FILENAME).await;
    let expected_digest = source.digest();
    let expected_key = source.source_key();
    let candidate = candidate(route_fresh_metadata(
        source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.pin().digest(), expected_digest);
    assert_eq!(candidate.source_key(), expected_key);
    assert_eq!(candidate.manifest().name(), "demo-pkg");
    assert_eq!(candidate.facts().requires_python(), Some(">=3.8"));
    assert_eq!(
        candidate
            .facts()
            .requirements()
            .iter()
            .map(|item| item.name())
            .collect::<Vec<_>>(),
        vec!["alpha", "never"]
    );
    assert_eq!(candidate.file_facts().requires_python(), Some(">=99"));
    assert_eq!(candidate.file_facts().filename(), FILENAME);
    assert_eq!(
        candidate.file_facts().locator().url().query(),
        Some("token=synthetic-source-secret")
    );
    assert_eq!(candidate.manifest().core_metadata().raw(), raw.as_bytes());
    candidate.recheck().unwrap();
    let requests = files.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].target.starts_with("/wheel?"));
    assert!(!requests[0].headers.contains_key("authorization"));
    unchanged(&fixture);
}

#[tokio::test]
async fn false_and_weak_sidecars_never_replace_actual_wheel_metadata_or_add_requests() {
    for core in [
        serde_json::json!(false),
        serde_json::json!(true),
        serde_json::json!({"md5":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
    ] {
        let raw = metadata("demo_pkg", "1.0", &["actual-dependency[docs]>=2"], "");
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let files = Peer::new(move |_| Reply::raw(bytes.clone()));
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            Some(core.clone()),
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = acquire(&lock, &fixture, &project, FILENAME).await;
        let candidate = candidate(route_fresh_metadata(
            source,
            &tags(),
            Default::default(),
            Default::default(),
        ));
        assert_eq!(
            candidate.facts().requirements()[0].name(),
            "actual-dependency"
        );
        assert_eq!(
            candidate.facts().requirements()[0]
                .requested_extras()
                .collect::<Vec<_>>(),
            vec!["docs"]
        );
        assert_eq!(candidate.manifest().core_metadata().raw(), raw.as_bytes());
        if core == serde_json::json!(false) {
            assert!(matches!(
                candidate.file_facts().core_metadata(),
                pakro_index::simple::CoreMetadataAvailability::Absent
            ));
        } else {
            assert!(matches!(
                candidate.file_facts().core_metadata(),
                pakro_index::simple::CoreMetadataAvailability::Available { .. }
            ));
        }
        assert_eq!(files.requests().len(), 1);
        assert_eq!(files.requests()[0].target, "/wheel");
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn same_version_different_files_keep_distinct_actual_dependency_metadata() {
    let first_file = "demo_pkg-1.0-1-py3-none-any.whl";
    let second_file = "demo_pkg-1.0-2-py3-none-any.whl";
    let first = wheel(
        "demo_pkg",
        "1.0",
        &metadata("demo_pkg", "1.0", &["alpha==1"], ""),
        Some("1"),
        false,
    );
    let second = wheel(
        "demo_pkg",
        "1.0",
        &metadata("demo_pkg", "1.0", &["beta==2"], ""),
        Some("2"),
        false,
    );
    a_c1_prelude(&first, first_file, "demo-pkg", "1.0");
    a_c1_prelude(&second, second_file, "demo-pkg", "1.0");
    let first_size = first.len();
    let second_size = second.len();
    let files = Peer::new(move |index| {
        Reply::raw(if index == 0 {
            first.clone()
        } else {
            second.clone()
        })
    });
    let index = index_page(wire_page(vec![
        wire_file(
            first_file,
            &format!("{}/first", files.url),
            first_size,
            None,
        ),
        wire_file(
            second_file,
            &format!("{}/second", files.url),
            second_size,
            None,
        ),
    ]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let first_source = acquire(&lock, &fixture, &project, first_file).await;
    let first = candidate(route_fresh_metadata(
        first_source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    let second_source = acquire(&lock, &fixture, &project, second_file).await;
    let second = candidate(route_fresh_metadata(
        second_source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(first.facts().version(), second.facts().version());
    assert_ne!(first.pin().id(), second.pin().id());
    assert_eq!(first.facts().requirements()[0].name(), "alpha");
    assert_eq!(second.facts().requirements()[0].name(), "beta");
    assert_eq!(first.file_facts().filename(), first_file);
    assert_eq!(second.file_facts().filename(), second_file);
    assert_ne!(first.source_key(), second.source_key());
    assert_eq!(files.requests().len(), 2);
    unchanged(&fixture);
}

#[tokio::test]
async fn raw_markers_requested_and_provided_extras_and_metadata26_dynamic_are_not_filtered() {
    let dependencies = [
        "alpha[docs]>=1;extra == 'docs'",
        "literal; 'never' == 'always'",
        "version; python_version in '3.13'",
        "pair; python_full_version > implementation_version",
    ];
    let raw = metadata(
        "demo_pkg",
        "1.0",
        &dependencies,
        "Provides-Extra: docs\nProvides-Extra: fast\nDynamic: Requires-Dist\n",
    );
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let candidate = candidate(route_fresh_metadata(
        acquire(&lock, &fixture, &project, FILENAME).await,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.facts().requirements().len(), dependencies.len());
    for (declaration, raw) in candidate.facts().requirements().iter().zip(dependencies) {
        assert_eq!(declaration.raw_for_semantic_resolution(), raw);
        assert_eq!(declaration.exact_marker().unwrap().comparisons().count(), 1);
        assert_eq!(
            declaration.marker_semantics(),
            PythonMarkerSemantics::NeedsExactMarkerSemantics
        );
    }
    assert_eq!(candidate.facts().provided_extras(), ["docs", "fast"]);
    assert_eq!(candidate.facts().dynamic_fields(), ["requires-dist"]);
    assert_eq!(
        candidate.facts().sdist_dynamic_policy(),
        SdistDynamicPolicy::AppendMarkedMultipleUseFields
    );
    assert_eq!(
        candidate.facts().requirements()[0]
            .requested_extras()
            .collect::<Vec<_>>(),
        vec!["docs"]
    );
    unchanged(&fixture);
}

#[tokio::test]
async fn requires_python_uses_actual_metadata_without_raw_text_equality_or_host_selection() {
    let raw = metadata("demo_pkg", "1.0.0", &[], "Requires-Python: <4,>=3.8\n");
    let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let mut file = wire_file(FILENAME, &format!("{}/wheel", files.url), size, None);
    file["requires-python"] = serde_json::json!(">=3.8, <4");
    let index = index_page(wire_page(vec![file]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let candidate = candidate(route_fresh_metadata(
        acquire(&lock, &fixture, &project, FILENAME).await,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.facts().requires_python(), Some("<4,>=3.8"));
    assert_eq!(candidate.file_facts().requires_python(), Some(">=3.8, <4"));
    assert_eq!(candidate.manifest().version(), "1.0.0");
    assert_eq!(candidate.pin().file().version(), "1.0");
    unchanged(&fixture);
}

#[tokio::test]
async fn inner_wheel_distribution_identity_must_match_the_actual_source_pin() {
    for (name, version, valid_filename) in [
        ("other", "1.0", "other-1.0-py3-none-any.whl"),
        ("demo_pkg", "2.0", "demo_pkg-2.0-py3-none-any.whl"),
    ] {
        let bytes = wheel(
            name,
            version,
            &metadata(name, version, &[], ""),
            None,
            false,
        );
        a_c1_prelude(&bytes, valid_filename, name, version);
        let size = bytes.len();
        let files = Peer::new(move |_| Reply::raw(bytes.clone()));
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel", files.url),
            size,
            None,
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let source = acquire(&lock, &fixture, &project, FILENAME).await;
        assert!(matches!(
            route_fresh_metadata(source, &tags(), Default::default(), Default::default())
                .unwrap_err(),
            FreshWheelError::Archive(_)
        ));
        assert_eq!(files.requests().len(), 1);
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn explicit_supported_tags_are_checked_without_selecting_another_source_file() {
    let bytes = wheel(
        "demo_pkg",
        "1.0",
        &metadata("demo_pkg", "1.0", &[], ""),
        None,
        false,
    );
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let supported =
        SupportedWheelTags::new(vec![WheelTag::new("py2", "none", "any").unwrap()]).unwrap();
    assert!(matches!(
        route_fresh_metadata(
            acquire(&lock, &fixture, &project, FILENAME).await,
            &supported,
            Default::default(),
            Default::default()
        )
        .unwrap_err(),
        FreshWheelError::Archive(_)
    ));
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    unchanged(&fixture);
}

#[tokio::test]
async fn the_existing_full_archive_and_record_validation_remain_mandatory() {
    let raw = metadata("demo_pkg", "1.0", &[], "");
    let legal = wheel("demo_pkg", "1.0", &raw, None, false);
    a_c1_prelude(&legal, FILENAME, "demo-pkg", "1.0");
    let bytes = wheel("demo_pkg", "1.0", &raw, None, true);
    let mut raw_reader = Cursor::new(&bytes);
    let supported = tags();
    assert!(
        inspect_wheel(
            &mut raw_reader,
            &WheelExpectation {
                filename: FILENAME,
                name: "demo-pkg",
                version: "1.0",
                supported: &supported
            },
            Default::default()
        )
        .is_err()
    );
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    assert!(matches!(
        route_fresh_metadata(
            acquire(&lock, &fixture, &project, FILENAME).await,
            &tags(),
            Default::default(),
            Default::default()
        )
        .unwrap_err(),
        FreshWheelError::Archive(_)
    ));
    unchanged(&fixture);
}

#[tokio::test]
async fn archive_and_dependency_budgets_are_explicit_at_the_metadata_boundary() {
    let bytes = wheel(
        "demo_pkg",
        "1.0",
        &metadata("demo_pkg", "1.0", &["one", "two"], ""),
        None,
        false,
    );
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    for (wheel_limits, metadata_limits) in [
        (
            WheelLimits {
                max_members: 0,
                ..Default::default()
            },
            PythonMetadataLimits::default(),
        ),
        (
            WheelLimits::default(),
            PythonMetadataLimits {
                max_requirements: 0,
                ..Default::default()
            },
        ),
    ] {
        assert_eq!(
            route_fresh_metadata(
                acquire(&lock, &fixture, &project, FILENAME).await,
                &tags(),
                wheel_limits,
                metadata_limits
            )
            .unwrap_err(),
            FreshWheelError::InvalidLimits
        );
    }
    assert!(matches!(
        route_fresh_metadata(
            acquire(&lock, &fixture, &project, FILENAME).await,
            &tags(),
            WheelLimits {
                max_archive_bytes: size as u64 - 1,
                ..Default::default()
            },
            Default::default()
        )
        .unwrap_err(),
        FreshWheelError::Archive(_)
    ));
    assert_eq!(
        route_fresh_metadata(
            acquire(&lock, &fixture, &project, FILENAME).await,
            &tags(),
            Default::default(),
            PythonMetadataLimits {
                max_requirements: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        FreshWheelError::Metadata(
            pakro_index::python_metadata::PythonMetadataError::LimitExceeded(
                PythonMetadataLimitKind::Requirements
            )
        )
    );
    unchanged(&fixture);
}

#[tokio::test]
async fn a_lock_change_after_pin_closure_revokes_metadata_observation() {
    let bytes = wheel(
        "demo_pkg",
        "1.0",
        &metadata("demo_pkg", "1.0", &[], ""),
        None,
        false,
    );
    a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        FILENAME,
        &format!("{}/wheel", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let source = acquire(&lock, &fixture, &project, FILENAME).await;
    let path = fixture.root.path().join("pakro.lock");
    fs::write(&path, "lockfile_version=4\n").unwrap();
    assert!(source.recheck().is_err());
    assert_eq!(
        route_fresh_metadata(source, &tags(), Default::default(), Default::default()).unwrap_err(),
        FreshWheelError::Revoked
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "lockfile_version=4\n");
    assert_eq!(files.requests().len(), 1);
}

#[tokio::test]
async fn dependency_errors_and_candidate_debug_hide_raw_secrets_and_never_publish() {
    for invalid in [false, true] {
        let declaration = if invalid {
            "dep; unknown_marker == 'synthetic-marker-secret'"
        } else {
            "dep @ https://user:synthetic-source-secret@example.test/a;param?token=private ; os_name == 'synthetic-marker-secret'"
        };
        let raw = metadata("demo_pkg", "1.0", &[declaration], "");
        let bytes = wheel("demo_pkg", "1.0", &raw, None, false);
        a_c1_prelude(&bytes, FILENAME, "demo-pkg", "1.0");
        let size = bytes.len();
        let files = Peer::new(move |_| Reply::raw(bytes.clone()));
        let index = index_page(wire_page(vec![wire_file(
            FILENAME,
            &format!("{}/wheel?token=synthetic-file-secret", files.url),
            size,
            None,
        )]));
        let fixture = Fixture::new(&index);
        let project = fixture.fetch().await;
        let lock = lock(&fixture);
        let result = route_fresh_metadata(
            acquire(&lock, &fixture, &project, FILENAME).await,
            &tags(),
            Default::default(),
            Default::default(),
        );
        if invalid {
            let error = result.unwrap_err();
            assert_eq!(
                error,
                FreshWheelError::Metadata(
                    pakro_index::python_metadata::PythonMetadataError::InvalidField {
                        field: PythonMetadataField::RequiresDist,
                        index: Some(0)
                    }
                )
            );
            assert!(!format!("{error:?} {error}").contains("synthetic-"));
        } else {
            let candidate = candidate(result);
            assert_eq!(
                candidate.facts().requirements()[0].raw_for_semantic_resolution(),
                declaration
            );
            assert!(
                !format!(
                    "{candidate:?} {:?} {:?}",
                    candidate.facts(),
                    candidate.file_facts()
                )
                .contains("synthetic-")
            );
            candidate.recheck().unwrap();
        }
        unchanged(&fixture);
    }
}

#[tokio::test]
async fn a_legitimately_pinned_sdist_remains_the_same_explicit_build_continuation() {
    let filename = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("fixtures/demo_pkg-1.0.tar.gz").to_vec();
    let expected = bytes.clone();
    let digest = Digest::Sha256(Sha256::digest(&bytes).into());
    let size = bytes.len();
    let files = Peer::new(move |_| Reply::raw(bytes.clone()));
    let index = index_page(wire_page(vec![wire_file(
        filename,
        &format!("{}/source", files.url),
        size,
        None,
    )]));
    let fixture = Fixture::new(&index);
    let project = fixture.fetch().await;
    let lock = lock(&fixture);
    let source = acquire(&lock, &fixture, &project, filename).await;
    let key = source.source_key();
    let FreshMetadataState::NeedsBuild(mut source) =
        route_fresh_metadata(source, &tags(), Default::default(), Default::default()).unwrap()
    else {
        panic!("sdist was fabricated as a ready wheel")
    };
    assert_eq!(source.filename(), filename);
    assert_eq!(source.digest(), digest);
    assert_eq!(source.source_key(), key);
    assert_eq!(source.pin().file().kind(), RegistryArtifactKind::Sdist);
    source.recheck().unwrap();
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(files.requests().len(), 1);
    unchanged(&fixture);
}
