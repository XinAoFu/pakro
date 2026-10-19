use super::*;

use std::collections::HashSet;

// These are validated-value fixtures, not alternative production constructors.
// They let each behavior reach its own stub before the public constructors work.
fn registry(id: &str, base: &str, credential: Option<&str>) -> PythonRegistry {
    PythonRegistry {
        id: PythonRegistryId(id.to_owned()),
        canonical_base: CanonicalRegistryUrl(Url::parse(base).unwrap()),
        credential_ref: credential.map(|value| CredentialRef(value.to_owned())),
    }
}
fn project() -> NormalizedPythonName {
    NormalizedPythonName("demo-pkg".to_owned())
}
fn id(value: &str) -> PythonRegistryId {
    PythonRegistryId(value.to_owned())
}
fn registries() -> Vec<PythonRegistry> {
    vec![
        registry(
            "internal",
            "https://internal.example/simple/",
            Some("corp-auth"),
        ),
        registry("pypi", "https://pypi.org/simple/", None),
    ]
}
fn hex_bytes(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64);
    std::array::from_fn(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
}

#[test]
fn python_names_are_validated_before_normalization() {
    for (raw, normalized) in [
        ("Demo_Pkg.Name", "demo-pkg-name"),
        ("DEMO--__..pkg", "demo-pkg"),
        ("A", "a"),
        ("0", "0"),
    ] {
        assert_eq!(NormalizedPythonName::new(raw).unwrap().as_str(), normalized);
    }
    for raw in [
        "", ".", "..", "_demo", "demo-", "../demo", "a/b", "a\\b", "café", "a\n", "a\0b",
    ] {
        assert!(matches!(
            NormalizedPythonName::new(raw),
            Err(PythonSourceError::InvalidConfiguration(_))
        ));
    }
}

#[test]
fn logical_references_accept_only_nonempty_identifier_atoms() {
    for raw in ["pypi", "Corp-Index_2.example"] {
        assert_eq!(PythonRegistryId::new(raw).unwrap().as_str(), raw);
        assert_eq!(CredentialRef::new(raw).unwrap().as_str(), raw);
    }
    for raw in [
        "",
        ".",
        "..",
        "bad/name",
        "https://private.example",
        "user:password",
        "a b",
        "x\n",
        "x\0",
    ] {
        assert!(matches!(
            PythonRegistryId::new(raw),
            Err(PythonSourceError::InvalidConfiguration(_))
        ));
        let error = CredentialRef::new(raw).unwrap_err();
        assert!(matches!(error, PythonSourceError::InvalidConfiguration(_)));
        let output = format!("{error:?} {error}");
        for secret in ["bad/name", "private.example", "user:password"] {
            assert!(!output.contains(secret), "raw reference leaked");
        }
    }
}

#[test]
fn registry_urls_preserve_directory_path_semantics() {
    for (raw, expected) in [
        (
            "HTTPS://INDEX.EXAMPLE:443/tenant/simple",
            "https://index.example/tenant/simple/",
        ),
        (
            "https://index.example/tenant/simple/",
            "https://index.example/tenant/simple/",
        ),
        (
            "http://INDEX.EXAMPLE:80/simple",
            "http://index.example/simple/",
        ),
        (
            "https://index.example:8443/simple",
            "https://index.example:8443/simple/",
        ),
        (
            "https://index.example/a/../simple",
            "https://index.example/simple/",
        ),
        (
            "https://index.example/tenant%2Fprivate/simple",
            "https://index.example/tenant%2Fprivate/simple/",
        ),
        (
            "https://index.example//simple",
            "https://index.example//simple/",
        ),
        ("http://[::1]:8080/simple", "http://[::1]:8080/simple/"),
        ("https://index.example", "https://index.example/"),
    ] {
        assert_eq!(
            CanonicalRegistryUrl::new(raw).unwrap().as_url().as_str(),
            expected
        );
    }
}

#[test]
fn registry_url_rejections_do_not_echo_the_locator() {
    for raw in [
        "https://user:synthetic-url-secret@index.example/simple/",
        "https://user@index.example/simple/",
        "https://@index.example/simple/",
        "https://index.example/simple/?token=synthetic-url-secret",
        "https://index.example/simple/?",
        "https://index.example/simple/#synthetic-url-secret",
        "https://index.example/simple/#",
        "file:///synthetic-url-secret/simple/",
        "ftp://index.example/synthetic-url-secret/",
        "https:///",
        " https://index.example/synthetic-url-secret/",
        "https://index.example/synthetic-url-secret/\n",
    ] {
        let error = CanonicalRegistryUrl::new(raw).unwrap_err();
        assert!(matches!(error, PythonSourceError::InvalidConfiguration(_)));
        let output = format!("{error:?} {error}");
        for secret in ["synthetic-url-secret", "index.example", "user@"] {
            assert!(!output.contains(secret), "locator leaked: {output}");
        }
    }
}

