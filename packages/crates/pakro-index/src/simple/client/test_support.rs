use super::*;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::python_source::transport_rules_fingerprint;

pub(super) const NAME: &str = "demo_pkg-1.0-py3-none-any.whl";
pub(super) const JSON_TYPE: &str = "application/vnd.pypi.simple.v1+json";
pub(super) const HTML_TYPE: &str = "application/vnd.pypi.simple.v1+html";
pub(super) const ACCEPT: &str = "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.01";

pub(super) struct NoCredentials;
impl SourceCredentials for NoCredentials {
    fn authorization_for(
        &self,
        _reference: &CredentialRef,
        _origin: &HttpOrigin,
        _policy: NetworkPolicy,
    ) -> SimpleClientResult<Option<OriginAuthorization>> {
        panic!("anonymous HTTP/codec fixture consulted a credential provider")
    }
}
pub(super) struct IdentityRoute;
impl SourceRequestRouter for IdentityRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
        Ok(canonical.clone())
    }
}
pub(super) struct QueryRoute;
impl SourceRequestRouter for QueryRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(Some(&[("query-ref", "test-only-ref")]))
    }
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
        let mut request = canonical.clone();
        request.set_query(Some("token=synthetic-request-secret"));
        Ok(request)
    }
}

pub(super) struct Fixture {
    pub(super) client: SimpleIndexClient,
    pub(super) registry: PythonRegistry,
    pub(super) name: NormalizedPythonName,
    pub(super) auth_scope: AuthScopeKey,
    pub(super) grants: AuthOriginGrants,
    pub(super) _cache: tempfile::TempDir,
}
impl Fixture {
    /// A valid explicit transport shared by method-level tests, independent
    /// of the cases which specifically exercise the public constructor.
    pub(super) fn new(base: &str) -> Self {
        let cache = tempfile::tempdir().unwrap();
        let client = SimpleIndexClient {
            client: reqwest::Client::builder()
                .no_proxy()
                .no_gzip()
                .no_brotli()
                .no_zstd()
                .no_deflate()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            cache_root: cache.path().join("metadata"),
            config: SimpleHttpConfig::default(),
        };
        Self {
            client,
            registry: PythonRegistry::new("corp", &format!("{base}/tenant/simple/"), None).unwrap(),
            name: NormalizedPythonName::new("DeMo_Pkg").unwrap(),
            auth_scope: AuthScopeKey::from_local_revision([7; 32]),
            grants: AuthOriginGrants::default(),
            _cache: cache,
        }
    }
    pub(super) async fn fetch(
        &self,
        limits: SimpleClientLimits,
    ) -> SimpleClientResult<ProjectLookup> {
        self.with_router(&IdentityRoute, limits).await
    }
    pub(super) async fn with_router(
        &self,
        router: &dyn SourceRequestRouter,
        limits: SimpleClientLimits,
    ) -> SimpleClientResult<ProjectLookup> {
        self.client
            .project(
                PythonSourceSession {
                    registry: &self.registry,
                    auth_scope: &self.auth_scope,
                    credentials: &NoCredentials,
                    auth_origins: &self.grants,
                    transport: router,
                },
                &self.name,
                SimpleFetchPolicy {
                    network: NetworkPolicy::Allowed,
                    fresh_for: Duration::from_secs(60),
                    now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                    refresh: true,
                },
                limits,
            )
            .await
    }
}

pub(super) fn json_page(files: bool) -> Vec<u8> {
    let files = if files {
        serde_json::json!([{
            "filename":NAME,
            "url":format!("../../files/{NAME}#sha256={}","ab".repeat(32)),
            "hashes":{}
        }])
    } else {
        serde_json::json!([])
    };
    serde_json::to_vec(&serde_json::json!({
        "meta":{"api-version":"1.0"}, "name":"demo-pkg", "files":files
    }))
    .unwrap()
}
pub(super) fn html_page(head: &str, reason: &str) -> String {
    format!(
        "<!doctype html><html><head>{head}</head><body><a href='../../files/{NAME}#sha256={}' data-yanked='{reason}'>{NAME}</a></body></html>",
        "ab".repeat(32)
    )
}
pub(super) fn present(result: SimpleClientResult<ProjectLookup>) -> SimpleProject {
    match result.unwrap() {
        ProjectLookup::Present(project) => project,
        ProjectLookup::Missing404 => panic!("a successful project became a project 404"),
    }
}

