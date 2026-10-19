//! Independent A4-III fixtures: valid private values bypass constructor stubs
//! in request tests so each behavior reaches the actual transport/auth gate.

use super::test_support::*;
use super::*;
use crate::python_source::transport_rules_fingerprint;
use std::sync::Mutex;

fn origin(value: &str) -> HttpOrigin {
    let parsed = Url::parse(value).unwrap();
    HttpOrigin(Url::parse(&parsed.origin().ascii_serialization()).unwrap())
}
fn private_fixture(base: &str) -> Fixture {
    let mut fixture = Fixture::new(base);
    fixture.registry = PythonRegistry::new(
        "corp",
        &format!("{base}/tenant/simple/"),
        Some(CredentialRef::new("fixture-ref").unwrap()),
    )
    .unwrap();
    fixture
}
struct LocalCredentials {
    rows: Vec<(HttpOrigin, HttpOrigin, HeaderValue)>,
    calls: Mutex<Vec<(String, HttpOrigin, NetworkPolicy)>>,
}
impl LocalCredentials {
    fn new(rows: &[(&str, &str)]) -> Self {
        Self {
            rows: rows
                .iter()
                .map(|(url, secret)| {
                    let mut value = HeaderValue::from_str(secret).unwrap();
                    value.set_sensitive(true);
                    (origin(url), origin(url), value)
                })
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<(String, HttpOrigin, NetworkPolicy)> {
        self.calls.lock().unwrap().clone()
    }
}
impl SourceCredentials for LocalCredentials {
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
            .find(|(request, _, _)| request == requested)
            .map(|(_, response, value)| OriginAuthorization {
                origin: response.clone(),
                value: value.clone(),
            }))
    }
}
async fn fetch(
    fixture: &Fixture,
    router: &dyn SourceRequestRouter,
    credentials: &dyn SourceCredentials,
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
            SimpleFetchPolicy {
                network: NetworkPolicy::Allowed,
                fresh_for: Duration::from_secs(60),
                now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                refresh: true,
            },
            Default::default(),
        )
        .await
}
fn authorization(request: &Request) -> Option<&str> {
    request.headers.get("authorization").map(String::as_str)
}

#[test]
fn typed_origin_authorization_and_grants_validate_without_secret_output() {
    let document =
        Url::parse("https://REGISTRY.example:443/tenant/page/?token=synthetic-origin-secret")
            .unwrap();
    let actual = HttpOrigin::new(&document).unwrap();
    assert_eq!(actual, origin("https://registry.example/"));
    assert_ne!(actual, origin("http://registry.example/"));
    assert_ne!(actual, origin("https://registry.example:444/"));
    assert_eq!(
        HttpOrigin::new(&Url::parse("https://[::1]:443/path/").unwrap()).unwrap(),
        origin("https://[::1]/")
    );
    for value in [
        "file:///synthetic-origin-secret",
        "https://user:synthetic-origin-secret@example.test/",
        "https://example.test/#synthetic-origin-secret",
    ] {
        let error = HttpOrigin::new(&Url::parse(value).unwrap()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("synthetic-origin-secret"));
    }
    let header = OriginAuthorization::new(
        actual.clone(),
        HeaderValue::from_static("Bearer synthetic-authorization-secret"),
    )
    .unwrap();
    assert_eq!(header.origin, actual);
    assert!(header.value.is_sensitive());
    assert!(OriginAuthorization::new(actual.clone(), HeaderValue::from_static("")).is_err());
    let grants = AuthOriginGrants::with_additional(vec![actual]).unwrap();
    assert!(
        !format!(
            "{header:?} {grants:?} {document_origin:?}",
            document_origin = HttpOrigin::new(&document).unwrap()
        )
        .contains("synthetic-")
    );
    let too_many = (0..17)
        .map(|index| origin(&format!("http://example.test:{}/", 10000 + index)))
        .collect();
    assert!(AuthOriginGrants::with_additional(too_many).is_err());
}

