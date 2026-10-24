//! Genuine loopback file transfers after an actual Simple project lookup.
//! Success receipts are never fabricated from private struct fields.

use super::super::test_support::*;
use super::*;
use crate::python_source::{
    ProjectPresence, PythonSourcePolicy, bind_python_source, transport_rules_fingerprint,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn origin(value: &str) -> HttpOrigin {
    HttpOrigin::new(&Url::parse(value).unwrap()).unwrap()
}

struct Credentials {
    rows: Vec<(String, HttpOrigin, String)>,
    calls: Mutex<Vec<(String, HttpOrigin, NetworkPolicy)>>,
}
impl Credentials {
    fn new(rows: &[(&str, &str, &str)]) -> Self {
        Self {
            rows: rows
                .iter()
                .map(|(reference, url, secret)| {
                    ((*reference).to_owned(), origin(url), (*secret).to_owned())
                })
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<(String, HttpOrigin, NetworkPolicy)> {
        self.calls.lock().unwrap().clone()
    }
}
impl SourceCredentials for Credentials {
    fn authorization_for(
        &self,
        reference: &CredentialRef,
        requested: &HttpOrigin,
        policy: NetworkPolicy,
    ) -> SimpleClientResult<Option<OriginAuthorization>> {
        self.calls
            .lock()
            .unwrap()
            .push((reference.as_str().to_owned(), requested.clone(), policy));
        Ok(self
            .rows
            .iter()
            .find(|(name, target, _)| name == reference.as_str() && target == requested)
            .map(|(_, target, secret)| {
                OriginAuthorization::new(target.clone(), HeaderValue::from_str(secret).unwrap())
                    .unwrap()
            }))
    }
}

fn index(locator: &str, private: bool) -> (Server, Fixture) {
    let server = Server::new(Reply::page(
        Some(JSON_TYPE),
        serde_json::to_vec(&serde_json::json!({
            "meta":{"api-version":"1.0"},
            "name":"demo-pkg",
            "files":[{"filename":NAME,"url":locator,"hashes":{}}]
        }))
        .unwrap(),
    ));
    let mut fixture = Fixture::new(&server.url);
    if private {
        fixture.registry = PythonRegistry::new(
            "corp",
            &format!("{}/tenant/simple/", server.url),
            Some(CredentialRef::new("index-only-ref").unwrap()),
        )
        .unwrap();
    }
    (server, fixture)
}

async fn selected(
    fixture: &Fixture,
    credentials: &dyn SourceCredentials,
) -> super::super::super::SimpleProject {
    let project = present(
        fixture
            .client
            .project(
                PythonSourceSession {
                    registry: &fixture.registry,
                    auth_scope: &fixture.auth_scope,
                    credentials,
                    auth_origins: &fixture.grants,
                    transport: &IdentityRoute,
                },
                &fixture.name,
                SimpleFetchPolicy {
                    network: NetworkPolicy::Allowed,
                    fresh_for: Duration::from_secs(60),
                    now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                    refresh: true,
                },
                Default::default(),
            )
            .await,
    );
    let binding = bind_python_source(
        std::slice::from_ref(&fixture.registry),
        &PythonSourcePolicy::Explicit(fixture.registry.id().clone()),
        &fixture.name,
        |registry, name| {
            assert_eq!(registry.id(), project.source());
            assert_eq!(name, project.name());
            Ok(ProjectPresence::Present)
        },
    )
    .unwrap();
    assert_eq!(binding.registry().id(), project.source());
    assert_eq!(binding.project(), project.name());
    assert_eq!(project.files().len(), 1);
    assert_eq!(project.files()[0].filename(), NAME);
    assert!(project.files()[0].hashes().preferred_digest().is_none());
    project
}

fn session<'a>(
    fixture: &'a Fixture,
    reference: Option<&'a CredentialRef>,
    credentials: &'a dyn SourceCredentials,
    grants: &'a AuthOriginGrants,
    transport: &'a dyn SourceRequestRouter,
) -> SourceFileSession<'a> {
    SourceFileSession {
        credential_ref: reference,
        auth_scope: &fixture.auth_scope,
        credentials,
        auth_origins: grants,
        transport,
    }
}

fn authorization(request: &Request) -> Option<&str> {
    request.headers.get("authorization").map(String::as_str)
}

#[tokio::test]
#[ignore = "explicit public input and proxy diagnostic"]
async fn s4_registry_http_protocol_probe() {
    use std::io::{Seek, Write};

    const FILENAME: &str = "torch-2.14.1-cp311-cp311-macosx_14_0_arm64.whl";
    const RANGE_BYTES: u64 = 8 * 1024 * 1024;
    const MAX_PAGE_BYTES: u64 = 64 * 1024 * 1024;
    let page_path = std::env::var_os("PAKRO_S4_PROTOCOL_PAGE")
        .expect("set PAKRO_S4_PROTOCOL_PAGE to the captured public torch JSON");
    let page_size = std::fs::metadata(&page_path)
        .unwrap_or_else(|_| panic!("captured public page metadata is unavailable"))
        .len();
    assert!(
        page_size <= MAX_PAGE_BYTES,
        "captured public page is too large"
    );
    let page_bytes =
        std::fs::read(&page_path).unwrap_or_else(|_| panic!("captured public page is unreadable"));
    assert!(page_bytes.len() as u64 <= MAX_PAGE_BYTES);
    let page: serde_json::Value = serde_json::from_slice(&page_bytes)
        .unwrap_or_else(|_| panic!("captured public page is invalid JSON"));
    let files = page["files"]
        .as_array()
        .expect("captured public page must contain files");
    let mut matches = files
        .iter()
        .filter(|file| file["filename"].as_str() == Some(FILENAME));
    let file = matches.next().expect("fixed public wheel is absent");
    assert!(matches.next().is_none(), "fixed public wheel is duplicated");
    let locator = file["url"]
        .as_str()
        .expect("fixed public wheel has no locator");
    let url = Url::parse(locator).unwrap_or_else(|_| panic!("fixed public locator is invalid"));
    assert!(
        url.scheme() == "https"
            && url.host_str() == Some("files.pythonhosted.org")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path_segments().and_then(Iterator::last) == Some(FILENAME),
        "fixed public locator violates the diagnostic scope"
    );

    let cache = tempfile::tempdir().expect("create owned protocol diagnostic directory");
    let config = SimpleHttpConfig {
        connect_timeout: Duration::from_secs(45),
        read_timeout: Some(Duration::from_secs(120)),
        request_timeout: Duration::from_secs(45),
        max_redirects: 0,
        use_system_proxy: false,
        proxies: vec![
            reqwest::Proxy::all("socks5h://127.0.0.1:7892")
                .unwrap_or_else(|_| panic!("explicit diagnostic proxy is invalid")),
        ],
        root_certificates: vec![],
    };
    let simple = SimpleIndexClient::new(cache.path(), config.clone())
        .unwrap_or_else(|_| panic!("construct actual Simple client"))
        .client
        .clone();
    let controlled_builder = || {
        reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(reqwest::header::HeaderMap::new())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout.unwrap())
            .timeout(config.request_timeout)
            .http1_max_headers(1024)
            .no_proxy()
            .proxy(config.proxies[0].clone())
    };
    let http1 = controlled_builder()
        .http1_only()
        .build()
        .unwrap_or_else(|_| panic!("construct controlled HTTP/1 client"));
    let rustls_auto = controlled_builder()
        .tls_backend_rustls()
        .build()
        .unwrap_or_else(|_| panic!("construct controlled rustls automatic client"));
    let rustls_http1 = controlled_builder()
        .tls_backend_rustls()
        .http1_only()
        .build()
        .unwrap_or_else(|_| panic!("construct controlled rustls HTTP/1 client"));
    let mut output =
        tempfile::tempfile_in(cache.path()).expect("create owned protocol diagnostic output");
    let mut clients = [
        ("simple-default", simple),
        ("controlled-http1", http1),
        ("controlled-rustls-auto", rustls_auto),
        ("controlled-rustls-http1", rustls_http1),
    ];
    if std::env::var_os("PAKRO_S4_PROTOCOL_REVERSE").is_some() {
        clients.reverse();
    }
    for (label, client) in clients {
        output.set_len(0).expect("truncate diagnostic output");
        output.rewind().expect("rewind diagnostic output");
        let started = Instant::now();
        let deadline = started + Duration::from_secs(45);
        let mut response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "*/*")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, "bytes=0-8388607")
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .unwrap_or_else(|_| panic!("{label}: diagnostic request failed"));
        let headers_elapsed = started.elapsed();
        let version = response.version();
        assert_eq!(
            response.status().as_u16(),
            206,
            "{label}: expected partial response"
        );
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .expect("partial response must identify its range");
        let total_size = content_range
            .strip_prefix("bytes 0-8388607/")
            .and_then(|value| value.parse::<u64>().ok())
            .expect("partial response must match the requested fixed range");
        assert!(
            total_size > RANGE_BYTES,
            "range must be part of a larger wheel"
        );
        assert!(
            response
                .content_length()
                .is_none_or(|size| size == RANGE_BYTES)
        );
        http::identity_encoding(response.headers())
            .unwrap_or_else(|_| panic!("{label}: unexpected content encoding"));
        let mut first_body_elapsed = None;
        let bytes = http::copy_response_body(&mut response, deadline, RANGE_BYTES, |chunk| {
            first_body_elapsed.get_or_insert_with(|| started.elapsed());
            output.write_all(chunk).map_err(|_| SimpleClientError::Io)
        })
        .await
        .unwrap_or_else(|_| panic!("{label}: bounded diagnostic body copy failed"));
        output.flush().expect("flush diagnostic output");
        assert_eq!(bytes, RANGE_BYTES, "{label}: incomplete fixed range");
        assert_eq!(output.metadata().unwrap().len(), RANGE_BYTES);
        let ttfb = first_body_elapsed.expect("fixed range must include body bytes");
        println!(
            "S4_PROTOCOL probe={label} version={version:?} ttfb_ms={} headers_ms={} total_ms={} bytes={bytes}",
            ttfb.as_millis(),
            headers_elapsed.as_millis(),
            started.elapsed().as_millis()
        );
    }
}