#[test]
fn project_urls_remain_below_the_configured_directory() {
    let registry = registry("corp", "https://index.example:8443/tenant/simple/", None);
    let url = registry.project_url(&project()).unwrap();
    assert_eq!(
        url.as_url().as_str(),
        "https://index.example:8443/tenant/simple/demo-pkg/"
    );
    assert!(!format!("{url:?}").contains("index.example"));
    assert!(!format!("{url:?}").contains("tenant"));
}

#[test]
fn registry_constructor_retains_only_validated_identity() {
    let registry = PythonRegistry::new(
        "corp",
        "HTTPS://INDEX.EXAMPLE:443/tenant-secret/simple",
        Some(CredentialRef("auth-reference-secret".to_owned())),
    )
    .unwrap();
    assert_eq!(registry.id().as_str(), "corp");
    assert_eq!(
        registry.canonical_base().as_url().as_str(),
        "https://index.example/tenant-secret/simple/"
    );
    assert_eq!(
        registry.credential_ref().unwrap().as_str(),
        "auth-reference-secret"
    );
    for output in [
        format!("{registry:?}"),
        format!("{:?}", registry.canonical_base()),
        format!("{:?}", registry.credential_ref().unwrap()),
    ] {
        for secret in ["index.example", "tenant-secret", "auth-reference-secret"] {
            assert!(
                !output.contains(secret),
                "registry details leaked: {output}"
            );
        }
    }
    assert!(matches!(
        PythonRegistry::new("../bad", "https://index.example/simple/", None),
        Err(PythonSourceError::InvalidConfiguration(_))
    ));
}

#[test]
fn auth_scope_consumes_an_application_revision() {
    let first = AuthScopeKey::from_local_revision([0x19; 32]);
    let same = AuthScopeKey::from_local_revision([0x19; 32]);
    let next = AuthScopeKey::from_local_revision([0x27; 32]);
    assert_eq!(first, same);
    assert_ne!(first, next);
    let debug = format!("{first:?}");
    assert!(!debug.contains(&"19".repeat(32)));
    assert!(!debug.contains("25, 25"));
}

#[test]
fn cache_identity_separates_every_source_dimension() {
    let base = registry("corp", "https://index.example/simple/", Some("auth-a"));
    let auth = AuthScopeKey([1; 32]);
    let mapping = TransportRulesFingerprint([2; 32]);
    let mut keys = HashSet::new();
    keys.insert(base.cache_key(&auth, &mapping, SimpleRepresentation::JsonV1));
    for alternative in [
        registry("other-id", "https://index.example/simple/", Some("auth-a")),
        registry("corp", "http://index.example/simple/", Some("auth-a")),
        registry("corp", "https://index.example:8443/simple/", Some("auth-a")),
        registry(
            "corp",
            "https://index.example/other/simple/",
            Some("auth-a"),
        ),
        registry("corp", "https://index.example/simple/", Some("auth-b")),
        registry("corp", "https://index.example/simple/", None),
    ] {
        keys.insert(alternative.cache_key(&auth, &mapping, SimpleRepresentation::JsonV1));
    }
    keys.insert(base.cache_key(
        &AuthScopeKey([3; 32]),
        &mapping,
        SimpleRepresentation::JsonV1,
    ));
    keys.insert(base.cache_key(
        &auth,
        &TransportRulesFingerprint([4; 32]),
        SimpleRepresentation::JsonV1,
    ));
    keys.insert(base.cache_key(&auth, &mapping, SimpleRepresentation::HtmlV1));
    keys.insert(base.cache_key(&auth, &mapping, SimpleRepresentation::LegacyHtml));
    assert_eq!(
        keys.len(),
        11,
        "one cache entry must not alias a different source scope"
    );
    assert_eq!(
        base.cache_key(&auth, &mapping, SimpleRepresentation::JsonV1),
        base.cache_key(&auth, &mapping, SimpleRepresentation::JsonV1)
    );
}

