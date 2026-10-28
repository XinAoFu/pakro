//! Real HTTP, genuine root lock and actual A/M1 bytes precede each depot target.
//! The constructor baseline intentionally blocks later targets until stage one.

use super::*;
#[path = "tests/active_policy.rs"]
mod active_policy;
#[path = "tests/build_fixture.rs"]
pub(super) mod build_fixture;
#[path = "tests/caller_deadline.rs"]
mod caller_deadline;
#[path = "tests/closed.rs"]
mod closed;
#[path = "tests/diagnostics.rs"]
mod diagnostics;
#[path = "tests/preferences.rs"]
mod preferences;
#[path = "tests/preflight.rs"]
mod preflight;
#[path = "tests/project_fixture.rs"]
pub(super) mod project_fixture;
#[path = "tests/source_build.rs"]
mod source_build;
use crate::packages::python_resolution::candidate_policy::{Prereleases, Sdists, Yanked};
use crate::packages::python_wheel::{WheelExpectation, inspect_wheel};
use base64::Engine;
use pakro_index::WheelTag;
use pakro_index::python_source::{
    AuthScopeKey, CredentialRef, PythonRegistryId, TransportRulesFingerprint,
    transport_rules_fingerprint,
};
use pakro_index::simple::{
    AuthOriginGrants, HttpOrigin, OriginAuthorization, SourceCredentials, SourceRequestRouter,
};
use std::fs;
use std::io::{Cursor, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

const DEMO: &str = "demo_pkg-1.0-py3-none-any.whl";
const DEP: &str = "dep-1.0-py3-none-any.whl";

struct NoCredentials(AtomicUsize);
impl SourceCredentials for NoCredentials {
    fn authorization_for(
        &self,
        _: &CredentialRef,
        _: &HttpOrigin,
        _: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(SimpleClientError::Authentication)
    }
}
struct Route;
impl SourceRequestRouter for Route {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
        Ok(canonical.clone())
    }
}
const ROUTE: Route = Route;