#[tokio::test]
async fn source_credentials_are_origin_bound_and_never_fetch_file_hosts() {
    let files = Server::new(Reply::page(None, "must not fetch an artifact"));
    let mut page: serde_json::Value = serde_json::from_slice(&json_page(true)).unwrap();
    page["files"][0]["url"] = serde_json::json!(format!("{}/files/{NAME}", files.url));
    let server = Server::new(Reply::page(
        Some(JSON_TYPE),
        serde_json::to_vec(&page).unwrap(),
    ));
    let fixture = private_fixture(&server.url);
    let credentials = LocalCredentials::new(&[(&server.url, "Bearer source-only-secret")]);
    let observed = present(fetch(&fixture, &IdentityRoute, &credentials).await);
    assert_eq!(observed.source(), fixture.registry.id());
    assert_eq!(
        observed.files()[0].locator().url().origin(),
        Url::parse(&files.url).unwrap().origin()
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        authorization(&server.requests()[0]),
        Some("Bearer source-only-secret")
    );
    assert!(
        files.requests().is_empty(),
        "metadata observation fetched an artifact"
    );
    assert_eq!(
        credentials.calls(),
        vec![(
            "fixture-ref".into(),
            origin(&server.url),
            NetworkPolicy::Allowed
        )]
    );

    // A malformed provider result must be rejected before a second request.
    let mut wrong = LocalCredentials::new(&[(&server.url, "Bearer wrong-origin-secret")]);
    wrong.rows[0].1 = origin(&files.url);
    let error = fetch(&fixture, &IdentityRoute, &wrong).await.unwrap_err();
    assert_eq!(error, SimpleClientError::Authentication);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(wrong.calls().len(), 1);
    assert!(!format!("{error:?} {error}").contains("wrong-origin-secret"));
}

#[tokio::test]
async fn redirects_reauthorize_each_granted_origin_without_forwarding_previous_headers() {
    let destination = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let destination_url = format!("{}/redirected/demo-pkg/", destination.url);
    let source = Server::new(
        Reply::page(None, Vec::new())
            .status(302)
            .header("Location", &destination_url),
    );
    let mut fixture = private_fixture(&source.url);
    fixture.grants = AuthOriginGrants {
        additional: vec![origin(&destination.url)],
    };
    let credentials = LocalCredentials::new(&[
        (&source.url, "Bearer first-origin-secret"),
        (&destination.url, "Bearer second-origin-secret"),
    ]);
    let observed = present(fetch(&fixture, &IdentityRoute, &credentials).await);
    assert_eq!(observed.source(), fixture.registry.id());
    assert_eq!(
        observed.files()[0].locator().url().origin(),
        Url::parse(&destination.url).unwrap().origin()
    );
    assert_eq!(
        observed.files()[0].locator().url().path(),
        format!("/files/{NAME}")
    );
    assert_eq!(
        authorization(&source.requests()[0]),
        Some("Bearer first-origin-secret")
    );
    assert_eq!(
        authorization(&destination.requests()[0]),
        Some("Bearer second-origin-secret")
    );
    assert_eq!(source.requests().len(), 1);
    assert_eq!(destination.requests().len(), 1);
    assert_eq!(
        credentials.calls(),
        vec![
            (
                "fixture-ref".into(),
                origin(&source.url),
                NetworkPolicy::Allowed
            ),
            (
                "fixture-ref".into(),
                origin(&destination.url),
                NetworkPolicy::Allowed
            ),
        ]
    );

    let ungranted = private_fixture(&source.url);
    let credentials = LocalCredentials::new(&[
        (&source.url, "Bearer first-origin-secret"),
        (&destination.url, "Bearer second-origin-secret"),
    ]);
    assert_eq!(
        fetch(&ungranted, &IdentityRoute, &credentials)
            .await
            .unwrap_err(),
        SimpleClientError::Authentication
    );
    assert_eq!(source.requests().len(), 2);
    assert_eq!(destination.requests().len(), 1);
    assert_eq!(
        credentials.calls().len(),
        1,
        "ungranted origin reached provider"
    );
}

