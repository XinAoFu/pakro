use super::cache::{CacheRequest, CachedResponse};
use super::test_support::*;
use super::*;
use crate::python_source::{SimpleRepresentation, transport_rules_fingerprint};

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

fn policy(network: NetworkPolicy, seconds: u64) -> SimpleFetchPolicy {
    SimpleFetchPolicy {
        network,
        fresh_for: Duration::from_secs(60),
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        refresh: false,
    }
}
async fn fetch(
    fixture: &Fixture,
    router: &dyn SourceRequestRouter,
    credentials: &dyn SourceCredentials,
    policy: SimpleFetchPolicy,
    limits: SimpleClientLimits,
) -> SimpleClientResult<ProjectLookup> {
    fixture
        .client
        .project(
            PythonSourceSession {
                registry: &fixture.registry,
                auth_scope: &fixture.auth_scope,
                credentials,
                auth_origins: &fixture.grants,
                transport: router,
            },
            &fixture.name,
            policy,
            limits,
        )
        .await
}
fn page(reason: &str) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(&json_page(true)).unwrap();
    value["files"][0]["yanked"] = serde_json::json!(reason);
    serde_json::to_vec(&value).unwrap()
}
fn request<'a>(
    fixture: &'a Fixture,
    canonical: &'a Url,
    physical: &'a Url,
    router: &dyn SourceRequestRouter,
) -> CacheRequest<'a> {
    CacheRequest {
        registry: &fixture.registry,
        auth_scope: &fixture.auth_scope,
        transport: router.fingerprint(),
        project: &fixture.name,
        canonical,
        physical,
    }
}
fn key(
    fixture: &Fixture,
    router: &dyn SourceRequestRouter,
    representation: SimpleRepresentation,
) -> cache::EntryKey {
    let document = fixture.registry.project_url(&fixture.name).unwrap();
    let physical = router.request_url(document.as_url()).unwrap();
    request(fixture, document.as_url(), &physical, router).key(representation)
}
fn path(fixture: &Fixture, router: &dyn SourceRequestRouter) -> PathBuf {
    let document = fixture.registry.project_url(&fixture.name).unwrap();
    let physical = router.request_url(document.as_url()).unwrap();
    let slot = request(fixture, document.as_url(), &physical, router).slot_key();
    cache::path(&fixture.client.cache_root, slot)
}
fn entry(
    fixture: &Fixture,
    router: &dyn SourceRequestRouter,
    body: Vec<u8>,
    stored: u64,
    etag: Option<&str>,
) -> CachedResponse {
    let canonical = fixture
        .registry
        .project_url(&fixture.name)
        .unwrap()
        .as_url()
        .clone();
    let physical = router.request_url(&canonical).unwrap();
    CachedResponse {
        representation: SimpleRepresentation::JsonV1,
        content_type: JSON_TYPE.into(),
        canonical,
        physical,
        stored_at: SystemTime::UNIX_EPOCH + Duration::from_secs(stored),
        max_age: None,
        etag: etag.map(|value| HeaderValue::from_str(value).unwrap()),
        body,
    }
}
/// Independent valid wire fixture: no production cache encoder/reader is used.
fn encoded(key: cache::EntryKey, entry: &CachedResponse) -> Vec<u8> {
    let stored = entry
        .stored_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let representation = match entry.representation {
        SimpleRepresentation::JsonV1 => "json-v1",
        SimpleRepresentation::HtmlV1 => "html-v1",
        SimpleRepresentation::LegacyHtml => "legacy-html",
    };
    let header=serde_json::to_vec(&serde_json::json!({
        "schema":1,"entry_key":key.hex(),"representation":representation,
        "content_type":entry.content_type,"canonical_document":entry.canonical.as_str(),"request_document":entry.physical.as_str(),
        "stored_at_seconds":stored.as_secs(),"stored_at_nanos":stored.subsec_nanos(),
        "max_age_seconds":entry.max_age.map(|value|value.as_secs()),"etag":entry.etag.as_ref().map(|value|value.as_bytes()),
        "body_size":entry.body.len(),"body_sha256":pakro_store::Digest::from_sha256_bytes(&entry.body).hex()
    })).unwrap();
    let mut bytes = cache::MAGIC.to_vec();
    bytes.extend_from_slice(&(header.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&entry.body);
    bytes
}
fn seed(fixture: &Fixture, router: &dyn SourceRequestRouter, entry: &CachedResponse) -> PathBuf {
    let destination = path(fixture, router);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    #[cfg(unix)]
    for directory in [&fixture.client.cache_root, destination.parent().unwrap()] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        &destination,
        encoded(key(fixture, router, SimpleRepresentation::JsonV1), entry),
    )
    .unwrap();
    #[cfg(unix)]
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
    destination
}
fn raw_body(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(cache::MAGIC) {
        return None;
    }
    let start = cache::MAGIC.len();
    let len = u64::from_be_bytes(bytes.get(start..start + 8)?.try_into().ok()?) as usize;
    let header_end = (start + 8).checked_add(len)?;
    let header: serde_json::Value =
        serde_json::from_slice(bytes.get(start + 8..header_end)?).ok()?;
    let body = bytes.get(header_end..)?;
    if header["body_size"].as_u64()? != body.len() as u64
        || header["body_sha256"].as_str()? != pakro_store::Digest::from_sha256_bytes(body).hex()
    {
        return None;
    }
    Some(body)
}

