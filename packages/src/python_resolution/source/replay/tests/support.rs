use super::*;
use pakro_index::python_source::{
    AuthScopeKey, CredentialRef, TransportRulesFingerprint, transport_rules_fingerprint,
};
use pakro_index::simple::{
    AuthOriginGrants, HttpOrigin, OriginAuthorization, SourceCredentials, SourceRequestRouter,
};
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

pub(super) const FILENAME: &str = "demo_pkg-1.0-1-py3-none-any.whl";
pub(super) const OTHER: &str = "demo_pkg-1.0-2-py3-none-any.whl";
pub(super) const PAYLOAD: &[u8] = b"locked exact artifact bytes";

#[derive(Default)]
pub(super) struct Credentials(AtomicUsize);
impl SourceCredentials for Credentials {
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
impl Credentials {
    pub(super) fn calls(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}
pub(super) struct Route;
impl SourceRequestRouter for Route {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
        Ok(canonical.clone())
    }
}
pub(super) const ROUTE: Route = Route;
pub(super) struct NoRoute;
impl SourceRequestRouter for NoRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        panic!("cache-first replay consulted a transport fingerprint")
    }
    fn request_url(&self, _: &Url) -> Result<Url, SimpleClientError> {
        panic!("cache-first replay consulted a locator/router")
    }
}
pub(super) const NO_ROUTE: NoRoute = NoRoute;

pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) media: Option<&'static str>,
    pub(super) body: Vec<u8>,
}
impl Reply {
    pub(super) fn file(bytes: &[u8]) -> Self {
        Self {
            status: 200,
            media: None,
            body: bytes.to_vec(),
        }
    }
    pub(super) fn page(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            media: Some("application/vnd.pypi.simple.v1+json"),
            body,
        }
    }
    pub(super) fn denied() -> Self {
        Self {
            status: 503,
            media: None,
            body: Vec::new(),
        }
    }
}
pub(super) struct Peer {
    pub(super) url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Peer {
    pub(super) fn new(handler: impl Fn(&str) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
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
                // Darwin's accepted socket inherits listener O_NONBLOCK.
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
                let Some(target) = raw
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                else {
                    continue;
                };
                observed.lock().unwrap().push(target.to_owned());
                let reply = handler(target);
                // Cancellation may close this real peer; a broken pipe is expected.
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
    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

pub(super) fn file_json(
    filename: &str,
    locator: &str,
    hashes: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({"filename":filename,"url":locator,"hashes":hashes,"requires-python":">=99","yanked":true})
}
pub(super) fn page(files: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"meta":{"api-version":"1.0"},"name":"demo-pkg","files":files}),
    )
    .unwrap()
}
pub(super) fn index(body: Vec<u8>) -> Peer {
    Peer::new(move |_| Reply::page(body.clone()))
}