struct Reply {
    status: u16,
    media: Option<&'static str>,
    body: Vec<u8>,
}
impl Reply {
    fn file(bytes: &[u8]) -> Self {
        Self {
            status: 200,
            media: None,
            body: bytes.to_vec(),
        }
    }
    fn page(name: &str, files: Vec<serde_json::Value>) -> Self {
        Self {
            status: 200,
            media: Some("application/vnd.pypi.simple.v1+json"),
            body: serde_json::to_vec(
                &serde_json::json!({"meta":{"api-version":"1.0"},"name":name,"files":files}),
            )
            .unwrap(),
        }
    }
    fn missing() -> Self {
        Self {
            status: 404,
            media: None,
            body: Vec::new(),
        }
    }
}
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Peer {
    fn new(handler: impl Fn(&str) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                let mut raw = Vec::new();
                while !raw.ends_with(b"\r\n\r\n") && raw.len() < 64 * 1024 {
                    let mut byte = [0];
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        break;
                    }
                    raw.push(byte[0]);
                }
                if !raw.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let Ok(raw) = std::str::from_utf8(&raw) else {
                    continue;
                };
                let Some(target) = raw.lines().next().and_then(|s| s.split_whitespace().nth(1))
                else {
                    continue;
                };
                seen.lock().unwrap().push(target.to_owned());
                let reply = handler(target);
                let _ = (|| -> io::Result<()> {
                    write!(
                        stream,
                        "HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n",
                        reply.status,
                        reply.body.len()
                    )?;
                    if let Some(media) = reply.media {
                        write!(stream, "Content-Type: {media}\r\n")?;
                    }
                    stream.write_all(b"\r\n")?;
                    stream.write_all(&reply.body)?;
                    stream.flush()
                })();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn wire_file(filename: &str) -> serde_json::Value {
    serde_json::json!({"filename":filename,"url":format!("/files/{filename}"),"hashes":{},"requires-python":">=3"})
}
fn wheel(name: &str, version: &str, declarations: &[&str], other: &str) -> Vec<u8> {
    wheel_with_build(name, version, declarations, other, None)
}
fn wheel_with_build(
    name: &str,
    version: &str,
    declarations: &[&str],
    other: &str,
    build: Option<&str>,
) -> Vec<u8> {
    let info = format!("{name}-{version}.dist-info");
    let mut metadata = format!("Metadata-Version: 2.6\nName: {name}\nVersion: {version}\n{other}");
    for declaration in declarations {
        metadata.push_str(&format!("Requires-Dist: {declaration}\n"));
    }
    let mut wheel_headers =
        "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_owned();
    if let Some(build) = build {
        wheel_headers.push_str(&format!("Build: {build}\n"));
    }
    let mut entries = vec![
        (format!("{name}/__init__.py"), b"VALUE=42\n".to_vec()),
        (format!("{info}/WHEEL"), wheel_headers.into_bytes()),
        (format!("{info}/METADATA"), metadata.into_bytes()),
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
    let bytes = zip.finish().unwrap().into_inner();
    // Independent real A parser proves each wheel fixture is structurally valid.
    let tags = tags();
    let build = build.map(|build| format!("-{build}")).unwrap_or_default();
    let filename = format!("{name}-{version}{build}-py3-none-any.whl");
    inspect_wheel(
        Cursor::new(&bytes),
        &WheelExpectation {
            filename: &filename,
            name,
            version,
            supported: &tags,
        },
        Default::default(),
    )
    .unwrap();
    bytes
}
fn tags() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap()
}
fn environment() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "linux",
        platform_release: "6",
        platform_system: "Linux",
        platform_version: "fixture",
        platform_machine: "aarch64",
        platform_python_implementation: "CPython",
        python_version: "3.13",
        python_full_version: "3.13.5",
        implementation_name: "cpython",
        implementation_version: "3.13.5",
    }
}
struct Fixture {
    root: tempfile::TempDir,
    client: SimpleIndexClient,
    registry: PythonRegistry,
    scope: AuthScopeKey,
    credentials: NoCredentials,
    grants: AuthOriginGrants,
    supported: SupportedWheelTags,
    cache: std::path::PathBuf,
    scratch: std::path::PathBuf,
}
impl Fixture {
    fn new(peer: &Peer) -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            client: SimpleIndexClient::new(root.path().join("simple"), Default::default()).unwrap(),
            registry: PythonRegistry::new("corp", &format!("{}/simple/", peer.url), None).unwrap(),
            scope: AuthScopeKey::from_local_revision([7; 32]),
            credentials: NoCredentials(AtomicUsize::new(0)),
            grants: AuthOriginGrants::default(),
            supported: tags(),
            cache: root.path().join("cache"),
            scratch: root.path().join("scratch"),
            root,
        }
    }
    fn endpoint(&self) -> RegistryEndpoint<'_> {
        RegistryEndpoint {
            project: PythonSourceSession {
                registry: &self.registry,
                auth_scope: &self.scope,
                credentials: &self.credentials,
                auth_origins: &self.grants,
                transport: &ROUTE,
            },
            file: SourceFileSession {
                credential_ref: None,
                auth_scope: &self.scope,
                credentials: &self.credentials,
                auth_origins: &self.grants,
                transport: &ROUTE,
            },
        }
    }
    fn lock(&self) -> BoundPythonLock {
        BoundPythonLock::acquire(
            &self.root.path().join("pakro.lock"),
            Default::default(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
    }
    fn context<'a>(
        &'a self,
        endpoints: &'a [RegistryEndpoint<'a>],
        policy: &'a PythonSourcePolicy,
    ) -> RegistryDepotContext<'a> {
        RegistryDepotContext {
            client: &self.client,
            endpoints,
            source_policy: policy,
            candidate_policy: CandidatePolicy {
                prereleases: Prereleases::Allow,
                yanked: Yanked::Disallow,
                sdists: Sdists::Include,
                installed_version: None,
            },
            preferred_pins: Vec::new(),
            supported: &self.supported,
            environment: environment(),
            root_scope: RegistryRootScope::Environment,
            semantics: MarkerSemantics::Current2026,
            fetch: SimpleFetchPolicy {
                network: NetworkPolicy::Allowed,
                refresh: true,
                fresh_for: Duration::from_secs(60),
                now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            },
            limits: Default::default(),
            cache_root: &self.cache,
            scratch_root: &self.scratch,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    fn no_publish(&self) {
        assert!(!self.root.path().join("pakro.lock").exists());
        self.no_environment_publish();
    }
    fn no_publish_with_existing_lock(&self, expected: &[u8]) {
        assert_eq!(
            std::fs::read(self.root.path().join("pakro.lock")).unwrap(),
            expected
        );
        self.no_environment_publish();
    }
    fn no_environment_publish(&self) {
        for path in ["refs", "environment", "journal", "catalog"] {
            assert!(!self.root.path().join(path).exists());
        }
        assert_eq!(self.credentials.0.load(Ordering::SeqCst), 0);
    }
}
fn policy() -> PythonSourcePolicy {
    PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap())
}
fn ready<'root>(value: RegistryPreparation<'root, '_>) -> RegistryPreparedPlan<'root> {
    match value {
        RegistryPreparation::Ready(value) => value,
        other => panic!("expected a genuine prepared plan, got {other:?}"),
    }
}

