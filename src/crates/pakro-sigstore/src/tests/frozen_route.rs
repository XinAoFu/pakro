use super::*;
use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

const LOCAL_HTTP_CHILD: &str = "PAKRO_R06_LOCAL_HTTP_CHILD";
const LOOPBACK_NO_PROXY: &str = "127.0.0.1,localhost,::1";
const PROXY_ENVIRONMENT: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

fn configure_local_test(command: &mut std::process::Command, test: &str) {
    command
        .args(["--exact", test, "--nocapture"])
        .env(LOCAL_HTTP_CHILD, test)
        .env("NO_PROXY", LOOPBACK_NO_PROXY)
        .env("no_proxy", LOOPBACK_NO_PROXY);
    for variable in PROXY_ENVIRONMENT {
        command.env_remove(variable);
    }
}

fn finish_local_test(mut command: std::process::Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "isolated loopback test failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn run_in_local_test_process(test: &str) -> bool {
    let child = std::env::var_os(LOCAL_HTTP_CHILD);
    if child.as_deref() == Some(std::ffi::OsStr::new(test)) {
        return false;
    }
    // Cargo feature unification may enable reqwest's system proxy support.
    // The macOS proxy adapter does not import the system ExceptionsList, so
    // explicitly bypass proxies for fixture addresses in this child only.
    // Do not mutate the parent environment or production client policy.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    configure_local_test(&mut command, test);
    finish_local_test(command);
    true
}

#[tokio::test]
async fn r06_loopback_child_does_not_inherit_proxy_routing() {
    const TEST: &str = "tests::frozen_route::r06_loopback_child_does_not_inherit_proxy_routing";
    let child = std::env::var_os(LOCAL_HTTP_CHILD);
    if child.as_deref() == Some(std::ffi::OsStr::new(TEST)) {
        for variable in PROXY_ENVIRONMENT {
            assert!(
                std::env::var_os(variable).is_none(),
                "proxy setting survived: {variable}"
            );
        }
        for variable in ["NO_PROXY", "no_proxy"] {
            assert_eq!(std::env::var(variable).unwrap(), LOOPBACK_NO_PROXY);
        }
        let api = LocalRoute::new(200, r#"{"attestations":[]}"#.into());
        let client = AttestationClient::builder()
            .base_url(api.url())
            .retry_config(RetryConfig {
                timeout: Duration::from_secs(2),
                retries: 0,
                backoff_base: Duration::ZERO,
            })
            .build()
            .unwrap();
        let result = client
            .fetch_attestations(FetchParams {
                owner: "fixture".into(),
                repo: Some("fixture/runtime".into()),
                digest: "sha256:fixture".into(),
                limit: 1,
                predicate_type: None,
            })
            .await
            .expect("isolated fixture HTTP must bypass inherited and system proxies");
        assert!(result.is_empty());
        assert_eq!(
            api.hits(),
            1,
            "the actual loopback server must receive the request"
        );
        return;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    // Simulate a caller's explicit proxy entries as well as inherited ones.
    // The child executes the assertions above; no fake success is returned.
    for variable in PROXY_ENVIRONMENT {
        command.env(variable, "http://127.0.0.1:9");
    }
    command.env("NO_PROXY", "unrelated.invalid");
    command.env("no_proxy", "unrelated.invalid");
    configure_local_test(&mut command, TEST);
    finish_local_test(command);
}

pub(super) struct LocalRoute {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl LocalRoute {
    pub(super) fn new(status: u16, body: String) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let observed = requests.clone();
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("local route accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                let mut request = [0; 4096];
                let length = match stream.read(&mut request) {
                    Ok(0) | Err(_) => continue,
                    Ok(length) => length,
                };
                observed.lock().unwrap().push(
                    String::from_utf8_lossy(&request[..length])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .into(),
                );
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    pub(super) fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    pub(super) fn url(&self) -> &str {
        &self.url
    }
    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for LocalRoute {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let joined = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            joined.unwrap();
        }
    }
}

struct GlobalTufRestore(Option<String>);
impl Drop for GlobalTufRestore {
    fn drop(&mut self) {
        set_tuf_url(self.0.take());
    }
}

pub(super) struct RoutingArtifact(std::path::PathBuf);
impl RoutingArtifact {
    pub(super) fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pakro-r06-attestation-{}-{nonce}",
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(b"routing fixture artifact").unwrap();
        Self(path)
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for RoutingArtifact {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn r06_github_verification_uses_each_calls_tuf_route_not_global_override() {
    // Isolate both the deliberate TUF global change and fixture proxy policy.
    if run_in_local_test_process(
        "tests::frozen_route::r06_github_verification_uses_each_calls_tuf_route_not_global_override",
    ) {
        return;
    }
    let mut bundle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/github_build_provenance_jdx_pakro.json"
    ))
    .unwrap();
    let envelope: serde_json::Value = serde_json::from_str(GENUINE_INTOTO_ENVELOPE).unwrap();
    // Select the public Sigstore trust-root path with an existing public Fulcio
    // certificate. We intentionally serve no trusted metadata: this test proves
    // actual request routing through the verifier, never signature acceptance.
    let certificate = envelope["signatures"][0]["cert"]
        .as_str()
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    bundle["verificationMaterial"]["certificate"]["rawBytes"] = serde_json::json!(certificate);
    let parsed: Bundle = serde_json::from_value(bundle.clone()).unwrap();
    assert!(!is_github_internal_certificate(&parsed));
    let api = LocalRoute::new(
        200,
        serde_json::json!({"attestations":[{"bundle":bundle,"bundle_url":null}]}).to_string(),
    );
    let first = LocalRoute::new(404, "{}".into());
    let second = LocalRoute::new(404, "{}".into());
    let _restore = GlobalTufRestore(
        crate::certificate::TUF_URL_OVERRIDE
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
    );
    set_tuf_url(Some(second.url.clone()));
    let artifact = RoutingArtifact::new();
    let request = || GithubAttestationRequest {
        artifact_path: &artifact.0,
        owner: "fixture",
        repo: "runtime",
        token: None,
        signer_workflow: None,
        base_url: Some(&api.url),
        digest: None,
        retry_config: RetryConfig {
            timeout: Duration::from_secs(2),
            retries: 0,
            backoff_base: Duration::ZERO,
        },
    };
    let (one, two) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(
            verify_github_attestation_with_tuf_url(request(), Some(first.url.clone())),
            verify_github_attestation_with_tuf_url(request(), Some(second.url.clone())),
        )
    })
    .await
    .expect("local route verification must finish within its test budget");
    assert!(
        one.is_err() && two.is_err(),
        "untrusted fixture TUF metadata must never verify"
    );
    assert_eq!(
        api.hits(),
        2,
        "both requests must pass the real GitHub attestation fetch: first={one:?}, second={two:?}"
    );
    assert!(
        second.hits() > 0,
        "fixture must reach actual TUF metadata IO before testing isolation: first={one:?}, second={two:?}"
    );
    assert!(
        first.hits() > 0,
        "the first call was redirected to another call's global TUF route"
    );
}