#[test]
fn cache_key_has_a_versioned_length_delimited_sha256_encoding() {
    // Independent golden generated from the frozen v1 field contract, not a
    // second Rust encoder that could accidentally mirror the production bug.
    let source = registry(
        "corp",
        "https://index.example/simple/",
        Some("corp-auth-ref"),
    );
    let key = source.cache_key(
        &AuthScopeKey([0x19; 32]),
        &TransportRulesFingerprint([0x23; 32]),
        SimpleRepresentation::JsonV1,
    );
    assert_eq!(
        key.as_bytes(),
        &hex_bytes("8e700927fed5da167f90a8abc79a788659c27107be98efca33480eebf8e8652f")
    );
    let ab_c = registry("ab", "https://index.example/simple/", Some("c"));
    let a_bc = registry("a", "https://index.example/simple/", Some("bc"));
    assert_ne!(
        ab_c.cache_key(
            &AuthScopeKey([0; 32]),
            &TransportRulesFingerprint([0; 32]),
            SimpleRepresentation::JsonV1
        ),
        a_bc.cache_key(
            &AuthScopeKey([0; 32]),
            &TransportRulesFingerprint([0; 32]),
            SimpleRepresentation::JsonV1
        ),
    );
}

#[test]
fn transport_fingerprint_preserves_ordered_rule_boundaries() {
    let none = transport_rules_fingerprint(None);
    let empty = transport_rules_fingerprint(Some(&[]));
    assert_eq!(none, empty);
    assert_eq!(
        none.0,
        hex_bytes("90d0308a388a2712e2b7d23a813e485c172c513b7866cb6e79b2abe68819ccb4")
    );
    let rules = [
        ("https://index.example/", "https://mirror.example/"),
        (
            "regex:^https://",
            "https://secondary.example/?token=synthetic-map-secret",
        ),
    ];
    let fingerprint = transport_rules_fingerprint(Some(&rules));
    assert_eq!(
        fingerprint.0,
        hex_bytes("4cd23c29b3af22f2f5bc02d7d8abf1e83beace3bd0b5010f1a71ddcfe8cf1d06")
    );
    assert_ne!(
        fingerprint,
        transport_rules_fingerprint(Some(&[rules[1], rules[0]]))
    );
    assert_ne!(
        transport_rules_fingerprint(Some(&[("a", "bc")])),
        transport_rules_fingerprint(Some(&[("ab", "c")]))
    );
    for output in [format!("{fingerprint:?}"), format!("{none:?}")] {
        for secret in ["synthetic-map-secret", "index.example", "secondary.example"] {
            assert!(!output.contains(secret), "rule contents leaked: {output}");
        }
    }
}

#[test]
fn default_policy_uses_only_public_pypi() {
    let registries = registries();
    let project = project();
    let mut calls = Vec::new();
    let binding = bind_python_source(
        &registries,
        &PythonSourcePolicy::default(),
        &project,
        |registry, observed| {
            calls.push(registry.id().as_str().to_owned());
            assert_eq!(observed, &project);
            assert_eq!(registry.id().as_str(), "pypi");
            Ok(ProjectPresence::Present)
        },
    )
    .unwrap();
    assert_eq!(binding.registry().id().as_str(), "pypi");
    assert_eq!(binding.project(), &project);
    assert_eq!(calls, ["pypi"]);
}

#[test]
fn explicit_binding_never_queries_another_registry() {
    for presence in [ProjectPresence::Present, ProjectPresence::Missing404] {
        let registries = registries();
        let mut calls = Vec::new();
        let result = bind_python_source(
            &registries,
            &PythonSourcePolicy::Explicit(id("internal")),
            &project(),
            |registry, _| {
                calls.push(registry.id().as_str().to_owned());
                assert_eq!(
                    registry.id().as_str(),
                    "internal",
                    "explicit source fell back"
                );
                Ok(presence)
            },
        );
        match presence {
            ProjectPresence::Present => {
                assert_eq!(result.unwrap().registry().id().as_str(), "internal")
            }
            ProjectPresence::Missing404 => {
                assert!(matches!(result, Err(PythonSourceError::ProjectMissing)))
            }
        }
        assert_eq!(calls, ["internal"]);
    }
}

#[test]
fn first_project_fallback_is_only_for_missing404() {
    let registries = registries();
    let policy = PythonSourcePolicy::FirstProject(vec![id("internal"), id("pypi")]);
    let mut calls = Vec::new();
    let binding = bind_python_source(&registries, &policy, &project(), |registry, _| {
        calls.push(registry.id().as_str().to_owned());
        Ok(if registry.id().as_str() == "internal" {
            ProjectPresence::Missing404
        } else {
            ProjectPresence::Present
        })
    })
    .unwrap();
    assert_eq!(binding.registry().id().as_str(), "pypi");
    assert_eq!(calls, ["internal", "pypi"]);
}