#[tokio::test]
async fn anonymous_files_never_inherit_the_index_credential_reference() {
    let bytes = b"\x1f\x8b\x00raw-artifact\xff\x00".to_vec();
    let files = Server::new(Reply::page(None, bytes.clone()));
    let file_url = format!("{}/opaque?file=synthetic-file-token", files.url);
    let (index, fixture) = index(&file_url, true);
    let credentials = Credentials::new(&[(
        "index-only-ref",
        &index.url,
        "Bearer synthetic-index-secret",
    )]);
    let project = selected(&fixture, &credentials).await;
    assert_eq!(credentials.calls().len(), 1);
    let mut output = Vec::new();
    let transfer = fixture
        .client
        .copy_source_file(
            session(
                &fixture,
                None,
                &credentials,
                &fixture.grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        )
        .await
        .unwrap();
    assert_eq!(output, bytes);
    assert_eq!(transfer.size(), bytes.len() as u64);
    assert_eq!(transfer.initial_canonical_url().as_str(), file_url);
    assert_eq!(transfer.request_url().as_str(), file_url);
    assert_eq!(transfer.auth_scope(), &fixture.auth_scope);
    assert_eq!(
        transfer.transport_fingerprint(),
        IdentityRoute.fingerprint()
    );
    assert_eq!(credentials.calls().len(), 1);
    assert_eq!(
        authorization(&index.requests()[0]),
        Some("Bearer synthetic-index-secret")
    );
    assert_eq!(authorization(&files.requests()[0]), None);
    assert_eq!(
        files.requests()[0].headers.get("accept-encoding").unwrap(),
        "identity"
    );
    assert_ne!(
        files.requests()[0]
            .headers
            .get("accept")
            .map(String::as_str),
        Some(ACCEPT)
    );
    assert!(!format!("{transfer:?}").contains("synthetic-"));
    assert!(!fixture._cache.path().join("artifacts").exists());
}

#[tokio::test]
async fn explicit_file_credentials_are_reselected_for_each_exact_origin() {
    let target = Server::new(Reply::page(None, b"complete".to_vec()));
    let start = Server::new(
        Reply::page(None, Vec::new())
            .status(302)
            .header("Location", &format!("{}/next", target.url)),
    );
    let (_index, fixture) = index(&format!("{}/start", start.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    let reference = CredentialRef::new("independent-file-ref").unwrap();
    let credentials = Credentials::new(&[
        (
            "independent-file-ref",
            &start.url,
            "Bearer synthetic-first-secret",
        ),
        (
            "independent-file-ref",
            &target.url,
            "Bearer synthetic-second-secret",
        ),
    ]);
    let grants = AuthOriginGrants::with_additional(vec![origin(&target.url)]).unwrap();
    let mut output = Vec::new();
    let transfer = fixture
        .client
        .copy_source_file(
            session(
                &fixture,
                Some(&reference),
                &credentials,
                &grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        )
        .await
        .unwrap();
    assert_eq!(output, b"complete");
    assert_eq!(
        authorization(&start.requests()[0]),
        Some("Bearer synthetic-first-secret")
    );
    assert_eq!(
        authorization(&target.requests()[0]),
        Some("Bearer synthetic-second-secret")
    );
    assert_eq!(
        credentials.calls(),
        vec![
            (
                "independent-file-ref".into(),
                origin(&start.url),
                NetworkPolicy::Allowed
            ),
            (
                "independent-file-ref".into(),
                origin(&target.url),
                NetworkPolicy::Allowed
            ),
        ]
    );
    assert_eq!(
        transfer.canonical_url().as_str(),
        format!("{}/next", target.url)
    );
    assert!(!format!("{transfer:?} {reference:?}").contains("synthetic-"));
}

#[tokio::test]
async fn an_ungranted_redirect_origin_is_rejected_before_credential_lookup() {
    let target = Server::new(Reply::page(
        None,
        b"must not receive old authorization".to_vec(),
    ));
    let start = Server::new(
        Reply::page(None, Vec::new())
            .status(302)
            .header("Location", &format!("{}/next", target.url)),
    );
    let (_index, fixture) = index(&format!("{}/start", start.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    let reference = CredentialRef::new("file-ref").unwrap();
    let credentials = Credentials::new(&[
        ("file-ref", &start.url, "Bearer synthetic-old-secret"),
        ("file-ref", &target.url, "Bearer synthetic-ungranted-secret"),
    ]);
    let mut output = Vec::new();
    let error = fixture
        .client
        .copy_source_file(
            session(
                &fixture,
                Some(&reference),
                &credentials,
                &fixture.grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        )
        .await
        .unwrap_err();
    assert_eq!(error, SimpleClientError::Authentication);
    assert_eq!(start.requests().len(), 1);
    assert!(target.requests().is_empty());
    assert!(output.is_empty());
    assert_eq!(
        credentials.calls(),
        vec![(
            "file-ref".into(),
            origin(&start.url),
            NetworkPolicy::Allowed
        )]
    );
    assert!(!format!("{error:?} {error}").contains("synthetic-"));
}

struct Remap {
    canonical: Url,
    physical: Url,
}
impl SourceRequestRouter for Remap {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(Some(&[("synthetic-canonical", "synthetic-physical")]))
    }
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
        if canonical.origin() != self.canonical.origin() {
            return Err(SimpleClientError::InvalidLocator);
        }
        let mut output = self.physical.clone();
        output.set_path(canonical.path());
        output.set_query(canonical.query());
        Ok(output)
    }
}

#[tokio::test]
async fn file_routes_preserve_tls_secret_downgrade_guards() {
    for (query, authenticated) in [(false, false), (true, false), (false, true)] {
        let files = Server::new(Reply::page(None, b"raw".to_vec()));
        let file_url = if query {
            "https://files.example/opaque?token=synthetic-downgrade-secret"
        } else {
            "https://files.example/opaque"
        };
        let (_index, fixture) = index(file_url, false);
        let project = selected(&fixture, &NoCredentials).await;
        let router = Remap {
            canonical: Url::parse("https://files.example/").unwrap(),
            physical: Url::parse(&files.url).unwrap(),
        };
        let reference = CredentialRef::new("file-ref").unwrap();
        let credentials =
            Credentials::new(&[("file-ref", &files.url, "Bearer synthetic-file-secret")]);
        let grants = AuthOriginGrants::with_additional(vec![origin(&files.url)]).unwrap();
        let mut output = Vec::new();
        let result = fixture
            .client
            .copy_source_file(
                session(
                    &fixture,
                    authenticated.then_some(&reference),
                    &credentials,
                    &grants,
                    &router,
                ),
                project.files()[0].locator().url(),
                NetworkPolicy::Allowed,
                Default::default(),
                &mut output,
            )
            .await;
        if !query && !authenticated {
            let receipt = result.unwrap();
            assert_eq!(output, b"raw");
            assert_eq!(receipt.initial_canonical_url().as_str(), file_url);
            assert_eq!(files.requests().len(), 1);
            assert!(credentials.calls().is_empty());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error, SimpleClientError::InvalidLocator);
            assert!(files.requests().is_empty());
            assert!(output.is_empty());
            if query {
                assert!(credentials.calls().is_empty());
            }
            assert!(!format!("{error:?} {error}").contains("synthetic-"));
        }
    }
}

#[tokio::test]
async fn file_hops_and_body_share_one_deadline_and_redirect_budget() {
    let target = Server::new({
        let mut reply = Reply::page(None, b"late body".to_vec());
        reply.body_delay = Duration::from_millis(650);
        reply
    });
    let start = Server::new({
        let mut reply = Reply::page(None, Vec::new())
            .status(302)
            .header("Location", &format!("{}/next", target.url));
        reply.header_delay = Duration::from_millis(650);
        reply
    });
    let (_index, mut fixture) = index(&format!("{}/start", start.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    fixture.client.config.request_timeout = Duration::from_secs(1);
    let mut output = Vec::new();
    let begun = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.client.copy_source_file(
            session(
                &fixture,
                None,
                &NoCredentials,
                &fixture.grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        ),
    )
    .await
    .expect("file request ignored its shared deadline");
    assert_eq!(result.unwrap_err(), SimpleClientError::Timeout);
    assert!(begun.elapsed() < Duration::from_secs(3));
    assert_eq!(start.requests().len(), 1);
    assert_eq!(target.requests().len(), 1);
    assert!(output.is_empty());

    let loopback = Server::handler(|request, _| {
        let next = if request.target == "/first" {
            "/second"
        } else {
            "/third"
        };
        Reply::page(None, Vec::new())
            .status(302)
            .header("Location", next)
    });
    let (_index, mut fixture) = index(&format!("{}/first", loopback.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    fixture.client.config.max_redirects = 1;
    let error = fixture
        .client
        .copy_source_file(
            session(
                &fixture,
                None,
                &NoCredentials,
                &fixture.grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SimpleClientError::LimitExceeded(SimpleClientLimitKind::Redirects)
    );
    assert_eq!(loopback.requests().len(), 2);
}

struct SharedSink(Arc<Mutex<Vec<u8>>>);
impl Write for SharedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn chunked_file_bodies_stream_before_eof_and_never_write_overflow() {
    for overflow in [false, true] {
        let gate = Gate::new();
        let _release_on_panic = ReleaseGate(gate.clone());
        let mut reply = Reply::page(None, b"abc".to_vec());
        reply.declared_size = None;
        reply.chunked = true;
        reply.gate_after_body = Some(gate.clone());
        if overflow {
            reply.suffix = b"d".to_vec();
        }
        let files = Server::new(reply);
        let (_index, fixture) = index(&format!("{}/body", files.url), false);
        let project = selected(&fixture, &NoCredentials).await;
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink_state = Arc::clone(&written);
        let task = tokio::spawn(async move {
            let mut sink = SharedSink(sink_state);
            fixture
                .client
                .copy_source_file(
                    session(
                        &fixture,
                        None,
                        &NoCredentials,
                        &fixture.grants,
                        &IdentityRoute,
                    ),
                    project.files()[0].locator().url(),
                    NetworkPolicy::Allowed,
                    SourceFileLimits {
                        max_wire_bytes: 3,
                        ..Default::default()
                    },
                    &mut sink,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !task.is_finished() && written.lock().unwrap().len() < 3 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("file body was collected instead of streamed into the sink");
        let before_eof = written.lock().unwrap().clone();
        let completed_before_eof = task.is_finished();
        gate.release();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            before_eof, b"abc",
            "the sink was not written before the peer supplied EOF"
        );
        assert!(
            !completed_before_eof,
            "exact byte limit was mistaken for EOF"
        );
        assert_eq!(*written.lock().unwrap(), b"abc");
        if overflow {
            assert_eq!(
                result.unwrap_err(),
                SimpleClientError::LimitExceeded(SimpleClientLimitKind::WireBytes)
            );
        } else {
            assert_eq!(result.unwrap().size(), 3);
        }
    }
}

struct PanicSink;
impl Write for PanicSink {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("preflight failure wrote bytes")
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("preflight failure flushed a sink")
    }
}
struct PanicRouter;
impl SourceRequestRouter for PanicRouter {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        panic!("offline entered routing")
    }
    fn request_url(&self, _: &Url) -> SimpleClientResult<Url> {
        panic!("offline entered routing")
    }
}

#[tokio::test]
async fn offline_and_invalid_file_limits_precede_provider_sink_and_network() {
    let files = Server::new(Reply::page(None, b"must not fetch".to_vec()));
    let (_index, fixture) = index(&format!("{}/body", files.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    let reference = CredentialRef::new("file-ref").unwrap();
    let variants = [
        SourceFileLimits {
            max_wire_bytes: 0,
            ..Default::default()
        },
        SourceFileLimits {
            max_header_bytes: 0,
            ..Default::default()
        },
        SourceFileLimits {
            max_headers: 0,
            ..Default::default()
        },
    ];
    for limits in variants {
        assert_eq!(
            fixture
                .client
                .copy_source_file(
                    session(
                        &fixture,
                        Some(&reference),
                        &NoCredentials,
                        &fixture.grants,
                        &PanicRouter
                    ),
                    project.files()[0].locator().url(),
                    NetworkPolicy::Allowed,
                    limits,
                    &mut PanicSink,
                )
                .await
                .unwrap_err(),
            SimpleClientError::InvalidLimits
        );
    }
    assert_eq!(
        fixture
            .client
            .copy_source_file(
                session(
                    &fixture,
                    Some(&reference),
                    &NoCredentials,
                    &fixture.grants,
                    &PanicRouter
                ),
                project.files()[0].locator().url(),
                NetworkPolicy::Offline,
                Default::default(),
                &mut PanicSink,
            )
            .await
            .unwrap_err(),
        SimpleClientError::OfflineMissing
    );
    assert!(files.requests().is_empty());
}

struct FailingSink;
impl Write for FailingSink {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(
            "https://user:synthetic-sink-secret@example.test/private",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn opaque_file_locators_keep_the_checked_canonical_route_and_safe_failures() {
    let files = Server::handler(|request, _| {
        if request.target.starts_with("/opaque?") {
            Reply::page(None, Vec::new())
                .status(302)
                .header("Location", "/renewed?id=synthetic-file-secret")
        } else {
            Reply::page(None, b"held file bytes".to_vec())
        }
    });
    let locator = "http://files.example/opaque?id=synthetic-file-secret";
    let (_index, fixture) = index(locator, false);
    let project = selected(&fixture, &NoCredentials).await;
    let router = Remap {
        canonical: Url::parse("http://files.example/").unwrap(),
        physical: Url::parse(&files.url).unwrap(),
    };
    let mut output = Vec::new();
    let receipt = fixture
        .client
        .copy_source_file(
            session(&fixture, None, &NoCredentials, &fixture.grants, &router),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        )
        .await
        .unwrap();
    assert_eq!(output, b"held file bytes");
    assert_eq!(receipt.initial_canonical_url().as_str(), locator);
    assert_eq!(
        receipt.initial_request_url().as_str(),
        format!("{}/opaque?id=synthetic-file-secret", files.url)
    );
    assert_eq!(
        receipt.canonical_url().as_str(),
        "http://files.example/renewed?id=synthetic-file-secret"
    );
    assert_eq!(
        receipt.request_url().as_str(),
        format!("{}/renewed?id=synthetic-file-secret", files.url)
    );
    assert!(!format!("{receipt:?}").contains("synthetic-"));
    assert_eq!(files.requests().len(), 2);

    let error = fixture
        .client
        .copy_source_file(
            session(&fixture, None, &NoCredentials, &fixture.grants, &router),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut FailingSink,
        )
        .await
        .unwrap_err();
    assert_eq!(error, SimpleClientError::Io);
    assert!(!format!("{error:?} {error}").contains("synthetic-"));

    let partial = Server::new(
        Reply::page(None, b"partial".to_vec())
            .status(206)
            .header("Content-Range", "bytes 0-6/99"),
    );
    let (_index, fixture) = index(&format!("{}/partial", partial.url), false);
    let project = selected(&fixture, &NoCredentials).await;
    let mut output = Vec::new();
    let error = fixture
        .client
        .copy_source_file(
            session(
                &fixture,
                None,
                &NoCredentials,
                &fixture.grants,
                &IdentityRoute,
            ),
            project.files()[0].locator().url(),
            NetworkPolicy::Allowed,
            Default::default(),
            &mut output,
        )
        .await
        .unwrap_err();
    assert_eq!(error, SimpleClientError::HttpStatus(206));
    assert!(output.is_empty());
}
