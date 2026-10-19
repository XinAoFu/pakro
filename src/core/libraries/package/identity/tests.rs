use super::*;
use crate::libraries::language::identity::{
    LanguageIdentityInput, LanguageInput, TargetIdentityInput,
};

fn limits() -> IdentityLimits {
    IdentityLimits::default()
}
fn artifact(byte: u8) -> ArtifactId {
    ArtifactId::new(Digest::Sha256([byte; 32]))
}
fn target(abi: &str) -> TargetIdentity {
    TargetIdentity::new(
        TargetIdentityInput {
            os: "linux",
            arch: "x86_64",
            libc: "glibc-2.31",
            abi,
            compatibility: [3; 32],
        },
        limits(),
    )
    .unwrap()
}
fn source() -> SourceIdentity {
    SourceIdentity::new("pypi", [7; 32], limits()).unwrap()
}
fn package_input<'a>(
    target: &'a TargetIdentity,
    source: &'a SourceIdentity,
) -> PackageIdentityInput<'a> {
    PackageIdentityInput {
        name: "demo-pkg",
        exact_version: "1.2.3",
        source,
        role: PackageRole::Runtime,
        target,
        dependency_context: [8; 32],
    }
}
fn language(target: &TargetIdentity) -> LanguageKey {
    LanguageKey::new(
        LanguageIdentityInput {
            implementation: "cpython",
            distribution: "standalone-20261001",
            exact_version: "3.11.14",
            target,
            input: LanguageInput::Artifact(artifact(4)),
            recipe: 1,
        },
        limits(),
    )
    .unwrap()
}
fn build_input<'a>(
    target: &'a TargetIdentity,
    configuration: &'a BTreeMap<String, String>,
) -> BuildRequestInput<'a> {
    BuildRequestInput {
        source: artifact(9),
        host: target,
        target,
        build_dependencies: &[],
        toolchain: artifact(10),
        configuration,
        recipe: 1,
    }
}

#[test]
fn r04_artifact_algorithm_is_preserved_separately_from_output_evidence() {
    let sha256 = artifact(4);
    let sha512 = ArtifactId::new(Digest::Sha512([4; 64]));
    assert_ne!(sha256, sha512);
    assert_eq!(sha256.digest(), Digest::Sha256([4; 32]));
    assert!(sha256.filename().starts_with("sha256-"));
    assert_eq!(
        OutputDigest::new(Digest::Sha512([4; 64])).digest(),
        sha512.digest()
    );
}

#[test]
fn r04_package_normalizes_names_and_exact_versions() {
    let target = target("cp311");
    let source = source();
    let input = package_input(&target, &source);
    let canonical = PackageInstanceId::python(input, limits()).unwrap();
    let alternate = PackageInstanceId::python(
        PackageIdentityInput {
            name: "Demo_Pkg",
            exact_version: "v1.2.3",
            ..input
        },
        limits(),
    )
    .unwrap();
    assert_eq!(canonical, alternate);
    assert_eq!(canonical.target(), target.fingerprint());
    for version in ["latest", ">=1.2.3", "1.*", " 1.2.3"] {
        assert!(
            PackageInstanceId::python(
                PackageIdentityInput {
                    exact_version: version,
                    ..input
                },
                limits()
            )
            .is_err()
        );
    }
}

#[test]
fn r04_package_equal_pep440_release_spellings_share_identity() {
    let target = target("cp311");
    let source = source();
    let input = package_input(&target, &source);
    for (short, extended) in [
        ("1.2", "1.2.0"),
        ("1!1.2", "1!1.2.0.0"),
        ("1.2rc1", "1.2.0rc1"),
        ("1.2+linux", "1.2.0+linux"),
    ] {
        assert_eq!(
            PackageInstanceId::python(
                PackageIdentityInput {
                    exact_version: short,
                    ..input
                },
                limits()
            )
            .unwrap(),
            PackageInstanceId::python(
                PackageIdentityInput {
                    exact_version: extended,
                    ..input
                },
                limits()
            )
            .unwrap(),
        );
    }
}

