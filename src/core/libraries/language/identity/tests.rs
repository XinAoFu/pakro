use super::*;
use pakro_store::Digest;

fn limits() -> IdentityLimits {
    IdentityLimits::default()
}
fn target_input() -> TargetIdentityInput<'static> {
    TargetIdentityInput {
        os: "linux",
        arch: "x86_64",
        libc: "glibc-2.31",
        abi: "cp311",
        compatibility: [3; 32],
    }
}
fn target() -> TargetIdentity {
    TargetIdentity::new(target_input(), limits()).unwrap()
}
fn language_input(target: &TargetIdentity) -> LanguageIdentityInput<'_> {
    LanguageIdentityInput {
        implementation: "cpython",
        distribution: "python-build-standalone-20261001",
        exact_version: "3.11.14",
        target,
        input: LanguageInput::Artifact(ArtifactId::new(Digest::Sha256([4; 32]))),
        recipe: 1,
    }
}

#[test]
fn r04_target_includes_explicit_platform_abi_and_compatibility() {
    let base = target();
    let input = target_input();
    for changed in [
        TargetIdentityInput {
            os: "macos",
            ..input
        },
        TargetIdentityInput {
            arch: "aarch64",
            ..input
        },
        TargetIdentityInput {
            libc: "musl-1.2.5",
            ..input
        },
        TargetIdentityInput {
            abi: "cp311d",
            ..input
        },
        TargetIdentityInput {
            compatibility: [5; 32],
            ..input
        },
    ] {
        assert_ne!(base, TargetIdentity::new(changed, limits()).unwrap());
    }
}

#[test]
fn r04_target_missing_facts_are_rejected_without_host_defaults() {
    for input in [
        TargetIdentityInput {
            os: "",
            ..target_input()
        },
        TargetIdentityInput {
            arch: "",
            ..target_input()
        },
        TargetIdentityInput {
            libc: "",
            ..target_input()
        },
        TargetIdentityInput {
            abi: "",
            ..target_input()
        },
    ] {
        assert!(TargetIdentity::new(input, limits()).is_err());
    }
}

#[test]
fn r04_language_identity_changes_for_every_fixed_input() {
    let target = target();
    let other_target = TargetIdentity::new(
        TargetIdentityInput {
            abi: "cp311d",
            ..target_input()
        },
        limits(),
    )
    .unwrap();
    let input = language_input(&target);
    let base = LanguageKey::new(input, limits()).unwrap();
    for changed in [
        LanguageIdentityInput {
            implementation: "pypy",
            ..input
        },
        LanguageIdentityInput {
            distribution: "other-distribution",
            ..input
        },
        LanguageIdentityInput {
            exact_version: "3.11.15",
            ..input
        },
        LanguageIdentityInput {
            target: &other_target,
            ..input
        },
        LanguageIdentityInput {
            input: LanguageInput::Artifact(ArtifactId::new(Digest::Sha256([5; 32]))),
            ..input
        },
        LanguageIdentityInput { recipe: 2, ..input },
    ] {
        assert_ne!(base, LanguageKey::new(changed, limits()).unwrap());
    }
    assert_eq!(base.target(), target.fingerprint());
    assert_eq!(base.as_ref(), &base.fingerprint());
}

#[test]
fn r04_language_exact_versions_reject_unresolved_requests() {
    let target = target();
    for version in ["latest", "3.11", "3", ">=3.11.0", "3.11.*", " 3.11.14"] {
        assert!(
            LanguageKey::new(
                LanguageIdentityInput {
                    exact_version: version,
                    ..language_input(&target)
                },
                limits()
            )
            .is_err(),
            "accepted {version}"
        );
    }
    assert!(
        LanguageKey::new(
            LanguageIdentityInput {
                recipe: 0,
                ..language_input(&target)
            },
            limits()
        )
        .is_err()
    );
    for implementation in ["java", "node", "unknown"] {
        assert!(
            LanguageKey::new(
                LanguageIdentityInput {
                    implementation,
                    ..language_input(&target)
                },
                limits()
            )
            .is_err()
        );
    }
}

#[test]
fn r04_identity_fields_are_framed_without_concatenation_collisions() {
    let input = target_input();
    let left = TargetIdentity::new(
        TargetIdentityInput {
            libc: "ab",
            abi: "c",
            ..input
        },
        limits(),
    )
    .unwrap();
    let right = TargetIdentity::new(
        TargetIdentityInput {
            libc: "a",
            abi: "bc",
            ..input
        },
        limits(),
    )
    .unwrap();
    assert_ne!(left, right);
}

#[test]
fn r04_identity_encoding_version_one_has_fixed_vectors() {
    // Generated independently with Python hashlib and struct, recorded in
    // target/r04-identity-vectors.log. Framing changes require a new version.
    let target = target();
    let language = LanguageKey::new(language_input(&target), limits()).unwrap();
    assert_eq!(
        hex::encode(target.fingerprint()),
        "0c49db3b43f40436c9807f6ef540aae19d20d4151bb300d5180dd29bfbf93b27"
    );
    assert_eq!(
        hex::encode(language.fingerprint()),
        "f2ef6a066da7c95d16839e810fbf49434ced0a70ff8c618d1e186506dbff1752"
    );
}

#[test]
fn r04_identity_text_and_aggregate_limits_are_enforced() {
    assert_eq!(
        TargetIdentity::new(
            target_input(),
            IdentityLimits {
                max_field_bytes: 0,
                ..limits()
            }
        ),
        Err(IdentityError::InvalidLimits)
    );
    assert_eq!(
        TargetIdentity::new(
            target_input(),
            IdentityLimits {
                max_field_bytes: 4,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
    assert_eq!(
        TargetIdentity::new(
            target_input(),
            IdentityLimits {
                max_total_utf8_bytes: 5,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
    assert!(
        TargetIdentity::new(
            TargetIdentityInput {
                abi: "/home/user/python",
                ..target_input()
            },
            limits()
        )
        .is_err()
    );
}

#[test]
fn r04_language_identity_has_safe_names_and_no_descriptive_debug_data() {
    let target = target();
    let key = LanguageKey::new(language_input(&target), limits()).unwrap();
    for filename in [target.filename(), key.filename()] {
        assert!(
            filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
    }
    assert!(!format!("{key:?}").contains("python-build-standalone"));
    assert_ne!(key.fingerprint(), target.fingerprint());
}