#[tokio::test]
async fn fresh_same_source_cache_hits_without_a_second_http_request() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), page("observed")));
    let fixture = Fixture::new(&server.url);
    let online = policy(NetworkPolicy::Allowed, 1_700_000_000);
    assert_eq!(
        present(
            fetch(
                &fixture,
                &IdentityRoute,
                &NoCredentials,
                online,
                Default::default()
            )
            .await
        )
        .files()[0]
            .yanked()
            .reason(),
        Some("observed")
    );
    assert_eq!(
        server.requests().len(),
        1,
        "first observation must actually come from HTTP"
    );
    assert_eq!(
        present(
            fetch(
                &fixture,
                &IdentityRoute,
                &NoCredentials,
                online,
                Default::default()
            )
            .await
        )
        .files()[0]
            .yanked()
            .reason(),
        Some("observed")
    );
    assert_eq!(
        server.requests().len(),
        1,
        "fresh hit unexpectedly sent HTTP"
    );
}

struct RejectCredentials(AtomicUsize);
impl SourceCredentials for RejectCredentials {
    fn authorization_for(
        &self,
        _reference: &CredentialRef,
        _origin: &HttpOrigin,
        _policy: NetworkPolicy,
    ) -> SimpleClientResult<Option<OriginAuthorization>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(SimpleClientError::Authentication)
    }
}
#[tokio::test]
async fn offline_stale_cache_and_online_fresh_cache_precede_local_credentials() {
    for network in [NetworkPolicy::Offline, NetworkPolicy::Allowed] {
        let server = Server::new(Reply::page(Some(JSON_TYPE), page("must not request")));
        let mut fixture = Fixture::new(&server.url);
        fixture.registry = PythonRegistry::new(
            "corp",
            &format!("{}/tenant/simple/", server.url),
            Some(CredentialRef::new("private-ref").unwrap()),
        )
        .unwrap();
        seed(
            &fixture,
            &IdentityRoute,
            &entry(
                &fixture,
                &IdentityRoute,
                page("private cached"),
                1_700_000_000,
                None,
            ),
        );
        let credentials = RejectCredentials(AtomicUsize::new(0));
        let now = if network == NetworkPolicy::Offline {
            1_700_010_000
        } else {
            1_700_000_001
        };
        let project = present(
            fetch(
                &fixture,
                &IdentityRoute,
                &credentials,
                policy(network, now),
                Default::default(),
            )
            .await,
        );
        assert_eq!(project.files()[0].yanked().reason(), Some("private cached"));
        assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn offline_misses_are_explicit_and_never_refresh_credentials_or_send_http() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let mut fixture = Fixture::new(&server.url);
    fixture.registry = PythonRegistry::new(
        "corp",
        &format!("{}/tenant/simple/", server.url),
        Some(CredentialRef::new("private-ref").unwrap()),
    )
    .unwrap();
    let credentials = RejectCredentials(AtomicUsize::new(0));
    for refresh in [false, true] {
        let mut offline = policy(NetworkPolicy::Offline, 1_700_000_000);
        offline.refresh = refresh;
        let result = fetch(
            &fixture,
            &IdentityRoute,
            &credentials,
            offline,
            Default::default(),
        )
        .await;
        assert!(
            matches!(result, Err(SimpleClientError::OfflineMissing)),
            "{result:?}"
        );
    }
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
    assert!(server.requests().is_empty());
    assert!(!fixture.client.cache_root.exists());
}

struct Routed {
    base: Url,
    reverse_rules: bool,
}
impl SourceRequestRouter for Routed {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        if self.reverse_rules {
            transport_rules_fingerprint(Some(&[("second", "B"), ("first", "A")]))
        } else {
            transport_rules_fingerprint(Some(&[("first", "A"), ("second", "B")]))
        }
    }
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
        let mut request = canonical.clone();
        request.set_scheme(self.base.scheme()).unwrap();
        request.set_host(self.base.host_str()).unwrap();
        request.set_port(self.base.port()).unwrap();
        Ok(request)
    }
}
#[tokio::test]
async fn complete_source_route_and_representation_identities_partition_entries() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let fixture = Fixture::new(&server.url);
    let initial = key(&fixture, &IdentityRoute, SimpleRepresentation::JsonV1).hex();
    for dimension in 0..9 {
        let mut other = Fixture::new(&server.url);
        match dimension {
            0 => {
                other.registry = PythonRegistry::new(
                    "different",
                    &format!("{}/tenant/simple/", server.url),
                    None,
                )
                .unwrap()
            }
            1 => {
                other.registry =
                    PythonRegistry::new("corp", &format!("{}/another/simple/", server.url), None)
                        .unwrap()
            }
            2 => {
                other.registry = PythonRegistry::new(
                    "corp",
                    &format!(
                        "{}/tenant/simple/",
                        server.url.replacen("http:", "https:", 1)
                    ),
                    None,
                )
                .unwrap()
            }
            3 => {
                other.registry = PythonRegistry::new(
                    "corp",
                    &format!("{}/tenant/simple/", server.url),
                    Some(CredentialRef::new("other-ref").unwrap()),
                )
                .unwrap()
            }
            4 => other.auth_scope = AuthScopeKey::from_local_revision([8; 32]),
            5 => {
                let base = Url::parse(&server.url).unwrap();
                let left = Routed {
                    base: base.clone(),
                    reverse_rules: false,
                };
                let right = Routed {
                    base,
                    reverse_rules: true,
                };
                assert_ne!(
                    key(&other, &left, SimpleRepresentation::JsonV1).hex(),
                    key(&other, &right, SimpleRepresentation::JsonV1).hex()
                );
                continue;
            }
            6 => {
                assert_ne!(
                    initial,
                    key(&other, &IdentityRoute, SimpleRepresentation::HtmlV1).hex()
                );
                assert_ne!(
                    key(&other, &IdentityRoute, SimpleRepresentation::HtmlV1).hex(),
                    key(&other, &IdentityRoute, SimpleRepresentation::LegacyHtml).hex()
                );
                continue;
            }
            7 => {
                let another = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
                let routed = Routed {
                    base: Url::parse(&another.url).unwrap(),
                    reverse_rules: false,
                };
                let original = Routed {
                    base: Url::parse(&server.url).unwrap(),
                    reverse_rules: false,
                };
                assert_ne!(
                    key(&other, &original, SimpleRepresentation::JsonV1).hex(),
                    key(&other, &routed, SimpleRepresentation::JsonV1).hex()
                );
                continue;
            }
            _ => {
                let another = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
                other.registry =
                    PythonRegistry::new("corp", &format!("{}/tenant/simple/", another.url), None)
                        .unwrap();
                let fixed = Routed {
                    base: Url::parse(&server.url).unwrap(),
                    reverse_rules: false,
                };
                assert_ne!(
                    key(&fixture, &fixed, SimpleRepresentation::JsonV1).hex(),
                    key(&other, &fixed, SimpleRepresentation::JsonV1).hex()
                );
                continue;
            }
        }
        assert_ne!(
            initial,
            key(&other, &IdentityRoute, SimpleRepresentation::JsonV1).hex(),
            "cache identity omitted dimension {dimension}"
        );
    }
    let response = entry(
        &fixture,
        &IdentityRoute,
        page("identity private"),
        1_700_000_000,
        None,
    );
    seed(&fixture, &IdentityRoute, &response);
    assert_eq!(
        present(
            fetch(
                &fixture,
                &IdentityRoute,
                &NoCredentials,
                policy(NetworkPolicy::Allowed, 1_700_000_001),
                Default::default()
            )
            .await
        )
        .files()[0]
            .yanked()
            .reason(),
        Some("identity private")
    );
    assert!(server.requests().is_empty());
    let mut other = Fixture::new(&server.url);
    other.client.cache_root = fixture.client.cache_root.clone();
    other.auth_scope = AuthScopeKey::from_local_revision([9; 32]);
    present(
        fetch(
            &other,
            &IdentityRoute,
            &NoCredentials,
            policy(NetworkPolicy::Allowed, 1_700_000_001),
            Default::default(),
        )
        .await,
    );
    assert_eq!(
        server.requests().len(),
        1,
        "new local auth revision reused old cache"
    );
}

