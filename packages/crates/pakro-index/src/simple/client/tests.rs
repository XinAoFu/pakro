use super::test_support::*;
use super::*;
use std::net::TcpListener;

#[tokio::test]
async fn owned_http_client_negotiates_json_at_the_normalized_project_path() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let mut fixture = Fixture::new(&server.url);
    fixture.client = SimpleIndexClient::new(
        fixture._cache.path().join("owned-client"),
        SimpleHttpConfig::default(),
    )
    .unwrap();
    let project = present(fixture.fetch(SimpleClientLimits::default()).await);
    assert_eq!(project.name().as_str(), "demo-pkg");
    assert_eq!(project.source().as_str(), "corp");
    assert_eq!(project.files().len(), 1);
    assert_eq!(project.files()[0].filename(), NAME);
    assert_eq!(
        project.files()[0].locator().url().as_str(),
        format!("{}/tenant/files/{NAME}", server.url)
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, "/tenant/simple/demo-pkg/");
    assert_eq!(requests[0].headers.get("accept").unwrap(), ACCEPT);
    assert_eq!(
        requests[0].headers.get("accept-encoding").unwrap(),
        "identity"
    );
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(!requests[0].headers.contains_key("cookie"));

    // The application can supply an explicit proxy without granting an
    // arbitrary default-header builder or consulting ambient proxy settings.
    let proxy = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let canonical = format!("http://{}", unused.local_addr().unwrap());
    drop(unused);
    let mut fixture = Fixture::new(&canonical);
    fixture.client = SimpleIndexClient::new(
        fixture._cache.path().join("explicit-proxy"),
        SimpleHttpConfig {
            proxies: vec![reqwest::Proxy::all(&proxy.url).unwrap()],
            ..SimpleHttpConfig::default()
        },
    )
    .unwrap();
    let project = present(fixture.fetch(SimpleClientLimits::default()).await);
    assert_eq!(
        project.files()[0].locator().url().as_str(),
        format!("{canonical}/tenant/files/{NAME}")
    );
    assert_eq!(proxy.requests().len(), 1);
    assert_eq!(
        proxy.requests()[0].target,
        format!("{canonical}/tenant/simple/demo-pkg/")
    );
}

#[tokio::test]
async fn response_media_type_selects_exactly_one_supported_parser() {
    for content_type in [HTML_TYPE, "text/html; charset=\"UTF-8\""] {
        let server = Server::new(Reply::page(Some(content_type), html_page("", "reason")));
        let fixture = Fixture::new(&server.url);
        let project = present(fixture.fetch(SimpleClientLimits::default()).await);
        assert_eq!(project.files()[0].filename(), NAME);
        assert_eq!(project.files()[0].yanked().reason(), Some("reason"));
        assert_eq!(server.requests().len(), 1);
    }
    for content_type in [Some("application/json"), Some("text/plain"), None] {
        let server = Server::new(Reply::page(content_type, json_page(true)));
        let fixture = Fixture::new(&server.url);
        assert!(matches!(
            fixture.fetch(SimpleClientLimits::default()).await,
            Err(SimpleClientError::UnsupportedMediaType)
        ));
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn invalid_json_never_retries_html_or_the_release_json_api() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), html_page("", "reason")));
    let fixture = Fixture::new(&server.url);
    let result = fixture.fetch(SimpleClientLimits::default()).await;
    assert!(matches!(
        result,
        Err(SimpleClientError::Parse(SimpleError::InvalidJson))
    ));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].target, "/tenant/simple/demo-pkg/");
}

