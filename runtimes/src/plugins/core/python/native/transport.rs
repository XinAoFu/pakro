//! Explicit transport routing for canonical, digest-bound runtime sources.
//! Callers retain canonical plans/locks and pass replacement settings as values.

use eyre::{Result, ensure};
use indexmap::IndexMap;
use pakro_index::artifact::SourceArtifact;
use pakro_store::Digest;
use url::Url;

fn validate_locator(url: &Url) -> Result<()> {
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "invalid native runtime transport locator"
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoutingKind {
    Artifact,
    Metadata,
}

fn validate_public_metadata_query(url: &Url) -> Result<()> {
    let Some(query) = url.query() else {
        return Ok(());
    };
    let mut per_page = None;
    let mut page = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| eyre::eyre!("invalid native runtime metadata pagination query"))?;
        let number = value
            .parse::<usize>()
            .map_err(|_| eyre::eyre!("invalid native runtime metadata pagination query"))?;
        ensure!(
            number > 0 && value == number.to_string(),
            "invalid native runtime metadata pagination query"
        );
        match key {
            "per_page" => {
                ensure!(
                    per_page.is_none() && number <= 100,
                    "invalid native runtime metadata pagination query"
                );
                per_page = Some(number);
            }
            "page" => {
                ensure!(
                    page.is_none(),
                    "invalid native runtime metadata pagination query"
                );
                page = Some(number);
            }
            _ => {
                return Err(eyre::eyre!(
                    "invalid native runtime metadata pagination query"
                ));
            }
        }
    }
    ensure!(
        per_page.is_some() && page.is_some(),
        "invalid native runtime metadata pagination query"
    );
    Ok(())
}

fn routed_url(
    url: Url,
    replacements: Option<&IndexMap<String, String>>,
    kind: RoutingKind,
) -> Result<Url> {
    validate_locator(&url)?;
    if kind == RoutingKind::Metadata {
        validate_public_metadata_query(&url)?;
    }
    let original = url.as_str();
    if let Some(replacements) = replacements {
        for (pattern, replacement) in replacements {
            let candidate = if let Some(pattern) = pattern.strip_prefix("regex:") {
                let Ok(regex) = regex::Regex::new(pattern) else {
                    continue;
                };
                regex.replace(original, replacement.as_str()).into_owned()
            } else if original.contains(pattern) {
                original.replace(pattern, replacement)
            } else {
                continue;
            };
            if candidate == original {
                continue;
            }
            let Ok(candidate) = Url::parse(&candidate) else {
                continue;
            };
            validate_locator(&candidate)?;
            let public_query_downgrade = kind == RoutingKind::Metadata
                && url.scheme() == "https"
                && candidate.scheme() == "http";
            if public_query_downgrade {
                ensure!(
                    candidate.query() == url.query(),
                    "native runtime metadata transport changed its public pagination query"
                );
            }
            crate::http::ensure_secure_url_replacement(
                &url,
                &candidate,
                !public_query_downgrade && (url.query().is_some() || candidate.query().is_some()),
            )?;
            return Ok(candidate);
        }
    }
    Ok(url)
}

/// Apply the existing ordered replacement semantics without logging locators or
/// patterns. Invalid regexes and non-URL replacement results are skipped, as in
/// the shared HTTP helper; a parsed but unsafe locator is always an error.
pub(crate) fn request_url(
    url: Url,
    replacements: Option<&IndexMap<String, String>>,
) -> Result<Url> {
    routed_url(url, replacements, RoutingKind::Artifact)
}

/// Only the catalog's public numeric page/per_page query can cross to an HTTP
/// mirror. The exact query must remain unchanged on a downgrade; artifact routing
/// has no such exception. Secure mirrors may add authentication queries.
pub(in super::super) fn metadata_request_url(
    url: Url,
    replacements: Option<&IndexMap<String, String>>,
) -> Result<Url> {
    routed_url(url, replacements, RoutingKind::Metadata)
}