#[tokio::test]
async fn corrupt_identity_hash_size_and_partial_cache_never_self_heal_online() {
    for dimension in 0..5 {
        let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
        let fixture = Fixture::new(&server.url);
        let cached = entry(
            &fixture,
            &IdentityRoute,
            page("trusted bytes"),
            1_700_000_000,
            None,
        );
        let destination = seed(&fixture, &IdentityRoute, &cached);
        let mut bytes = fs::read(&destination).unwrap();
        if dimension == 0 {
            bytes[0] ^= 1;
        } else if dimension == 1 {
            bytes.truncate(bytes.len() - 1);
        } else if dimension == 2 {
            *bytes.last_mut().unwrap() ^= 1;
        } else {
            let start = cache::MAGIC.len();
            let len = u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap()) as usize;
            let mut header: serde_json::Value =
                serde_json::from_slice(&bytes[start + 8..start + 8 + len]).unwrap();
            if dimension == 3 {
                header["entry_key"] = serde_json::json!("fe".repeat(32));
            } else {
                header["representation"] = serde_json::json!("html-v1");
            }
            let header = serde_json::to_vec(&header).unwrap();
            bytes = cache::MAGIC.to_vec();
            bytes.extend_from_slice(&(header.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&header);
            bytes.extend_from_slice(&cached.body);
        }
        fs::write(&destination, &bytes).unwrap();
        let result = fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            policy(NetworkPolicy::Allowed, 1_700_000_001),
            Default::default(),
        )
        .await;
        assert!(
            matches!(result, Err(SimpleClientError::InvalidCache(_))),
            "dimension {dimension}: {result:?}"
        );
        assert!(server.requests().is_empty());
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }
}