#[tokio::test]
async fn actual_project_404_is_distinct_from_empty_projects_and_http_errors() {
    for status in [200, 404, 401, 403, 429, 500] {
        let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(false)).status(status));
        let fixture = Fixture::new(&server.url);
        let result = fixture.fetch(SimpleClientLimits::default()).await;
        match status {
            200 => assert!(present(result).files().is_empty()),
            404 => assert!(matches!(result, Ok(ProjectLookup::Missing404))),
            _ => assert!(
                matches!(result, Err(SimpleClientError::HttpStatus(code)) if code == status)
            ),
        }
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn zero_resource_and_timeout_inputs_fail_before_http_or_cache_creation() {
    let server = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    for dimension in 0..7 {
        let fixture = Fixture::new(&server.url);
        let mut limits = SimpleClientLimits::default();
        match dimension {
            0 => limits.max_wire_bytes = 0,
            1 => limits.max_decoded_bytes = 0,
            2 => limits.max_cache_bytes = 0,
            3 => limits.max_header_bytes = 0,
            4 => limits.max_headers = 0,
            5 => limits.parser.max_files = 0,
            _ => limits.html.max_tokens = 0,
        }
        assert!(matches!(
            fixture.fetch(limits).await,
            Err(SimpleClientError::InvalidLimits)
        ));
        assert!(!fixture.client.cache_root.exists());
    }
    let fixture = Fixture::new(&server.url);
    assert!(matches!(
        SimpleIndexClient::new(
            &fixture.client.cache_root,
            SimpleHttpConfig {
                request_timeout: Duration::ZERO,
                ..SimpleHttpConfig::default()
            }
        ),
        Err(SimpleClientError::InvalidConfiguration(_))
    ));
    assert!(matches!(
        SimpleIndexClient::new(
            &fixture.client.cache_root,
            SimpleHttpConfig {
                proxies: vec![reqwest::Proxy::all(&server.url).unwrap(); 17],
                ..SimpleHttpConfig::default()
            }
        ),
        Err(SimpleClientError::InvalidConfiguration(_))
    ));
    assert!(!fixture.client.cache_root.exists());
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn declared_eof_and_chunked_bodies_obey_wire_limits_and_real_eof() {
    let body = json_page(true);
    for eof in [false, true] {
        for limit in [body.len() as u64, body.len() as u64 - 1] {
            let mut reply = Reply::page(Some(JSON_TYPE), body.clone());
            if eof {
                reply.declared_size = None;
            }
            let server = Server::new(reply);
            let fixture = Fixture::new(&server.url);
            let limits = SimpleClientLimits {
                max_wire_bytes: limit,
                ..Default::default()
            };
            let result = fixture.fetch(limits).await;
            if limit == body.len() as u64 {
                assert_eq!(present(result).files()[0].filename(), NAME);
            } else {
                assert!(matches!(
                    result,
                    Err(SimpleClientError::LimitExceeded(
                        SimpleClientLimitKind::WireBytes
                    ))
                ));
            }
            assert_eq!(server.requests().len(), 1);
        }
    }
    // A complete valid JSON prefix at the byte limit must still wait for EOF;
    // the later extra byte may not be silently ignored after parsing the prefix.
    let gate = Gate::new();
    let release = ReleaseGate(gate.clone());
    let mut reply = Reply::page(Some(JSON_TYPE), body.clone());
    reply.chunked = true;
    reply.gate_after_body = Some(gate.clone());
    reply.suffix = b"X".to_vec();
    let server = Server::new(reply);
    let fixture = Fixture::new(&server.url);
    let limits = SimpleClientLimits {
        max_wire_bytes: body.len() as u64,
        ..Default::default()
    };
    let mut task = tokio::spawn(async move { fixture.fetch(limits).await });
    let early = tokio::time::timeout(Duration::from_millis(200), &mut task).await;
    gate.release();
    assert!(early.is_err(), "a matching prefix is not proof of EOF");
    assert!(matches!(
        task.await.unwrap(),
        Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::WireBytes
        ))
    ));
    drop(release);
    assert_eq!(server.requests().len(), 1);
    // A declared overflow is detected without waiting for any body byte.
    let mut reply = Reply::page(Some(JSON_TYPE), Vec::new());
    reply.declared_size = Some(body.len() + 1);
    reply.body_delay = Duration::from_secs(5);
    let server = Server::new(reply);
    let fixture = Fixture::new(&server.url);
    let result = tokio::time::timeout(Duration::from_secs(2), fixture.fetch(limits)).await;
    assert!(matches!(
        result,
        Ok(Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::WireBytes
        )))
    ));
}