#[tokio::test]
async fn demand_driven_real_metadata_preserves_original_roots_and_exact_owners() {
    let demo = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
    let dep = wheel("dep", "1.0", &[], "");
    let peer = Peer::new(move |target| match target {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
        "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
        value if value == format!("/files/{DEMO}") => Reply::file(&demo),
        value if value == format!("/files/{DEP}") => Reply::file(&dep),
        _ => Reply::missing(),
    });
    let fixture = Fixture::new(&peer);
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let result = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg==1.0", "ghost; python_version<'2'"],
            fixture.context(&endpoints, &policy),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    assert_eq!(result.roots().len(), 2);
    assert_eq!(result.plan().selected.len(), 2);
    assert_eq!(result.environment().python_full_version, "3.13.5");
    assert!(matches!(
        result.root_scope(),
        RegistryRootScope::Environment
    ));
    for selected in &result.plan().selected {
        let owner = result.owner(selected.key).unwrap();
        assert_eq!(owner.pin().file().project(), selected.name);
        owner.recheck_for(&root).unwrap();
    }
    assert_eq!(peer.requests().len(), 4);
    fixture.no_publish();
}

#[tokio::test]
async fn repeated_needinput_and_dependency_cycles_do_not_refetch_project_or_file() {
    let demo = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
    let dep = wheel("dep", "1.0", &["demo-pkg==1.0"], "");
    let peer = Peer::new(move |target| match target {
        "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
        "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
        value if value == format!("/files/{DEMO}") => Reply::file(&demo),
        value if value == format!("/files/{DEP}") => Reply::file(&dep),
        _ => Reply::missing(),
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(
            &root,
            &["demo-pkg", "demo-pkg>=1", "dep"],
            f.context(&endpoints, &p),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    assert_eq!(result.plan().selected.len(), 2);
    let requests = peer.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|p| p.as_str() == "/simple/demo-pkg/")
            .count(),
        1
    );
    f.no_publish();
}

#[tokio::test]
async fn firstproject_advances_only_on_actual_404_and_present_empty_stops() {
    for missing in [true, false] {
        let first = Peer::new(move |_| {
            if missing {
                Reply::missing()
            } else {
                Reply::page("demo-pkg", vec![])
            }
        });
        let demo = wheel("demo_pkg", "1.0", &[], "");
        let second = Peer::new(move |target| {
            if target == "/simple/demo-pkg/" {
                Reply::page("demo-pkg", vec![wire_file(DEMO)])
            } else {
                Reply::file(&demo)
            }
        });
        let f = Fixture::new(&first);
        let other = Fixture::new(&second);
        let root = f.lock();
        let mut second_endpoint = other.endpoint();
        let registry =
            PythonRegistry::new("public", &format!("{}/simple/", second.url), None).unwrap();
        second_endpoint.project.registry = &registry;
        let endpoints = [f.endpoint(), second_endpoint];
        let p =
            PythonSourcePolicy::FirstProject(vec![f.registry.id().clone(), registry.id().clone()]);
        let result = ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
            .unwrap()
            .prepare()
            .await;
        if missing {
            assert_eq!(ready(result.unwrap()).plan().selected.len(), 1);
            assert_eq!(second.requests().len(), 2);
        } else {
            assert!(matches!(result, Err(RegistryDepotError::NoSolution)));
            assert!(second.requests().is_empty());
        }
        assert_eq!(first.requests().len(), 1);
        f.no_publish();
    }
}

#[tokio::test]
async fn sameversion_distinct_files_actual_requires_python_backtracks_without_ghost_owner() {
    let first = "demo_pkg-1.0-py3-none-any.whl";
    // PEP 440-equivalent versions remain two independent archive identities.
    let second = "demo_pkg-1.0.0-py3-none-any.whl";
    let badbytes = wheel("demo_pkg", "1.0", &["ghost"], "Requires-Python: >=99\n");
    let goodbytes = wheel("demo_pkg", "1.0.0", &[], "");
    let peer = Peer::new(move |target| {
        if target == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(first), wire_file(second)])
        } else if target == format!("/files/{first}") {
            Reply::file(&badbytes)
        } else {
            Reply::file(&goodbytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(result.plan().selected.len(), 1);
    let selected = &result.plan().selected[0];
    assert_eq!(
        result.owner(selected.key).unwrap().pin().file().filename(),
        second
    );
    assert_eq!(result.owners.len(), 1);
    assert!(peer.requests().iter().all(|v| !v.contains("ghost")));
    f.no_publish();
}

#[tokio::test]
async fn prepared_metadata_cannot_be_reused_for_another_actual_project_lock() {
    let bytes = wheel("demo_pkg", "1.0", &[], "");
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    let other = tempfile::tempdir().unwrap();
    let other = BoundPythonLock::acquire(
        &other.path().join("pakro.lock"),
        Default::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(result.recheck_for(&other), Err(RegistryDepotError::Revoked));
    result.recheck_for(&root).unwrap();
    f.no_publish();
}

#[tokio::test]
async fn auth_scope_is_part_of_real_observation_not_a_name_version_cache_key() {
    let bytes = wheel("demo_pkg", "1.0", &[], "");
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let p = policy();
    let second_scope = AuthScopeKey::from_local_revision([8; 32]);
    for scope in [&f.scope, &second_scope] {
        let mut endpoint = f.endpoint();
        endpoint.project.auth_scope = scope;
        endpoint.file.auth_scope = scope;
        let endpoints = [endpoint];
        let mut context = f.context(&endpoints, &p);
        // Both calls are allowed to reuse a fresh observation. Only the
        // independently scoped cache identity requires this second HTTP page.
        context.fetch.refresh = false;
        let result = ready(
            ResolveRegistryDepot::new(&root, &["demo-pkg"], context)
                .unwrap()
                .prepare()
                .await
                .unwrap(),
        );
        assert_eq!(result.plan().selected.len(), 1);
    }
    assert_eq!(
        peer.requests()
            .iter()
            .filter(|p| p.as_str() == "/simple/demo-pkg/")
            .count(),
        2
    );
    f.no_publish();
}

#[tokio::test]
async fn active_direct_reference_is_a_typed_continuation_and_inactive_reference_stays_original() {
    let peer = Peer::new(|_| panic!("direct-reference continuation used a registry"));
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let raw = "demo-pkg @ https://private.invalid/file.whl?token=SECRET";
    let state = ResolveRegistryDepot::new(&root, &[raw], f.context(&endpoints, &p))
        .unwrap()
        .prepare()
        .await
        .unwrap();
    match state {
        RegistryPreparation::NeedsSourceAdapter(value) => {
            assert_eq!(value.declaration_for_source_binding(), raw);
            assert!(!format!("{:?}", value.depot).contains("SECRET"));
        }
        other => panic!("source reference became {other:?}"),
    }
    let result = ready(
        ResolveRegistryDepot::new(
            &root,
            &["ghost @ https://private.invalid/x ; python_version<'2'"],
            f.context(&endpoints, &p),
        )
        .unwrap()
        .prepare()
        .await
        .unwrap(),
    );
    assert_eq!(result.roots().len(), 1);
    assert!(result.plan().selected.is_empty());
    assert!(peer.requests().is_empty());
    f.no_publish();
}

#[tokio::test]
async fn sdist_keeps_actual_pin_as_build_continuation_and_has_no_fake_metadata() {
    const SDIST: &str = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz");
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(SDIST)])
        } else {
            Reply::file(bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let state = ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
        .unwrap()
        .prepare()
        .await
        .unwrap();
    let RegistryPreparation::NeedsBuild(value) = state else {
        panic!("sdist became {state:?}")
    };
    assert_eq!(value.pin().file().filename(), SDIST);
    assert_eq!(value.pin().size(), bytes.len() as u64);
    assert!(value.depot.metadata.is_empty());
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}

#[tokio::test]
async fn root_scope_and_preflight_budgets_are_checked_before_any_http() {
    let peer = Peer::new(|_| panic!("invalid root context performed HTTP"));
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let error = ResolveRegistryDepot::new(
        &root,
        &["demo-pkg; extra=='speed'"],
        f.context(&endpoints, &p),
    )
    .unwrap_err();
    assert_eq!(error, RegistryDepotError::RootMarker);
    let mut context = f.context(&endpoints, &p);
    context.limits.max_roots = 0;
    assert!(matches!(
        ResolveRegistryDepot::new(&root, &["demo-pkg"], context),
        Err(RegistryDepotError::InvalidLimits)
    ));
    let mut context = f.context(&endpoints, &p);
    context.deadline = Instant::now() - Duration::from_millis(1);
    assert!(matches!(
        ResolveRegistryDepot::new(&root, &["demo-pkg"], context),
        Err(RegistryDepotError::Deadline)
    ));
    assert!(peer.requests().is_empty());
    f.no_publish();
}

#[tokio::test]
async fn replaced_root_lock_cannot_publish_or_return_a_prepared_plan() {
    let bytes = wheel("demo_pkg", "1.0", &[], "");
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let depot = ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p)).unwrap();
    fs::write(f.root.path().join("pakro.lock"), "lockfile_version=4\n").unwrap();
    assert!(matches!(
        depot.prepare().await,
        Err(RegistryDepotError::Revoked)
    ));
    assert!(peer.requests().is_empty());
    assert_eq!(
        fs::read_to_string(f.root.path().join("pakro.lock")).unwrap(),
        "lockfile_version=4\n"
    );
}