#[tokio::test]
async fn cache_byte_limits_and_nonregular_entries_fail_before_http() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    for dimension in 0..4 {
        let fixture = Fixture::new(&server.url);
        let destination = seed(
            &fixture,
            &IdentityRoute,
            &entry(
                &fixture,
                &IdentityRoute,
                json_page(true),
                1_700_000_000,
                None,
            ),
        );
        let original = fs::read(&destination).unwrap();
        let mut limits = SimpleClientLimits::default();
        if dimension == 0 {
            limits.max_cache_bytes = fs::metadata(&destination).unwrap().len() - 1;
        } else {
            fs::remove_file(&destination).unwrap();
            if dimension == 1 {
                fs::create_dir(&destination).unwrap();
            }
            #[cfg(unix)]
            if dimension == 2 {
                let outside = fixture._cache.path().join("outside");
                fs::write(&outside, &original).unwrap();
                fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
                symlink(&outside, &destination).unwrap();
            }
            #[cfg(unix)]
            if dimension == 3 {
                let name =
                    std::ffi::CString::new(destination.as_os_str().as_encoded_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
        }
        // Supervised worker catches an accidental blocking FIFO open. No raw
        // pathname is used as a successful reader capability.
        let (send, receive) = mpsc::channel();
        let destination_for_cleanup = destination.clone();
        let worker = thread::spawn(move || {
            let canonical = fixture.registry.project_url(&fixture.name).unwrap();
            let physical = canonical.as_url().clone();
            let result = cache::read(
                &fixture.client.cache_root,
                &request(&fixture, canonical.as_url(), &physical, &IdentityRoute),
                limits,
            );
            send.send(result.map(|value| value.is_some())).unwrap();
        });
        let early = receive.recv_timeout(Duration::from_secs(1));
        if early.is_err() {
            #[cfg(unix)]
            {
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&destination_for_cleanup);
            }
            let _ = receive.recv_timeout(Duration::from_secs(1));
            panic!("cache reader blocked on a nonregular entry");
        }
        let result = early.unwrap();
        worker.join().unwrap();
        if dimension == 0 {
            assert!(
                matches!(
                    result,
                    Err(SimpleClientError::LimitExceeded(
                        SimpleClientLimitKind::CacheBytes
                    ))
                ),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(SimpleClientError::InvalidCache(_))),
                "{result:?}"
            );
        }
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn failed_refreshes_preserve_the_old_complete_cache_and_leave_no_partial_entry() {
    for malformed in [false, true] {
        let reply = if malformed {
            Reply::page(Some(JSON_TYPE), b"private bad JSON".to_vec())
        } else {
            Reply::page(Some(JSON_TYPE), json_page(true)).status(500)
        };
        let server = Server::new(reply);
        let fixture = Fixture::new(&server.url);
        let destination = seed(
            &fixture,
            &IdentityRoute,
            &entry(
                &fixture,
                &IdentityRoute,
                page("previous"),
                1_700_000_000,
                None,
            ),
        );
        let before = fs::read(&destination).unwrap();
        let mut online = policy(NetworkPolicy::Allowed, 1_700_010_000);
        online.refresh = true;
        assert!(
            fetch(
                &fixture,
                &IdentityRoute,
                &NoCredentials,
                online,
                Default::default()
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), before);
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
        let fresh = Fixture::new(&server.url);
        assert!(
            fetch(
                &fresh,
                &IdentityRoute,
                &NoCredentials,
                online,
                Default::default()
            )
            .await
            .is_err()
        );
        assert!(!fresh.client.cache_root.exists());
    }
}

#[test]
fn atomic_cache_publication_never_exposes_a_partial_page_to_concurrent_readers() {
    let fixture = Fixture::new("http://127.0.0.1:9");
    let old = entry(
        &fixture,
        &IdentityRoute,
        page("previous"),
        1_700_000_000,
        None,
    );
    let new = entry(
        &fixture,
        &IdentityRoute,
        page("replacement"),
        1_700_000_001,
        None,
    );
    let destination = seed(&fixture, &IdentityRoute, &old);
    let finished = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&finished);
    let failures = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&failures);
    let old_body = old.body.clone();
    let new_body = new.body.clone();
    let reader_path = destination.clone();
    let (started, ready) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut first = true;
        while !stop.load(Ordering::Acquire) {
            let value = fs::read(&reader_path).ok();
            if value
                .as_deref()
                .and_then(raw_body)
                .is_none_or(|body| body != old_body && body != new_body)
            {
                observed
                    .lock()
                    .unwrap()
                    .push("reader observed a partial or missing page");
            }
            if first {
                started.send(()).unwrap();
                first = false;
            }
        }
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let canonical = fixture.registry.project_url(&fixture.name).unwrap();
    let physical = canonical.as_url().clone();
    let request = request(&fixture, canonical.as_url(), &physical, &IdentityRoute);
    let mut result = Ok(());
    for index in 0..8 {
        let value = if index % 2 == 0 { &old } else { &new };
        result = cache::publish(
            &fixture.client.cache_root,
            &request,
            value,
            &reqwest::header::HeaderMap::new(),
            Default::default(),
        );
        if result.is_err() {
            break;
        }
    }
    finished.store(true, Ordering::Release);
    reader.join().unwrap();
    result.unwrap();
    assert!(failures.lock().unwrap().is_empty());
    assert_eq!(raw_body(&fs::read(destination).unwrap()).unwrap(), new.body);
}