#[tokio::test]
async fn automatic_content_decoding_cannot_bypass_the_raw_body_contract() {
    // Real gzip member containing a valid empty Simple JSON page (mtime=0).
    const GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 202, 77, 45, 73, 84, 178, 170, 86, 74, 44, 200,
        212, 45, 75, 45, 42, 206, 204, 207, 83, 178, 82, 50, 212, 51, 80, 170, 213, 81, 202, 75,
        204, 77, 5, 242, 82, 82, 115, 243, 117, 11, 178, 211, 149, 116, 148, 210, 50, 115, 82, 139,
        149, 172, 162, 99, 107, 1, 54, 66, 212, 0, 59, 0, 0, 0,
    ];
    let server = Server::new(Reply::page(Some(JSON_TYPE), GZIP).header("Content-Encoding", "gzip"));
    let fixture = Fixture::new(&server.url);
    let result = fixture.fetch(SimpleClientLimits::default()).await;
    assert!(matches!(
        result,
        Err(SimpleClientError::UnsupportedContentEncoding)
    ));
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        server.requests()[0].headers.get("accept-encoding").unwrap(),
        "identity"
    );
}

#[tokio::test]
async fn redirects_and_slow_bodies_share_one_request_deadline() {
    let body = json_page(true);
    let server = Server::handler(move |_, index| {
        if index == 0 {
            let mut reply = Reply::page(None, Vec::new())
                .status(302)
                .header("Location", "step/");
            reply.header_delay = Duration::from_millis(650);
            reply
        } else {
            let mut reply = Reply::page(Some(JSON_TYPE), body.clone());
            reply.body_delay = Duration::from_millis(650);
            reply
        }
    });
    let mut fixture = Fixture::new(&server.url);
    fixture.client.config.request_timeout = Duration::from_secs(1);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.fetch(SimpleClientLimits::default()),
    )
    .await;
    assert!(matches!(result, Ok(Err(SimpleClientError::Timeout))));
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.requests()[1].target, "/tenant/simple/demo-pkg/step/");
}

#[tokio::test]
async fn network_json_is_strict_utf8_even_with_a_charset_parameter() {
    for content_type in [
        JSON_TYPE,
        "application/vnd.pypi.simple.v1+json; charset=utf-8",
    ] {
        let mut body = json_page(true);
        let index = body.iter().position(|byte| *byte == b'{').unwrap() + 1;
        body.splice(
            index..index,
            [b'"', b'x', b'"', b':', b'"', 0xff, b'"', b','],
        );
        let server = Server::new(Reply::page(Some(content_type), body));
        let fixture = Fixture::new(&server.url);
        assert!(matches!(
            fixture.fetch(SimpleClientLimits::default()).await,
            Err(SimpleClientError::InvalidEncoding)
        ));
        assert_eq!(server.requests().len(), 1);
    }
    // RFC8259 defines no charset parameter: it cannot ask the receiver to
    // reinterpret network JSON's valid UTF-8 as a legacy single-byte encoding.
    let mut value: serde_json::Value = serde_json::from_slice(&json_page(true)).unwrap();
    value["files"][0]["yanked"] = serde_json::json!("café");
    let server = Server::new(Reply::page(
        Some("application/vnd.pypi.simple.v1+json; charset=windows-1252"),
        serde_json::to_vec(&value).unwrap(),
    ));
    let fixture = Fixture::new(&server.url);
    assert_eq!(
        present(fixture.fetch(SimpleClientLimits::default()).await).files()[0]
            .yanked()
            .reason(),
        Some("café")
    );
}

fn latin1_html(head: &str, reason: &str) -> Vec<u8> {
    html_page(head, reason)
        .chars()
        .map(|character| u8::try_from(character as u32).unwrap())
        .collect()
}

