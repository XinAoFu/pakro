use super::test_support::*;
use super::*;

const CHILD: &str = "PAKRO_SIMPLE_PROXY_TEST_CHILD";
const PROXIES: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

#[tokio::test]
async fn f3_registry_transport_system_proxy_is_opt_in_and_respects_no_proxy() {
    const TEST: &str = "simple::client::proxy_tests::f3_registry_transport_system_proxy_is_opt_in_and_respects_no_proxy";
    if let Ok(mode) = std::env::var(CHILD) {
        let canonical = std::env::var("PAKRO_SIMPLE_PROXY_TEST_ORIGIN").unwrap();
        let mut fixture = Fixture::new(&canonical);
        fixture.client = SimpleIndexClient::new(
            fixture._cache.path().join("proxy-mode"),
            SimpleHttpConfig {
                use_system_proxy: mode != "default",
                connect_timeout: Duration::from_secs(2),
                request_timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .unwrap();
        let project = present(fixture.fetch(Default::default()).await);
        assert_eq!(project.name().as_str(), "demo-pkg");
        return;
    }

    let proxy = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    let origin = Server::new(Reply::page(Some(JSON_TYPE), json_page(true)));
    for (mode, canonical, no_proxy) in [
        ("proxy", "http://simple-proxy-only.invalid", ""),
        ("bypass", origin.url.as_str(), "127.0.0.1,localhost,::1"),
        ("default", origin.url.as_str(), ""),
    ] {
        // Set each case's environment only in its child. macOS system proxy
        // defaults are bypassed explicitly rather than changing process state.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, mode)
            .env("PAKRO_SIMPLE_PROXY_TEST_ORIGIN", canonical)
            .env("NO_PROXY", no_proxy)
            .env("no_proxy", no_proxy)
            .env_remove("REQUEST_METHOD");
        for variable in PROXIES {
            command.env(variable, &proxy.url);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "isolated proxy case {mode} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let proxied = proxy.requests();
    assert_eq!(proxied.len(), 1);
    assert_eq!(
        proxied[0].target,
        "http://simple-proxy-only.invalid/tenant/simple/demo-pkg/"
    );
    let direct = origin.requests();
    assert_eq!(direct.len(), 2);
    assert!(
        direct
            .iter()
            .all(|request| request.target == "/tenant/simple/demo-pkg/")
    );
}

#[tokio::test]
async fn f3_registry_transport_idle_timeout_bounds_a_slow_body_before_total_timeout() {
    let server = Server::new(Reply {
        body_delay: Duration::from_millis(250),
        ..Reply::page(Some(JSON_TYPE), json_page(true))
    });
    let mut fixture = Fixture::new(&server.url);
    fixture.client = SimpleIndexClient::new(
        fixture._cache.path().join("idle-bound"),
        SimpleHttpConfig {
            read_timeout: Some(Duration::from_millis(50)),
            request_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        fixture.fetch(Default::default()).await.unwrap_err(),
        SimpleClientError::Timeout
    );
    assert_eq!(server.requests().len(), 1);
    assert!(!fixture.client.cache_root.exists());
}

#[test]
fn f3_registry_transport_rejects_zero_idle_and_conflicting_proxy_configuration() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("not-created");
    for config in [
        SimpleHttpConfig {
            read_timeout: Some(Duration::ZERO),
            ..Default::default()
        },
        SimpleHttpConfig {
            use_system_proxy: true,
            proxies: vec![reqwest::Proxy::all("http://user:synthetic-secret@127.0.0.1:9").unwrap()],
            ..Default::default()
        },
    ] {
        assert!(!format!("{config:?}").contains("synthetic-secret"));
        assert!(matches!(
            SimpleIndexClient::new(&cache, config),
            Err(SimpleClientError::InvalidConfiguration(_))
        ));
        assert!(!cache.exists());
    }
    let defaults = SimpleHttpConfig::default();
    assert!(!defaults.use_system_proxy);
    assert_eq!(defaults.read_timeout, None);
}