#[tokio::test]
async fn etag_304_revalidates_only_a_complete_matching_cached_body() {
    let server = Server::handler(|_, _| Reply::page(None, Vec::new()).status(304));
    let fixture = Fixture::new(&server.url);
    seed(
        &fixture,
        &IdentityRoute,
        &entry(
            &fixture,
            &IdentityRoute,
            page("from cached body"),
            1_700_000_000,
            Some("\"fixture-etag\""),
        ),
    );
    let online = policy(NetworkPolicy::Allowed, 1_700_010_000);
    assert_eq!(
        present(
            fetch(
                &fixture,
                &IdentityRoute,
                &NoCredentials,
                online,
                Default::default()
            )
            .await
        )
        .files()[0]
            .yanked()
            .reason(),
        Some("from cached body")
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        server.requests()[0]
            .headers
            .get("if-none-match")
            .map(String::as_str),
        Some("\"fixture-etag\"")
    );
    present(
        fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            online,
            Default::default(),
        )
        .await,
    );
    assert_eq!(
        server.requests().len(),
        1,
        "304 did not renew cache freshness"
    );
    let fresh = Fixture::new(&server.url);
    let result = fetch(
        &fresh,
        &IdentityRoute,
        &NoCredentials,
        online,
        Default::default(),
    )
    .await;
    assert!(
        matches!(&result, Err(SimpleClientError::InvalidCache(_))),
        "a 304 without any cached body returned {result:?}"
    );
}