#[test]
fn present_project_ends_source_search() {
    // Presence has no version/ABI/file state. An eventual empty/yanked or
    // incompatible file set cannot turn Present into Missing404 in this API.
    let registries = registries();
    let policy = PythonSourcePolicy::FirstProject(vec![id("internal"), id("pypi")]);
    let mut calls = Vec::new();
    let binding = bind_python_source(&registries, &policy, &project(), |registry, _| {
        calls.push(registry.id().as_str().to_owned());
        assert_eq!(
            registry.id().as_str(),
            "internal",
            "existing project changed source"
        );
        Ok(ProjectPresence::Present)
    })
    .unwrap();
    assert_eq!(binding.registry().id().as_str(), "internal");
    assert_eq!(calls, ["internal"]);
}

#[test]
fn lookup_failures_preserve_the_original_source_failure() {
    for kind in [
        LookupFailureKind::Authentication,
        LookupFailureKind::Transport,
        LookupFailureKind::InvalidResponse,
    ] {
        let registries = registries();
        let policy = PythonSourcePolicy::FirstProject(vec![id("internal"), id("pypi")]);
        let mut calls = Vec::new();
        let result = bind_python_source(&registries, &policy, &project(), |registry, _| {
            calls.push(registry.id().as_str().to_owned());
            assert_eq!(registry.id().as_str(), "internal", "error changed source");
            Err(PythonSourceError::LookupFailed(kind))
        });
        assert!(
            matches!(result, Err(PythonSourceError::LookupFailed(observed)) if observed == kind)
        );
        assert_eq!(calls, ["internal"]);
    }
}

#[test]
fn policy_configuration_is_checked_before_lookup() {
    let normal = registries();
    let duplicate_registry = vec![normal[0].clone(), normal[0].clone(), normal[1].clone()];
    let cases = [
        (
            &normal,
            PythonSourcePolicy::Explicit(id("missing")),
            "unknown registry id",
        ),
        (
            &normal,
            PythonSourcePolicy::FirstProject(vec![]),
            "source policy is empty",
        ),
        (
            &normal,
            PythonSourcePolicy::FirstProject(vec![id("internal"), id("missing")]),
            "unknown registry id",
        ),
        (
            &normal,
            PythonSourcePolicy::FirstProject(vec![id("internal"), id("internal")]),
            "source policy repeats a registry id",
        ),
        (
            &duplicate_registry,
            PythonSourcePolicy::default(),
            "duplicate registry id",
        ),
    ];
    for (registries, policy, expected) in cases {
        let result = bind_python_source(registries, &policy, &project(), |_, _| {
            panic!("invalid source configuration reached lookup")
        });
        assert!(
            matches!(result, Err(PythonSourceError::InvalidConfiguration(reason)) if reason == expected)
        );
    }
}

#[test]
fn shared_source_order_is_complete_explicit_and_independent_of_availability() {
    let registries = registries();
    let references: Vec<_> = registries.iter().collect();
    assert_eq!(
        ordered_python_source_indices(
            &references,
            &PythonSourcePolicy::FirstProject(vec![id("pypi"), id("internal")])
        )
        .unwrap(),
        [1, 0]
    );
    assert_eq!(
        ordered_python_source_indices(&references, &PythonSourcePolicy::Explicit(id("internal")))
            .unwrap(),
        [0]
    );
    let duplicate = [&registries[0], &registries[0], &registries[1]];
    for (registries, policy, expected) in [
        (
            references.as_slice(),
            PythonSourcePolicy::FirstProject(vec![]),
            "source policy is empty",
        ),
        (
            references.as_slice(),
            PythonSourcePolicy::FirstProject(vec![id("internal"), id("missing")]),
            "unknown registry id",
        ),
        (
            references.as_slice(),
            PythonSourcePolicy::FirstProject(vec![id("internal"), id("internal")]),
            "source policy repeats a registry id",
        ),
        (
            duplicate.as_slice(),
            PythonSourcePolicy::Explicit(id("internal")),
            "duplicate registry id",
        ),
    ] {
        assert_eq!(
            ordered_python_source_indices(registries, &policy).unwrap_err(),
            PythonSourceError::InvalidConfiguration(expected)
        );
    }
}