pub(super) struct Fixture {
    pub(super) root: tempfile::TempDir,
    pub(super) cache: std::path::PathBuf,
    pub(super) scratch: std::path::PathBuf,
    pub(super) lock_path: std::path::PathBuf,
    pub(super) registry: PythonRegistry,
    client: SimpleIndexClient,
    credentials: Credentials,
    scope: AuthScopeKey,
    grants: AuthOriginGrants,
}
impl Fixture {
    pub(super) fn new(peer: &Peer) -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            cache: root.path().join("artifacts-cache"),
            scratch: root.path().join("private-scratch"),
            lock_path: root.path().join("pakro.lock"),
            registry: PythonRegistry::new("corp", &format!("{}/simple/", peer.url), None).unwrap(),
            client: SimpleIndexClient::new(root.path().join("simple-cache"), Default::default())
                .unwrap(),
            credentials: Credentials::default(),
            scope: AuthScopeKey::from_local_revision([7; 32]),
            grants: AuthOriginGrants::default(),
            root,
        }
    }
    pub(super) fn file(&self, filename: &str) -> RegistryFileRef {
        RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: self.registry.id().as_str(),
                canonical_base_fingerprint: canonical_registry_base_fingerprint(
                    self.registry.canonical_base(),
                ),
                project: "demo-pkg",
                version: "1.0.0",
                filename,
                kind: if filename.ends_with(".whl") {
                    RegistryArtifactKind::Wheel
                } else {
                    RegistryArtifactKind::Sdist
                },
            },
            Default::default(),
        )
        .unwrap()
    }
    pub(super) fn lock(
        &self,
        file: &RegistryFileRef,
        digest: Digest,
        size: u64,
    ) -> BoundPythonLock {
        let limits = LockModelLimits::default();
        let pin = ContentPin::new(file, digest, size, PinEvidence::Advertised, limits).unwrap();
        let key = GroupKey::new("", "main", [3; 32], limits).unwrap();
        let roots = [RootInput {
            declaration_index: 0,
            node: "demo-pkg",
        }];
        let nodes = [NodeInput {
            name: "demo-pkg",
            version: "1.0",
            source_pin: pin.id(),
        }];
        let graph = LockGraphInput {
            key: &key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: RuntimePin::new([4; 32], 1, [3; 32]).unwrap(),
            root_declarations: &["demo-pkg==1.0"],
            roots: &roots,
            nodes: &nodes,
            edges: &[],
        };
        let model = PythonLockModel::from_parts(&[pin], &[graph], limits).unwrap();
        fs::write(
            &self.lock_path,
            format!(
                "lockfile_version = 4\n{}",
                model.encode_projection(limits).unwrap()
            ),
        )
        .unwrap();
        BoundPythonLock::acquire(
            &self.lock_path,
            limits,
            Instant::now() + Duration::from_secs(15),
        )
        .unwrap()
    }
    pub(super) fn context<'ctx>(
        &'ctx self,
        network: NetworkPolicy,
        route: &'ctx dyn SourceRequestRouter,
    ) -> LockedReplayContext<'ctx> {
        LockedReplayContext {
            client: &self.client,
            project_session: PythonSourceSession {
                registry: &self.registry,
                auth_scope: &self.scope,
                credentials: &self.credentials,
                auth_origins: &self.grants,
                transport: route,
            },
            file_session: SourceFileSession {
                credential_ref: None,
                auth_scope: &self.scope,
                credentials: &self.credentials,
                auth_origins: &self.grants,
                transport: route,
            },
            policy: SimpleFetchPolicy {
                network,
                refresh: true,
                fresh_for: Duration::from_secs(60),
                now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            },
            project_limits: SimpleClientLimits::default(),
            source_limits: SourceObservationLimits {
                transfer: SourceFileLimits {
                    max_wire_bytes: 64 * 1024,
                    ..Default::default()
                },
                max_spool_bytes: 128 * 1024,
                ..Default::default()
            },
            lock_limits: LockModelLimits::default(),
            cache_root: &self.cache,
            scratch_root: &self.scratch,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    pub(super) fn seed_cache(
        &self,
        filename: &str,
        digest: Digest,
        bytes: &[u8],
    ) -> std::path::PathBuf {
        let path = self
            .cache
            .join("artifacts")
            .join(digest.to_string())
            .join(filename);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
    pub(super) fn assert_no_authority_writes(&self) {
        for name in ["refs", "environment", "journal", "catalog"] {
            assert!(!self.root.path().join(name).exists());
        }
        assert_eq!(self.credentials.calls(), 0);
    }
}

pub(super) fn slot<'lock>(
    lock: &'lock BoundPythonLock,
    file: &RegistryFileRef,
) -> ExistingFileSlot<'lock> {
    let FilePinLookup::Existing(slot) = lock.lookup(file, ResolutionIntent::Locked).unwrap() else {
        panic!("real lock did not prove an existing exact pin")
    };
    slot.recheck().unwrap();
    slot
}
pub(super) fn read(artifact: &mut LockedRegistryArtifact<'_>) -> Vec<u8> {
    artifact.rewind().unwrap();
    let mut bytes = Vec::new();
    artifact.read_to_end(&mut bytes).unwrap();
    bytes
}