#[test]
fn r04_package_version_normalization_charges_expanded_utf8_budget() {
    let target = target("cp311");
    let source = source();
    // "1r2" canonicalizes to "1.post2": the one-byte name plus that
    // seven-byte version needs eight bytes, despite only four input bytes.
    let input = PackageIdentityInput {
        name: "p",
        exact_version: "1r2",
        ..package_input(&target, &source)
    };
    assert_eq!(
        PackageInstanceId::python(
            input,
            IdentityLimits {
                max_total_utf8_bytes: 7,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
    let at_boundary = PackageInstanceId::python(
        input,
        IdentityLimits {
            max_total_utf8_bytes: 8,
            ..limits()
        },
    )
    .unwrap();
    assert_eq!(
        at_boundary,
        PackageInstanceId::python(
            PackageIdentityInput {
                exact_version: "1.post2",
                ..input
            },
            IdentityLimits {
                max_total_utf8_bytes: 8,
                ..limits()
            }
        )
        .unwrap()
    );
}

#[test]
fn r04_package_source_role_target_and_dependency_context_stay_distinct() {
    let target = target("cp311");
    let other_target = self::target("cp311d");
    let source = source();
    let other_source = SourceIdentity::new("private", [7; 32], limits()).unwrap();
    let input = package_input(&target, &source);
    let base = PackageInstanceId::python(input, limits()).unwrap();
    for changed in [
        PackageIdentityInput {
            name: "other",
            ..input
        },
        PackageIdentityInput {
            exact_version: "1.2.4",
            ..input
        },
        PackageIdentityInput {
            source: &other_source,
            ..input
        },
        PackageIdentityInput {
            role: PackageRole::Build,
            ..input
        },
        PackageIdentityInput {
            target: &other_target,
            ..input
        },
        PackageIdentityInput {
            dependency_context: [9; 32],
            ..input
        },
    ] {
        assert_ne!(base, PackageInstanceId::python(changed, limits()).unwrap());
    }
}

#[test]
fn r04_source_fingerprint_changes_without_treating_digest_as_authorization() {
    let original = source();
    assert_ne!(
        original,
        SourceIdentity::new("pypi", [8; 32], limits()).unwrap()
    );
    for logical in [
        "https://user:secret@example.org",
        "/local/path",
        "",
        "../local",
    ] {
        assert!(SourceIdentity::new(logical, [7; 32], limits()).is_err());
    }
}

#[test]
fn r04_build_request_tracks_fixed_inputs_not_later_outputs() {
    let target = target("cp311");
    let other_target = self::target("cp311d");
    let configuration = BTreeMap::new();
    let changed_configuration = BTreeMap::from([("optimization".into(), "2".into())]);
    let input = build_input(&target, &configuration);
    let base = BuildRequestId::new(input, limits()).unwrap();
    for changed in [
        BuildRequestInput {
            source: artifact(11),
            ..input
        },
        BuildRequestInput {
            host: &other_target,
            ..input
        },
        BuildRequestInput {
            target: &other_target,
            ..input
        },
        BuildRequestInput {
            toolchain: artifact(11),
            ..input
        },
        BuildRequestInput {
            configuration: &changed_configuration,
            ..input
        },
        BuildRequestInput { recipe: 2, ..input },
    ] {
        assert_ne!(base, BuildRequestId::new(changed, limits()).unwrap());
    }
    let outputs = [
        OutputDigest::new(Digest::Sha256([40; 32])),
        OutputDigest::new(Digest::Sha256([41; 32])),
    ];
    assert_ne!(outputs[0], outputs[1]);
    assert_eq!(base, BuildRequestId::new(input, limits()).unwrap());
}

#[test]
fn r04_build_dependency_sets_are_order_independent() {
    let target = target("cp311");
    let source = source();
    let input = package_input(&target, &source);
    let first = PackageInstanceId::python(input, limits()).unwrap();
    let second = PackageInstanceId::python(
        PackageIdentityInput {
            name: "other",
            ..input
        },
        limits(),
    )
    .unwrap();
    let configuration = BTreeMap::new();
    let build = build_input(&target, &configuration);
    let forward = BuildRequestId::new(
        BuildRequestInput {
            build_dependencies: &[first, second],
            ..build
        },
        limits(),
    )
    .unwrap();
    let reverse = BuildRequestId::new(
        BuildRequestInput {
            build_dependencies: &[second, first, first],
            ..build
        },
        limits(),
    )
    .unwrap();
    assert_eq!(forward, reverse);
    assert_ne!(forward, BuildRequestId::new(build, limits()).unwrap());
    assert_eq!(
        BuildRequestId::new(
            BuildRequestInput {
                build_dependencies: &[first, first],
                ..build
            },
            IdentityLimits {
                max_collection_items: 1,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
}

#[test]
fn r04_layout_input_identity_tracks_language_graph_target_recipe_and_builds() {
    let target = target("cp311");
    let language = language(&target);
    let configuration = BTreeMap::new();
    let build = BuildRequestId::new(build_input(&target, &configuration), limits()).unwrap();
    let changed_build = BuildRequestId::new(
        BuildRequestInput {
            source: artifact(12),
            ..build_input(&target, &configuration)
        },
        limits(),
    )
    .unwrap();
    let input = SharedLayoutInput {
        language,
        graph: [20; 32],
        build_requests: &[],
        target: &target,
        recipe: 1,
    };
    let base = SharedLayoutKey::new(input, limits()).unwrap();
    for changed in [
        SharedLayoutInput {
            graph: [21; 32],
            ..input
        },
        SharedLayoutInput {
            build_requests: &[build],
            ..input
        },
        SharedLayoutInput { recipe: 2, ..input },
    ] {
        assert_ne!(base, SharedLayoutKey::new(changed, limits()).unwrap());
    }
    let forward = SharedLayoutKey::new(
        SharedLayoutInput {
            build_requests: &[build, changed_build],
            ..input
        },
        limits(),
    )
    .unwrap();
    let reverse = SharedLayoutKey::new(
        SharedLayoutInput {
            build_requests: &[changed_build, build, build],
            ..input
        },
        limits(),
    )
    .unwrap();
    assert_eq!(forward, reverse);
    assert_eq!(
        SharedLayoutKey::new(
            SharedLayoutInput {
                build_requests: &[build, build],
                ..input
            },
            IdentityLimits {
                max_collection_items: 1,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
    assert_eq!(base.target(), target.fingerprint());
    assert_ne!(base.fingerprint(), language.fingerprint());
}

#[test]
fn r04_language_build_and_layout_target_mismatches_are_rejected() {
    let target = target("cp311");
    let other_target = self::target("cp311d");
    let configuration = BTreeMap::new();
    let build = BuildRequestId::new(build_input(&other_target, &configuration), limits()).unwrap();
    assert_eq!(
        LanguageKey::new(
            LanguageIdentityInput {
                implementation: "cpython",
                distribution: "standalone-20261001",
                exact_version: "3.11.14",
                target: &target,
                input: LanguageInput::Build(build),
                recipe: 1,
            },
            limits()
        ),
        Err(IdentityError::TargetMismatch)
    );
    let language = language(&target);
    let input = SharedLayoutInput {
        language,
        graph: [20; 32],
        build_requests: &[],
        target: &target,
        recipe: 1,
    };
    assert_eq!(
        SharedLayoutKey::new(
            SharedLayoutInput {
                target: &other_target,
                ..input
            },
            limits()
        ),
        Err(IdentityError::TargetMismatch)
    );
    assert_eq!(
        SharedLayoutKey::new(
            SharedLayoutInput {
                build_requests: &[build],
                ..input
            },
            limits()
        ),
        Err(IdentityError::TargetMismatch)
    );
}

#[test]
fn r04_build_request_and_raw_artifact_are_different_language_inputs() {
    let target = target("cp311");
    let configuration = BTreeMap::new();
    let build = BuildRequestId::new(build_input(&target, &configuration), limits()).unwrap();
    let input = LanguageIdentityInput {
        implementation: "cpython",
        distribution: "standalone-20261001",
        exact_version: "3.11.14",
        target: &target,
        input: LanguageInput::Build(build),
        recipe: 1,
    };
    let from_build = LanguageKey::new(input, limits()).unwrap();
    let from_artifact = LanguageKey::new(
        LanguageIdentityInput {
            input: LanguageInput::Artifact(ArtifactId::new(Digest::Sha256(build.fingerprint()))),
            ..input
        },
        limits(),
    )
    .unwrap();
    assert_ne!(from_build, from_artifact);
}

#[test]
fn r04_build_configuration_limits_apply_before_hashing() {
    let target = target("cp311");
    let configuration = BTreeMap::from([("optimization".into(), "2".into())]);
    assert_eq!(
        BuildRequestId::new(
            build_input(&target, &configuration),
            IdentityLimits {
                max_total_utf8_bytes: 5,
                ..limits()
            }
        ),
        Err(IdentityError::Limit)
    );
    assert_eq!(
        BuildRequestId::new(
            BuildRequestInput {
                recipe: 0,
                ..build_input(&target, &configuration)
            },
            limits()
        ),
        Err(IdentityError::InvalidField("recipe"))
    );
}

#[test]
fn r04_build_configuration_keeps_empty_values_but_rejects_paths() {
    let target = target("cp311");
    let absent = BTreeMap::new();
    let absent_id = BuildRequestId::new(build_input(&target, &absent), limits()).unwrap();
    let empty = BTreeMap::from([("flags".into(), String::new())]);
    let empty_id = BuildRequestId::new(build_input(&target, &empty), limits()).unwrap();
    assert_ne!(empty_id, absent_id);
    let fixed_flags = BTreeMap::from([("flags".into(), "-O2 -g".into())]);
    assert_ne!(
        empty_id,
        BuildRequestId::new(build_input(&target, &fixed_flags), limits()).unwrap()
    );
    for value in [
        "/tmp/prefix",
        "../include",
        "-I../include",
        "--prefix=/tmp/env",
        "C:\\prefix",
        "C:relative",
        "https://example.org/source",
        "-O2\n-g",
    ] {
        let configuration = BTreeMap::from([("flags".into(), value.into())]);
        assert_eq!(
            BuildRequestId::new(build_input(&target, &configuration), limits()),
            Err(IdentityError::InvalidField("configuration_value")),
            "accepted {value:?}"
        );
    }
}

#[test]
fn r04_shared_identity_filenames_are_safe_single_components() {
    let target = target("cp311");
    let source = source();
    let package = PackageInstanceId::python(package_input(&target, &source), limits()).unwrap();
    let configuration = BTreeMap::new();
    let build = BuildRequestId::new(build_input(&target, &configuration), limits()).unwrap();
    let layout = SharedLayoutKey::new(
        SharedLayoutInput {
            language: language(&target),
            graph: [20; 32],
            build_requests: &[build],
            target: &target,
            recipe: 1,
        },
        limits(),
    )
    .unwrap();
    for filename in [package.filename(), build.filename(), layout.filename()] {
        assert!(
            filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
    }
    assert_eq!(package.as_ref(), &package.fingerprint());
    assert_eq!(build.as_ref(), &build.fingerprint());
    assert_eq!(layout.as_ref(), &layout.fingerprint());
    assert!(!format!("{source:?}").contains("pypi"));
}