struct MirrorRoute {
    base: Url,
    explicit_redirect: bool,
}
impl SourceRequestRouter for MirrorRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(Some(&[(
            "explicit-loopback-mirror",
            if self.explicit_redirect {
                "with-redirect-ref"
            } else {
                "without-redirect-ref"
            },
        )]))
    }
    fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
        let mut request = canonical.clone();
        request.set_scheme(self.base.scheme()).unwrap();
        request.set_host(self.base.host_str()).unwrap();
        request.set_port(self.base.port()).unwrap();
        Ok(request)
    }
    fn canonical_redirect(
        &self,
        previous_canonical: &Url,
        previous_request: &Url,
        location: &str,
    ) -> SimpleClientResult<Option<Url>> {
        if !self.explicit_redirect || previous_request.origin() != self.base.origin() {
            return Ok(None);
        }
        let resolved = previous_request
            .join(location)
            .map_err(|_| SimpleClientError::InvalidLocator)?;
        if resolved.origin() != self.base.origin() || resolved.path() != "/redirected/demo-pkg/" {
            return Ok(None);
        }
        // Explicit application mapping for one known mirror route, rather
        // than a generic reverse-regex or a source selected from response data.
        let mut canonical = previous_canonical.clone();
        canonical.set_path("/redirected/demo-pkg/");
        Ok(Some(canonical))
    }
}

#[tokio::test]
async fn ungranted_mirrors_fail_before_provider_and_granted_mirrors_preserve_source() {
    let canonical_peer = Server::new(Reply::page(None, "not the request route"));
    let mirror = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let mut fixture = private_fixture(&canonical_peer.url);
    let router = MirrorRoute {
        base: Url::parse(&mirror.url).unwrap(),
        explicit_redirect: false,
    };
    let credentials = LocalCredentials::new(&[
        (&canonical_peer.url, "Bearer canonical-secret"),
        (&mirror.url, "Bearer mirror-secret"),
    ]);
    assert_eq!(
        fetch(&fixture, &router, &credentials).await.unwrap_err(),
        SimpleClientError::Authentication
    );
    assert!(
        credentials.calls().is_empty(),
        "ungranted mirror saw credential reference"
    );
    assert!(mirror.requests().is_empty());
    fixture.grants = AuthOriginGrants {
        additional: vec![origin(&mirror.url)],
    };
    let observed = present(fetch(&fixture, &router, &credentials).await);
    assert_eq!(mirror.requests().len(), 1);
    assert_eq!(
        authorization(&mirror.requests()[0]),
        Some("Bearer mirror-secret")
    );
    assert!(canonical_peer.requests().is_empty());
    assert_eq!(
        credentials.calls(),
        vec![(
            "fixture-ref".into(),
            origin(&mirror.url),
            NetworkPolicy::Allowed
        )]
    );
    assert_eq!(observed.source(), fixture.registry.id());
    assert_eq!(
        observed.files()[0].locator().url().origin(),
        Url::parse(&canonical_peer.url).unwrap().origin()
    );
    assert_eq!(
        observed.files()[0].locator().url().path(),
        format!("/tenant/files/{NAME}")
    );
}

#[tokio::test]
async fn tls_downgrades_reject_headers_or_queries_without_anonymous_retry() {
    let mirror = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let mut fixture = Fixture::new(&mirror.url);
    fixture.registry = PythonRegistry::new(
        "corp",
        "https://registry.example.test/tenant/simple/",
        Some(CredentialRef::new("fixture-ref").unwrap()),
    )
    .unwrap();
    fixture.grants = AuthOriginGrants {
        additional: vec![origin(&mirror.url)],
    };
    let router = MirrorRoute {
        base: Url::parse(&mirror.url).unwrap(),
        explicit_redirect: false,
    };
    let credentials = LocalCredentials::new(&[(&mirror.url, "Bearer must-not-downgrade-secret")]);
    let error = fetch(&fixture, &router, &credentials).await.unwrap_err();
    assert_eq!(error, SimpleClientError::InvalidLocator);
    assert!(
        mirror.requests().is_empty(),
        "secret downgrade made an anonymous or authorized request"
    );
    assert!(credentials.calls().len() <= 1);
    assert!(!format!("{error:?} {error}").contains("must-not-downgrade-secret"));

    // Query refusal is independent of whether any credential is configured.
    struct QueryMirror(MirrorRoute);
    impl SourceRequestRouter for QueryMirror {
        fn fingerprint(&self) -> TransportRulesFingerprint {
            self.0.fingerprint()
        }
        fn request_url(&self, canonical: &Url) -> SimpleClientResult<Url> {
            let mut request = self.0.request_url(canonical)?;
            request.set_query(Some("token=synthetic-downgrade-query-secret"));
            Ok(request)
        }
    }
    fixture.registry =
        PythonRegistry::new("corp", "https://registry.example.test/tenant/simple/", None).unwrap();
    let error = fetch(&fixture, &QueryMirror(router), &NoCredentials)
        .await
        .unwrap_err();
    assert_eq!(error, SimpleClientError::InvalidLocator);
    assert!(!format!("{error:?} {error}").contains("synthetic-downgrade-query-secret"));
    assert!(mirror.requests().is_empty());
}