#[tokio::test]
async fn html_transport_charset_bom_and_bounded_meta_prescan_decode_real_text() {
    let mut cases = vec![
        ("text/html; charset=windows-1252", latin1_html("", "café")),
        (
            "text/html; charset=\"UTF-8\"",
            html_page("", "café").into_bytes(),
        ),
        (
            "text/html",
            latin1_html("<meta charset='windows-1252'>", "café"),
        ),
        (
            "text/html",
            latin1_html(
                "<meta http-equiv='content-type' content='text/html; charset=windows-1252'>",
                "café",
            ),
        ),
        (
            "text/html",
            latin1_html(
                "<!-- <meta charset=utf-8> --><meta charset=windows-1252>",
                "café",
            ),
        ),
    ];
    // A conforming declaration is in the first 1024 bytes. The UTF-8 fallback
    // cannot be replaced by a conveniently found later byte pattern.
    let late = format!("<!--{}--><meta charset=windows-1252>", "x".repeat(2048));
    cases.push(("text/html", html_page(&late, "café").into_bytes()));
    let mut utf8_bom = b"\xef\xbb\xbf".to_vec();
    utf8_bom.extend_from_slice(html_page("", "café").as_bytes());
    cases.push(("text/html; charset=windows-1252", utf8_bom));
    let mut utf16_bom = vec![0xff, 0xfe];
    for unit in html_page("", "café").encode_utf16() {
        utf16_bom.extend_from_slice(&unit.to_le_bytes());
    }
    cases.push(("text/html; charset=windows-1252", utf16_bom));
    for (index, (content_type, body)) in cases.into_iter().enumerate() {
        eprintln!("Simple HTML charset case={index}");
        let server = Server::new(Reply::page(Some(content_type), body));
        let fixture = Fixture::new(&server.url);
        let project = present(fixture.fetch(SimpleClientLimits::default()).await);
        assert_eq!(project.files()[0].yanked().reason(), Some("café"));
        assert_eq!(server.requests().len(), 1);
    }
    let server = Server::new(Reply::page(
        Some("text/html; charset=synthetic-charset-secret"),
        html_page("", "reason"),
    ));
    let fixture = Fixture::new(&server.url);
    assert!(matches!(
        fixture.fetch(SimpleClientLimits::default()).await,
        Err(SimpleClientError::UnsupportedEncoding)
    ));
}

#[tokio::test]
async fn decoded_utf8_expansion_has_its_own_exact_byte_limit() {
    let reason = "é".repeat(200);
    let raw = latin1_html("", &reason);
    let decoded = html_page("", &reason);
    assert!(raw.len() < decoded.len());
    for limit in [raw.len() as u64, decoded.len() as u64] {
        let server = Server::new(Reply::page(
            Some("text/html; charset=windows-1252"),
            raw.clone(),
        ));
        let fixture = Fixture::new(&server.url);
        let result = fixture
            .fetch(SimpleClientLimits {
                max_wire_bytes: raw.len() as u64,
                max_decoded_bytes: limit,
                ..Default::default()
            })
            .await;
        if limit == decoded.len() as u64 {
            assert_eq!(
                present(result).files()[0].yanked().reason(),
                Some(reason.as_str())
            );
        } else {
            assert!(matches!(
                result,
                Err(SimpleClientError::LimitExceeded(
                    SimpleClientLimitKind::DecodedBytes
                ))
            ));
        }
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn network_errors_never_echo_private_url_header_charset_or_response_data() {
    let cases = [
        (
            Reply::page(Some(JSON_TYPE), b"private-response-secret".to_vec()).status(500),
            0,
        ),
        (
            Reply::page(Some("application/private-media-secret"), json_page(true)),
            1,
        ),
        (
            Reply::page(
                Some("text/html; charset=private-charset-secret"),
                html_page("", "reason"),
            ),
            2,
        ),
        (
            Reply::page(Some(JSON_TYPE), b"private-response-secret".to_vec()),
            3,
        ),
        (
            Reply::page(None, Vec::new())
                .status(302)
                .header("Location", "http://user:private-location-secret@127.0.0.1/"),
            4,
        ),
    ];
    for (reply, kind) in cases {
        let server = Server::new(reply);
        let fixture = Fixture::new(&server.url);
        let error = fixture
            .with_router(&QueryRoute, SimpleClientLimits::default())
            .await
            .unwrap_err();
        assert!(
            match kind {
                0 => matches!(error, SimpleClientError::HttpStatus(500)),
                1 => matches!(error, SimpleClientError::UnsupportedMediaType),
                2 => matches!(error, SimpleClientError::UnsupportedEncoding),
                3 => matches!(error, SimpleClientError::Parse(SimpleError::InvalidJson)),
                _ => matches!(error, SimpleClientError::InvalidLocator),
            },
            "unexpected controlled diagnostic: {error:?}"
        );
        let diagnostic = format!("{error:?} {error} {:#?}", fixture.client);
        for secret in [
            "synthetic-request-secret",
            "private-response-secret",
            "private-media-secret",
            "private-charset-secret",
            "private-location-secret",
        ] {
            assert!(
                !diagnostic.contains(secret),
                "private data leaked through a diagnostic"
            );
        }
        assert_eq!(server.requests().len(), 1);
    }
}