/// The returned source is only the request locator. Its filename and expected
/// digest are copied from the canonical source; callers keep the original plan
/// for locks, provenance and runtime identity.
pub(in super::super) fn download_source(
    source: &SourceArtifact,
    replacements: Option<&IndexMap<String, String>>,
) -> Result<SourceArtifact> {
    let request = request_url(source.url().clone(), replacements)?;
    Ok(SourceArtifact::new(
        source.filename(),
        request.as_str(),
        source.digest(),
    )?)
}

/// Source routing changes metadata meaning, including rule precedence. Hash the
/// ordered mapping for cache identity; never persist its credential-bearing text.
pub(in super::super) fn mapping_fingerprint(
    replacements: Option<&IndexMap<String, String>>,
) -> Result<String> {
    let normalized = replacements.filter(|mapping| !mapping.is_empty());
    let encoded = serde_json::to_vec(&normalized).map_err(|_| {
        eyre::eyre!("native runtime transport mapping identity could not be encoded")
    })?;
    Ok(Digest::from_sha256_bytes(&encoded).hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pakro_store::Digest;

    fn original() -> Url {
        "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/runtime.tar.gz".parse().unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    #[test]
    fn absent_mapping_preserves_the_canonical_locator() {
        let url = original();
        assert_eq!(request_url(url.clone(), None).unwrap(), url);
        assert_eq!(
            request_url(url.clone(), Some(&IndexMap::new())).unwrap(),
            url
        );
    }

    #[test]
    fn string_replacement_preserves_first_match_and_replaces_all_occurrences() {
        let url: Url = "https://upstream.invalid/upstream/data".parse().unwrap();
        let routes = map(&[("upstream", "mirror"), ("mirror", "other")]);
        assert_eq!(
            request_url(url, Some(&routes)).unwrap().as_str(),
            "https://mirror.invalid/mirror/data"
        );
    }

    #[test]
    fn regex_captures_replace_only_the_first_regex_match() {
        let url: Url = "https://host.invalid/file-1/file-2".parse().unwrap();
        let routes = map(&[(r"regex:/file-(\d+)", "/item-$1")]);
        assert_eq!(
            request_url(url, Some(&routes)).unwrap().as_str(),
            "https://host.invalid/item-1/file-2"
        );
    }

    #[test]
    fn invalid_or_unchanged_rules_do_not_hide_a_later_valid_rule() {
        let url: Url = "https://host.invalid/artifact".parse().unwrap();
        let routes = map(&[
            ("regex:[", "https://unused.invalid"),
            ("https://host.invalid", "https://host.invalid"),
            ("https://host.invalid/artifact", "http://["),
            ("host.invalid", "mirror.invalid"),
        ]);
        assert_eq!(
            request_url(url, Some(&routes)).unwrap().as_str(),
            "https://mirror.invalid/artifact"
        );
    }

    #[test]
    fn artifact_fetch_route_changes_without_mutating_canonical_digest_identity() {
        let canonical = SourceArtifact::new(
            "runtime.tar.gz",
            original().as_str(),
            Digest::from_sha256_bytes(b"fixture"),
        )
        .unwrap();
        let routes = map(&[("https://github.com", "http://127.0.0.1:9999")]);
        let fetched = download_source(&canonical, Some(&routes)).unwrap();
        assert_eq!(fetched.url().scheme(), "http");
        assert_eq!(fetched.url().host_str(), Some("127.0.0.1"));
        assert_eq!(fetched.filename(), canonical.filename());
        assert_eq!(fetched.digest(), canonical.digest());
        assert_eq!(canonical.url(), &original());
    }

    #[test]
    fn userinfo_fragments_and_non_http_routes_are_rejected_without_secrets() {
        for locator in [
            "https://user:password@mirror.invalid/file",
            "https://mirror.invalid/file#secret",
            "file:///tmp/secret",
        ] {
            let routes = map(&[(
                "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/runtime.tar.gz",
                locator,
            )]);
            let error = request_url(original(), Some(&routes)).unwrap_err();
            let message = format!("{error:?} {error}");
            for secret in ["password", "secret", "mirror.invalid", "file://"] {
                assert!(!message.contains(secret), "{message}");
            }
        }
        for locator in [
            "https://user:password@mirror.invalid/file",
            "https://mirror.invalid/file#secret",
        ] {
            assert!(request_url(locator.parse().unwrap(), None).is_err());
        }
    }

    #[test]
    fn https_downgrades_reject_any_source_or_destination_query() {
        let routes = map(&[("https://github.com", "http://127.0.0.1:9999")]);
        let mut queried = original();
        queried.set_query(Some("token=hidden"));
        assert!(request_url(queried, Some(&routes)).is_err());
        let routes = map(&[(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/runtime.tar.gz",
            "http://127.0.0.1:9999/file?token=hidden",
        )]);
        let error = request_url(original(), Some(&routes)).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("hidden"));
        let routes = map(&[("https://github.com", "http://127.0.0.1:9999")]);
        assert!(request_url(original(), Some(&routes)).is_ok());
    }

    #[test]
    fn metadata_downgrade_allows_only_unchanged_public_numeric_pagination() {
        let canonical: Url =
            "https://api.github.com/repositories/162334160/releases?per_page=10&page=2"
                .parse()
                .unwrap();
        let secure_routes = map(&[(
            canonical.as_str(),
            "https://private.invalid/releases?per_page=10&page=2&token=hidden",
        )]);
        let secure = metadata_request_url(canonical.clone(), Some(&secure_routes)).unwrap();
        assert_eq!(secure.scheme(), "https");
        assert_eq!(secure.query(), Some("per_page=10&page=2&token=hidden"));
        let routes = map(&[("https://api.github.com", "http://127.0.0.1:9999")]);
        let routed = metadata_request_url(canonical.clone(), Some(&routes)).unwrap();
        assert_eq!(routed.scheme(), "http");
        assert_eq!(routed.query(), canonical.query());
        // A metadata exception must not relax digest-bound artifact routing.
        assert!(request_url(canonical.clone(), Some(&routes)).is_err());

        for query in [
            "per_page=10&page=2&token=hidden",
            "per_page=10&page=hidden",
            "per_page=10&page=02",
            "per_page=0&page=2",
            "per_page=101&page=2",
            "per_page=10&page=0",
            "per_page=10&page=2&page=3",
        ] {
            let mut source = canonical.clone();
            source.set_query(Some(query));
            let error = metadata_request_url(source, Some(&routes)).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("hidden"));
        }
        for query in [
            "per_page=10&page=2&token=hidden",
            "per_page=10&page=3",
            "per_page=20&page=2",
            "page=2&per_page=10",
        ] {
            let destination = format!("http://127.0.0.1:9999/releases?{query}");
            let routes = map(&[(canonical.as_str(), &destination)]);
            let error = metadata_request_url(canonical.clone(), Some(&routes)).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("hidden"));
        }
    }

    #[test]
    fn secure_query_routes_are_supported_without_exposing_source_debug() {
        let canonical = SourceArtifact::new(
            "runtime.tar.gz",
            original().as_str(),
            Digest::from_sha256_bytes(b"fixture"),
        )
        .unwrap();
        let routes = map(&[(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/runtime.tar.gz",
            "https://private.invalid/file?token=hidden",
        )]);
        let fetched = download_source(&canonical, Some(&routes)).unwrap();
        assert_eq!(fetched.url().query(), Some("token=hidden"));
        assert!(!format!("{fetched:?}").contains("hidden"));
        assert!(!format!("{fetched:?}").contains("private.invalid"));
    }

    #[test]
    fn mapping_fingerprint_keeps_rule_order_and_secrets_out_of_the_encoded_identity() {
        assert_eq!(
            mapping_fingerprint(None).unwrap(),
            mapping_fingerprint(Some(&IndexMap::new())).unwrap()
        );
        let routes = map(&[
            ("upstream", "https://first.invalid/a?token=hidden"),
            (
                "regex:upstream/.*",
                "https://alice:password@second.invalid/b",
            ),
        ]);
        let reverse = routes
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let encoded = mapping_fingerprint(Some(&routes)).unwrap();
        assert_ne!(encoded, mapping_fingerprint(None).unwrap());
        assert_ne!(encoded, mapping_fingerprint(Some(&reverse)).unwrap());
        assert_eq!(encoded.len(), 64);
        assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
        for secret in ["hidden", "password", "alice", "first.invalid"] {
            assert!(!encoded.contains(secret));
        }
    }
}