#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) target: String,
    pub(super) headers: BTreeMap<String, String>,
}
#[derive(Clone)]
pub(super) struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    pub(super) fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }
    pub(super) fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
    fn wait(&self, stopping: &AtomicBool) -> bool {
        while !stopping.load(Ordering::Acquire) {
            let state = self.0.0.lock().unwrap();
            if *state {
                return true;
            }
            let (state, _) = self
                .0
                .1
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap();
            if *state {
                return true;
            }
        }
        false
    }
}
pub(super) struct ReleaseGate(pub(super) Gate);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[derive(Clone)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
    pub(super) declared_size: Option<usize>,
    pub(super) chunked: bool,
    pub(super) gate_after_body: Option<Gate>,
    pub(super) suffix: Vec<u8>,
    pub(super) header_delay: Duration,
    pub(super) body_delay: Duration,
}
impl Reply {
    pub(super) fn page(content_type: Option<&str>, body: impl Into<Vec<u8>>) -> Self {
        let body = body.into();
        Self {
            status: 200,
            headers: content_type
                .map(|value| vec![("Content-Type".into(), value.into())])
                .unwrap_or_default(),
            declared_size: Some(body.len()),
            body,
            chunked: false,
            gate_after_body: None,
            suffix: Vec::new(),
            header_delay: Duration::ZERO,
            body_delay: Duration::ZERO,
        }
    }
    pub(super) fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }
    pub(super) fn status(mut self, value: u16) -> Self {
        self.status = value;
        self
    }
}

/// A stoppable raw loopback peer: wrong-stub tests cannot leave an accept(),
/// slow-body, or gate worker blocked forever when no HTTP has occurred.
pub(super) struct Server {
    pub(super) url: String,
    pub(super) requests: Arc<Mutex<Vec<Request>>>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub(super) fn new(reply: Reply) -> Self {
        Self::handler(move |_, _| reply.clone())
    }
    pub(super) fn handler(handler: impl Fn(&Request, usize) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let records = Arc::clone(&requests);
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                // macOS inherits the listener's O_NONBLOCK flag on accept.
                // SO_RCVTIMEO alone does not turn that socket into a blocking
                // stream, so an early read can otherwise fail at zero elapsed.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
                    eprintln!(
                        "Simple fixture accepted port={} flags_ok={} nonblocking={}",
                        stream.local_addr().map(|value| value.port()).unwrap_or(0),
                        flags >= 0,
                        flags >= 0 && flags & libc::O_NONBLOCK != 0
                    );
                }
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let index = {
                    let mut records = records.lock().unwrap();
                    let index = records.len();
                    records.push(request.clone());
                    index
                };
                let reply = handler(&request, index);
                if let Err(error) = send_reply(&mut stream, reply, &stopping) {
                    eprintln!(
                        "Simple fixture reply index={index} port={} kind={:?} errno={:?}",
                        stream.local_addr().map(|value| value.port()).unwrap_or(0),
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
    pub(super) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let started = std::time::Instant::now();
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(1) if bytes.len() < 64 * 1024 => {}
            Ok(count) => {
                eprintln!(
                    "Simple fixture header ended port={} count={count} header_bytes={}",
                    stream.local_addr().map(|value| value.port()).unwrap_or(0),
                    bytes.len()
                );
                return None;
            }
            Err(error) => {
                eprintln!(
                    "Simple fixture header read port={} header_bytes={} elapsed_us={} kind={:?} errno={:?}",
                    stream.local_addr().map(|value| value.port()).unwrap_or(0),
                    bytes.len(),
                    started.elapsed().as_micros(),
                    error.kind(),
                    error.raw_os_error()
                );
                return None;
            }
        }
        bytes.push(byte[0]);
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut lines = text.split("\r\n");
    let target = lines.next()?.split_whitespace().nth(1)?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Some(Request { target, headers })
}
fn delay(duration: Duration, stopping: &AtomicBool) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        if stopping.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    !stopping.load(Ordering::Acquire)
}
fn send_reply(stream: &mut TcpStream, reply: Reply, stopping: &AtomicBool) -> std::io::Result<()> {
    if !delay(reply.header_delay, stopping) {
        return Ok(());
    }
    write!(
        stream,
        "HTTP/1.1 {} Fixture\r\nConnection: close\r\n",
        reply.status
    )?;
    for (key, value) in reply.headers {
        write!(stream, "{key}: {value}\r\n")?;
    }
    if reply.chunked {
        stream.write_all(b"Transfer-Encoding: chunked\r\n")?;
    } else if let Some(size) = reply.declared_size {
        write!(stream, "Content-Length: {size}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.flush()?;
    if !delay(reply.body_delay, stopping) {
        return Ok(());
    }
    if reply.chunked {
        write!(stream, "{:x}\r\n", reply.body.len())?;
        stream.write_all(&reply.body)?;
        stream.write_all(b"\r\n")?;
    } else {
        stream.write_all(&reply.body)?;
    }
    stream.flush()?;
    if let Some(gate) = reply.gate_after_body {
        if !gate.wait(stopping) {
            return Ok(());
        }
    }
    if !reply.suffix.is_empty() {
        if reply.chunked {
            write!(stream, "{:x}\r\n", reply.suffix.len())?;
            stream.write_all(&reply.suffix)?;
            stream.write_all(b"\r\n")?;
        } else {
            stream.write_all(&reply.suffix)?;
        }
    }
    if reply.chunked {
        stream.write_all(b"0\r\n\r\n")?;
    }
    stream.flush()
}