#[tokio::test]
async fn redirect_limits_locations_and_explicit_mirror_resolution_are_bounded() {
    for location in [
        None,
        Some("/tenant/simple/demo-pkg/"),
        Some("https://user:synthetic-location-secret@example.test/"),
        Some("https://example.test/#synthetic-location-secret"),
    ] {
        let mut reply = Reply::page(None, Vec::new()).status(302);
        if let Some(location) = location {
            reply = reply.header("Location", location);
        }
        let server = Server::new(reply);
        let fixture = Fixture::new(&server.url);
        let error = fixture.fetch(Default::default()).await.unwrap_err();
        assert_eq!(error, SimpleClientError::InvalidLocator);
        assert_eq!(server.requests().len(), 1);
        assert!(!format!("{error:?} {error}").contains("synthetic-location-secret"));
    }
    let hop_server = Server::handler(|_, index| {
        Reply::page(None, Vec::new())
            .status(302)
            .header("Location", &format!("/redirect/{index}/"))
    });
    let mut fixture = Fixture::new(&hop_server.url);
    fixture.client.config.max_redirects = 1;
    assert_eq!(
        fixture.fetch(Default::default()).await.unwrap_err(),
        SimpleClientError::LimitExceeded(SimpleClientLimitKind::Redirects)
    );
    assert_eq!(hop_server.requests().len(), 2);
    let oversized = Server::new(Reply::page(None, Vec::new()).status(302).header(
        "Location",
        &format!("/synthetic-location-secret{}", "x".repeat(512)),
    ));
    let fixture = Fixture::new(&oversized.url);
    let error = fixture
        .fetch(SimpleClientLimits {
            max_header_bytes: 256,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        error,
        SimpleClientError::LimitExceeded(SimpleClientLimitKind::HeaderBytes)
    );
    assert!(!format!("{error:?} {error}").contains("synthetic-location-secret"));

    for scheme_relative in [false, true] {
        let mirror = Server::handler(move |request, _| {
            if request.target == "/tenant/simple/demo-pkg/" {
                let prefix = if scheme_relative { "//" } else { "http://" };
                Reply::page(None, Vec::new()).status(302).header(
                    "Location",
                    &format!("{prefix}{}/redirected/demo-pkg/", request.headers["host"]),
                )
            } else {
                Reply::page(Some(JSON_TYPE), json_page(true))
            }
        });
        let mut fixture = Fixture::new(&mirror.url);
        fixture.registry =
            PythonRegistry::new("corp", "https://registry.example.test/tenant/simple/", None)
                .unwrap();
        let router = MirrorRoute {
            base: Url::parse(&mirror.url).unwrap(),
            explicit_redirect: false,
        };
        assert_eq!(
            fetch(&fixture, &router, &NoCredentials).await.unwrap_err(),
            SimpleClientError::AmbiguousRedirect
        );
        assert_eq!(mirror.requests().len(), 1);
        let router = MirrorRoute {
            base: Url::parse(&mirror.url).unwrap(),
            explicit_redirect: true,
        };
        let observed = present(fetch(&fixture, &router, &NoCredentials).await);
        assert_eq!(
            observed.files()[0].locator().url().host_str(),
            Some("registry.example.test")
        );
        assert_eq!(observed.files()[0].locator().url().scheme(), "https");
        assert_eq!(
            observed.files()[0].locator().url().path(),
            format!("/files/{NAME}")
        );
        assert_eq!(mirror.requests().len(), 3);
    }
}

#[tokio::test]
async fn authentication_errors_do_not_retry_anonymous_or_another_source() {
    for status in [401, 403] {
        let other = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
        let source = Server::new(Reply::page(None, "synthetic-response-secret").status(status));
        let fixture = private_fixture(&source.url);
        let credentials = LocalCredentials::new(&[
            (&source.url, "Bearer auth-failure-secret"),
            (&other.url, "Bearer unrelated-secret"),
        ]);
        let error = fetch(&fixture, &IdentityRoute, &credentials)
            .await
            .unwrap_err();
        assert_eq!(error, SimpleClientError::HttpStatus(status));
        assert_eq!(source.requests().len(), 1);
        assert_eq!(
            authorization(&source.requests()[0]),
            Some("Bearer auth-failure-secret")
        );
        assert_eq!(
            credentials.calls(),
            vec![(
                "fixture-ref".into(),
                origin(&source.url),
                NetworkPolicy::Allowed
            )]
        );
        assert!(other.requests().is_empty());
        assert!(!format!("{error:?} {error}").contains("synthetic-"));
    }
}

#[tokio::test]
async fn project_presence_preserves_incompatible_file_facts_without_following_related_sources() {
    let related = Server::new(Reply::page(Some(JSON_TYPE), json_page(false)));
    let mut body: serde_json::Value = serde_json::from_slice(&json_page(true)).unwrap();
    body["meta"]["api-version"] = serde_json::json!("1.4");
    body["versions"] = serde_json::json!(["1.0"]);
    body["files"][0]["size"] = serde_json::json!(42);
    body["files"][0]["upload-time"] = serde_json::json!("2026-10-15T00:00:00Z");
    body["meta"]["tracks"] = serde_json::json!([format!("{}/other/simple/demo-pkg/", related.url)]);
    body["alternate-locations"] =
        serde_json::json!([format!("{}/alternate/simple/demo-pkg/", related.url)]);
    body["files"][0]["requires-python"] = serde_json::json!(">=99");
    let source = Server::new(Reply::page(
        Some(JSON_TYPE),
        serde_json::to_vec(&body).unwrap(),
    ));
    let fixture = private_fixture(&source.url);
    let credentials = LocalCredentials::new(&[(&source.url, "Bearer chosen-source-secret")]);
    let observed = present(fetch(&fixture, &IdentityRoute, &credentials).await);
    assert_eq!(observed.source(), fixture.registry.id());
    assert_eq!(observed.files().len(), 1);
    assert_eq!(observed.files()[0].requires_python(), Some(">=99"));
    assert_eq!(observed.relations().tracks().len(), 1);
    assert_eq!(observed.relations().alternate_locations().len(), 1);
    assert_eq!(source.requests().len(), 1);
    assert!(related.requests().is_empty());
    assert_eq!(credentials.calls().len(), 1);
}

#[tokio::test]
async fn absolute_canonical_redirects_are_forward_routed_through_the_mirror() {
    let mirror = Server::handler(|request, index| {
        if request.target == "/tenant/simple/demo-pkg/" && index != 0 {
            Reply::page(None, Vec::new()).status(302).header(
                "Location",
                "https://registry.example.test/moved/simple/demo-pkg/",
            )
        } else {
            Reply::page(Some(JSON_TYPE), json_page(true))
        }
    });
    let mut fixture = Fixture::new(&mirror.url);
    fixture.registry =
        PythonRegistry::new("corp", "https://registry.example.test/tenant/simple/", None).unwrap();
    let router = MirrorRoute {
        base: Url::parse(&mirror.url).unwrap(),
        explicit_redirect: false,
    };
    // First prove the same anonymous/default mirror route is usable before
    // exercising the canonical absolute Location. No TLS peer is contacted.
    let initial = present(fetch(&fixture, &router, &NoCredentials).await);
    assert_eq!(initial.source(), fixture.registry.id());
    assert_eq!(
        initial.files()[0].locator().url().host_str(),
        Some("registry.example.test")
    );
    assert_eq!(
        initial.files()[0].locator().url().path(),
        format!("/tenant/files/{NAME}")
    );
    assert_eq!(mirror.requests().len(), 1);

    let redirected = present(fetch(&fixture, &router, &NoCredentials).await);
    assert_eq!(redirected.source(), fixture.registry.id());
    assert_eq!(redirected.files()[0].locator().url().scheme(), "https");
    assert_eq!(
        redirected.files()[0].locator().url().host_str(),
        Some("registry.example.test")
    );
    assert_eq!(
        redirected.files()[0].locator().url().path(),
        format!("/moved/files/{NAME}")
    );
    let requests = mirror.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].target, "/moved/simple/demo-pkg/");
    assert!(
        requests
            .iter()
            .all(|request| authorization(request).is_none())
    );
}