#[tokio::test]
async fn private_cache_honors_session_only_documents_and_never_prints_raw_secrets() {
    let mut signed: serde_json::Value = serde_json::from_slice(&json_page(true)).unwrap();
    signed["files"][0]["url"] = serde_json::json!(format!(
        "../../files/{NAME}?token=private-file-secret#sha256={}",
        "ab".repeat(32)
    ));
    let body = serde_json::to_vec(&signed).unwrap();
    let server = Server::new(Reply::page(Some(JSON_TYPE), body.clone()));
    let fixture = Fixture::new(&server.url);
    let online = policy(NetworkPolicy::Allowed, 1_700_000_000);
    let project = present(
        fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            online,
            Default::default(),
        )
        .await,
    );
    assert!(project.files()[0].locator().is_ephemeral());
    let destination = path(&fixture, &IdentityRoute);
    assert!(
        destination.is_file(),
        "stable documents with signed file facts remain privately cacheable"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&fixture.client.cache_root)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(destination.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert!(!format!("{project:?} {:?}", fixture.client).contains("private-file-secret"));
    for condition in 0..3 {
        let mut reply = Reply::page(Some(JSON_TYPE), body.clone());
        if condition == 0 {
            reply = reply.header("Cache-Control", "private, no-store");
        }
        if condition == 1 {
            reply = reply.header("Vary", "X-Private-Unknown");
        }
        let server = Server::new(reply);
        let fixture = Fixture::new(&server.url);
        let router: &dyn SourceRequestRouter = if condition == 2 {
            &QueryRoute
        } else {
            &IdentityRoute
        };
        let project =
            present(fetch(&fixture, router, &NoCredentials, online, Default::default()).await);
        assert_eq!(project.files()[0].filename(), NAME);
        assert!(
            !fixture.client.cache_root.exists(),
            "session-only response was persisted"
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn a_new_representation_replaces_the_prior_complete_observation() {
    let server = Server::handler(|_, index| {
        if index == 0 {
            Reply::page(Some(JSON_TYPE), page("old JSON"))
        } else {
            Reply::page(Some("text/html"), html_page("", "new HTML"))
        }
    });
    let fixture = Fixture::new(&server.url);
    let first = policy(NetworkPolicy::Allowed, 1_700_000_000);
    let observed = present(
        fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            first,
            Default::default(),
        )
        .await,
    );
    assert_eq!(observed.files()[0].yanked().reason(), Some("old JSON"));
    let mut refresh = policy(NetworkPolicy::Allowed, 1_700_000_001);
    refresh.refresh = true;
    let observed = present(
        fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            refresh,
            Default::default(),
        )
        .await,
    );
    assert_eq!(observed.files()[0].yanked().reason(), Some("new HTML"));
    assert_eq!(server.requests().len(), 2);
    let cached = present(
        fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            policy(NetworkPolicy::Allowed, 1_700_000_002),
            Default::default(),
        )
        .await,
    );
    assert_eq!(
        cached.files()[0].yanked().reason(),
        Some("new HTML"),
        "fixed representation preference resurrected the old response"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "a fresh new representation was not reusable"
    );
}