#[tokio::test]
async fn dependency_source_continuation_uses_the_actual_original_metadata_index() {
    let raw = "dep @ https://private.invalid/wheel?ticket=source-secret";
    let bytes = wheel(
        "demo_pkg",
        "1.0",
        &["inactive; python_version<'2'", raw],
        "",
    );
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let state = ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
        .unwrap()
        .prepare()
        .await
        .unwrap();
    let RegistryPreparation::NeedsSourceAdapter(value) = state else {
        panic!("actual active source reference became {state:?}")
    };
    let RegistrySourceRequest::Dependency {
        parent,
        declaration,
    } = value.request()
    else {
        panic!("dependency source lost its parent")
    };
    assert_eq!(declaration, 1);
    assert_eq!(value.declaration_for_source_binding(), raw);
    let actual = &value.depot.metadata.get(&parent).unwrap().owner;
    assert_eq!(
        actual.facts().requirements()[declaration].raw_for_semantic_resolution(),
        raw
    );
    assert!(!format!("{:?}", value.depot).contains("source-secret"));
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}

#[tokio::test]
async fn inactive_metadata_sources_do_not_interrupt_the_registry_solve() {
    let bytes = wheel(
        "demo_pkg",
        "1.0",
        &["dep @ https://private.invalid/wheel?ticket=source-secret ; python_version<'2'"],
        "",
    );
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let result = ready(
        ResolveRegistryDepot::new(&root, &["demo-pkg"], f.context(&endpoints, &p))
            .unwrap()
            .prepare()
            .await
            .unwrap(),
    );
    assert_eq!(result.plan().selected.len(), 1);
    let selected = &result.plan().selected[0];
    assert_eq!(
        result
            .owner(selected.key)
            .unwrap()
            .facts()
            .requirements()
            .len(),
        1
    );
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}