#[tokio::test]
async fn a_304_cannot_change_the_media_type_of_its_cached_body() {
    let server = Server::new(Reply::page(Some("text/html"), Vec::new()).status(304));
    let fixture = Fixture::new(&server.url);
    let destination = seed(
        &fixture,
        &IdentityRoute,
        &entry(
            &fixture,
            &IdentityRoute,
            page("existing JSON"),
            1_700_000_000,
            Some("\"original-etag\""),
        ),
    );
    let before = fs::read(&destination).unwrap();
    let result = fetch(
        &fixture,
        &IdentityRoute,
        &NoCredentials,
        policy(NetworkPolicy::Allowed, 1_700_010_000),
        Default::default(),
    )
    .await;
    assert!(
        matches!(&result, Err(SimpleClientError::InvalidCache(_))),
        "contradictory 304 media type returned {result:?}"
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        server.requests()[0]
            .headers
            .get("if-none-match")
            .map(String::as_str),
        Some("\"original-etag\"")
    );
    assert_eq!(
        fs::read(destination).unwrap(),
        before,
        "failed revalidation replaced its old complete response"
    );
}

#[tokio::test]
async fn a_304_validator_must_identify_its_existing_cached_body() {
    let cases: &[(&str, &[&str], bool)] = &[
        ("\"old\"", &["\"different\""], false),
        ("\"old\"", &["W/\"different\""], false),
        ("W/\"old\"", &["\"old\""], false),
        ("\"old\"", &["unquoted-invalid"], false),
        ("\"old\"", &["\"old\"", "\"other\""], false),
        ("\"old\"", &["\"old\""], true),
        ("\"old\"", &["W/\"old\""], true),
        ("W/\"old\"", &["W/\"old\""], true),
        // Existing contract: an otherwise valid conditional 304 which omits
        // ETag keeps the exact validator that was sent, rather than inventing one.
        ("\"old\"", &[], true),
    ];
    for (index, (stored, returned, valid)) in cases.iter().enumerate() {
        let mut reply = Reply::page(None, Vec::new()).status(304);
        for returned in *returned {
            reply = reply.header("ETag", returned);
        }
        let server = Server::new(reply);
        let fixture = Fixture::new(&server.url);
        let destination = seed(
            &fixture,
            &IdentityRoute,
            &entry(
                &fixture,
                &IdentityRoute,
                page("same body"),
                1_700_000_000,
                Some(stored),
            ),
        );
        let before = fs::read(&destination).unwrap();
        let online = policy(NetworkPolicy::Allowed, 1_700_010_000);
        let result = fetch(
            &fixture,
            &IdentityRoute,
            &NoCredentials,
            online,
            Default::default(),
        )
        .await;
        assert_eq!(server.requests().len(), 1, "validator case {index}");
        assert_eq!(
            server.requests()[0]
                .headers
                .get("if-none-match")
                .map(String::as_str),
            Some(*stored)
        );
        if *valid {
            assert_eq!(
                present(result).files()[0].yanked().reason(),
                Some("same body")
            );
            present(
                fetch(
                    &fixture,
                    &IdentityRoute,
                    &NoCredentials,
                    online,
                    Default::default(),
                )
                .await,
            );
            assert_eq!(
                server.requests().len(),
                1,
                "valid 304 did not renew case {index}"
            );
        } else {
            assert!(
                matches!(&result, Err(SimpleClientError::InvalidCache(_))),
                "validator case {index} changed its cached body identity: {result:?}"
            );
            assert_eq!(
                fs::read(&destination).unwrap(),
                before,
                "validator case {index} changed old cache bytes"
            );
        }
    }
}