#[tokio::test]
async fn sidecar_reserves_its_two_spools_in_addition_to_the_held_wheel() {
    let description = format!("\n{}", "description".repeat(4096));
    let raw = format!("Metadata-Version: 2.6\nName: demo_pkg\nVersion: 1.0\n{description}");
    let bytes = wheel("demo_pkg", "1.0", &[], &description);
    let size = bytes.len() as u64;
    assert!(
        (raw.len() as u64) * 2 > size,
        "fixture must cross the total 2N budget only after the wheel is held"
    );
    let digest = hex::encode(Sha256::digest(raw.as_bytes()));
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            let mut file = wire_file(DEMO);
            file["core-metadata"] = serde_json::json!({"sha256":digest});
            Reply::page("demo-pkg", vec![file])
        } else if p.ends_with(".metadata") {
            Reply::file(raw.as_bytes())
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let mut context = f.context(&endpoints, &p);
    context.limits.source.transfer.max_wire_bytes = size;
    context.limits.source.max_spool_bytes = size.checked_mul(2).unwrap();
    context.limits.max_retained_bytes = size.checked_mul(2).unwrap();
    let error = ResolveRegistryDepot::new(&root, &["demo-pkg"], context)
        .unwrap()
        .prepare()
        .await
        .unwrap_err();
    assert_eq!(error, RegistryDepotError::Limit);
    assert_eq!(
        peer.requests().len(),
        2,
        "resource reservation must reject before fetching the sidecar"
    );
    f.no_publish();
}

#[tokio::test]
async fn total_utf8_budget_includes_complete_actual_metadata_description() {
    let description = format!("\n{}", "retained-description".repeat(4096));
    let raw = format!("Metadata-Version: 2.6\nName: demo_pkg\nVersion: 1.0\n{description}");
    let bytes = wheel("demo_pkg", "1.0", &[], &description);
    let peer = Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    });
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let p = policy();
    let mut context = f.context(&endpoints, &p);
    context.limits.max_utf8_bytes = raw.len() - 1;
    assert!(context.limits.max_utf8_bytes > "demo-pkg".len());
    let error = ResolveRegistryDepot::new(&root, &["demo-pkg"], context)
        .unwrap()
        .prepare()
        .await
        .unwrap_err();
    assert_eq!(error, RegistryDepotError::Limit);
    assert_eq!(
        peer.requests().len(),
        2,
        "the exact actual A metadata is the late resource target"
    );
    f.no_publish();
}
#[path = "tests/scoped.rs"]
mod scoped;
