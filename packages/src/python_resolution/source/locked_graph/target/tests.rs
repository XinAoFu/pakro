use super::*;
use pakro_index::WheelTag;

const ENVIRONMENT: [&str; 11] = [
    "posix",
    "linux",
    "6.1.0",
    "Linux",
    "build-one",
    "aarch64",
    "CPython",
    "3.11",
    "3.11.17",
    "cpython",
    "3.11.17",
];
fn environment<'a>(values: &'a [&'a str; 11]) -> PythonMarkerEnvironment<'a> {
    PythonMarkerEnvironment {
        os_name: values[0],
        sys_platform: values[1],
        platform_release: values[2],
        platform_system: values[3],
        platform_version: values[4],
        platform_machine: values[5],
        platform_python_implementation: values[6],
        python_version: values[7],
        python_full_version: values[8],
        implementation_name: values[9],
        implementation_version: values[10],
    }
}
fn tags() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![
        WheelTag::new("cp311", "cp311", "manylinux_2_17_aarch64").unwrap(),
        WheelTag::new("py3", "none", "any").unwrap(),
    ])
    .unwrap()
}
fn frozen(
    values: &[&str; 11],
    tags: &SupportedWheelTags,
    scope: PythonMarkerScope<'_>,
) -> FrozenRegistryTarget {
    FrozenRegistryTarget::new(
        environment(values),
        tags,
        scope,
        MarkerSemantics::Current2026,
        FrozenTargetLimits::default(),
    )
    .expect("explicit valid target control")
}
#[test]
fn fingerprint_matches_an_independent_sha256_length_prefixed_oracle() {
    let target = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["Fast_Mode", "speed", "fast-mode"],
        },
    );
    // Independently computed with Python hashlib/struct.pack('>Q'), domain,
    // fixed eleven strings, actual ordered tag triples and canonical scope.
    assert_eq!(
        hex::encode(target.fingerprint()),
        "c0e58ec233b0154309866f11988ff2e04de77014f0a6608dd467c2d8c59a8c95"
    );
    assert_eq!(target.environment().python_full_version, "3.11.17");
    assert_eq!(target.semantics(), MarkerSemantics::Current2026);
}
#[test]
fn every_resolution_field_and_ordered_tag_sequence_affects_the_target() {
    let base = frozen(&ENVIRONMENT, &tags(), PythonMarkerScope::Environment);
    for (index, changed) in [
        (0, "nt"),
        (1, "linux-other"),
        (2, "6.2.0"),
        (3, "Other"),
        (4, "build-two"),
        (5, "arm64"),
    ] {
        let mut values = ENVIRONMENT;
        values[index] = changed;
        assert_ne!(
            frozen(&values, &tags(), PythonMarkerScope::Environment).fingerprint(),
            base.fingerprint()
        );
    }
    let mut language = ENVIRONMENT;
    language[7] = "3.12";
    language[8] = "3.12.5";
    language[10] = "3.12.5";
    assert_ne!(
        frozen(&language, &tags(), PythonMarkerScope::Environment).fingerprint(),
        base.fingerprint()
    );
    let mut pypy = ENVIRONMENT;
    pypy[6] = "PyPy";
    pypy[9] = "pypy";
    pypy[10] = "8.0.1";
    let pypy_target = frozen(&pypy, &tags(), PythonMarkerScope::Environment);
    assert_ne!(pypy_target.fingerprint(), base.fingerprint());
    pypy[10] = "8.0.2";
    assert_ne!(
        frozen(&pypy, &tags(), PythonMarkerScope::Environment).fingerprint(),
        pypy_target.fingerprint()
    );
    let mut ordered = tags().ordered().to_vec();
    ordered.reverse();
    let reversed = SupportedWheelTags::new(ordered).unwrap();
    assert_ne!(
        frozen(&ENVIRONMENT, &reversed, PythonMarkerScope::Environment).fingerprint(),
        base.fingerprint()
    );
    let empty = SupportedWheelTags::new(vec![]).unwrap();
    assert!(
        frozen(&ENVIRONMENT, &empty, PythonMarkerScope::Environment)
            .supported()
            .ordered()
            .is_empty()
    );
}
#[test]
fn normalized_scope_sets_are_canonical_but_scope_types_and_both_lock_sets_are_distinct() {
    let first = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["Fast_Mode", "fast.mode", "other"],
        },
    );
    let second = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["other", "fast-mode"],
        },
    );
    assert_eq!(first.fingerprint(), second.fingerprint());
    first.with_root_scope(|scope| match scope {
        PythonMarkerScope::Dependency { requested_extras } => {
            assert_eq!(requested_extras, ["fast-mode", "other"])
        }
        _ => panic!("scope type must survive"),
    });
    let empty_dependency = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &[],
        },
    );
    let env = frozen(&ENVIRONMENT, &tags(), PythonMarkerScope::Environment);
    assert_ne!(env.fingerprint(), empty_dependency.fingerprint());
    let lock = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Lock {
            selected_extras: &["Fast_Mode"],
            dependency_groups: &["DEV_Group", "dev-group"],
        },
    );
    let same = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Lock {
            selected_extras: &["fast-mode"],
            dependency_groups: &["dev.group"],
        },
    );
    assert_eq!(lock.fingerprint(), same.fingerprint());
    assert_ne!(
        lock.fingerprint(),
        frozen(
            &ENVIRONMENT,
            &tags(),
            PythonMarkerScope::Lock {
                selected_extras: &["dev-group"],
                dependency_groups: &["fast-mode"]
            }
        )
        .fingerprint()
    );
    lock.with_root_scope(|scope| match scope {
        PythonMarkerScope::Lock {
            selected_extras,
            dependency_groups,
        } => {
            assert_eq!(selected_extras, ["fast-mode"]);
            assert_eq!(dependency_groups, ["dev-group"]);
        }
        _ => panic!("lock scope must survive"),
    });
}
#[test]
fn returned_facts_are_owned_and_debug_does_not_reveal_explicit_context() {
    let target = {
        let mut values = ENVIRONMENT.map(str::to_owned);
        values[4] = "private-target-context".into();
        let borrowed = values.each_ref().map(String::as_str);
        let target = frozen(
            &borrowed,
            &tags(),
            PythonMarkerScope::Dependency {
                requested_extras: &["private-extra"],
            },
        );
        values[4] = "external caller changed this".into();
        target
    };
    assert_eq!(
        target.environment().platform_version,
        "private-target-context"
    );
    assert_eq!(target.supported().ordered().len(), 2);
    let debug = format!("{target:?}");
    assert!(!debug.contains("private-target-context") && !debug.contains("private-extra"));
}
#[test]
fn raw_scope_duplicates_and_all_utf8_fields_are_charged_before_canonical_collection() {
    // Independent totals: eleven fields = 68 bytes, both tag triples = 42;
    // scope literal "speed" = 5. Canonical dedupe must not hide raw copies.
    let exact = FrozenTargetLimits {
        max_supported_tags: 2,
        max_field_bytes: 22,
        max_total_utf8_bytes: 115,
        ..FrozenTargetLimits::default()
    };
    let target = FrozenRegistryTarget::new(
        environment(&ENVIRONMENT),
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["speed"],
        },
        MarkerSemantics::Current2026,
        exact,
    )
    .unwrap();
    assert_eq!(target.supported().ordered().len(), 2);
    for limits in [
        FrozenTargetLimits {
            max_supported_tags: 1,
            ..exact
        },
        FrozenTargetLimits {
            max_field_bytes: 21,
            ..exact
        },
        FrozenTargetLimits {
            max_total_utf8_bytes: 114,
            ..exact
        },
    ] {
        assert_eq!(
            FrozenRegistryTarget::new(
                environment(&ENVIRONMENT),
                &tags(),
                PythonMarkerScope::Dependency {
                    requested_extras: &["speed"]
                },
                MarkerSemantics::Current2026,
                limits
            )
            .unwrap_err(),
            LockedGraphError::Limit
        );
    }
    let set_limit = FrozenTargetLimits {
        markers: PythonMarkerLimits {
            max_set_entries: 1,
            ..PythonMarkerLimits::default()
        },
        ..FrozenTargetLimits::default()
    };
    FrozenRegistryTarget::new(
        environment(&ENVIRONMENT),
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["Speed"],
        },
        MarkerSemantics::Current2026,
        set_limit,
    )
    .unwrap();
    assert_eq!(
        FrozenRegistryTarget::new(
            environment(&ENVIRONMENT),
            &tags(),
            PythonMarkerScope::Dependency {
                requested_extras: &["Speed", "speed"]
            },
            MarkerSemantics::Current2026,
            set_limit
        )
        .unwrap_err(),
        LockedGraphError::Limit
    );
}
#[test]
fn invalid_limits_context_and_scope_fail_without_echoing_or_host_repair() {
    frozen(&ENVIRONMENT, &tags(), PythonMarkerScope::Environment);
    let mut wrong = ENVIRONMENT;
    wrong[7] = "3.12";
    assert_eq!(
        FrozenRegistryTarget::new(
            environment(&wrong),
            &tags(),
            PythonMarkerScope::Environment,
            MarkerSemantics::Current2026,
            FrozenTargetLimits::default()
        )
        .unwrap_err(),
        LockedGraphError::InvalidTarget
    );
    for scope in [
        PythonMarkerScope::Dependency {
            requested_extras: &["private invalid extra"],
        },
        PythonMarkerScope::Lock {
            selected_extras: &[],
            dependency_groups: &["private invalid group"],
        },
    ] {
        let error = FrozenRegistryTarget::new(
            environment(&ENVIRONMENT),
            &tags(),
            scope,
            MarkerSemantics::Current2026,
            FrozenTargetLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error, LockedGraphError::InvalidTarget);
        assert!(!format!("{error:?} {error}").contains("private invalid"));
    }
    for limits in [
        FrozenTargetLimits {
            max_supported_tags: 0,
            ..FrozenTargetLimits::default()
        },
        FrozenTargetLimits {
            max_field_bytes: 0,
            ..FrozenTargetLimits::default()
        },
        FrozenTargetLimits {
            max_total_utf8_bytes: 0,
            ..FrozenTargetLimits::default()
        },
        FrozenTargetLimits {
            markers: PythonMarkerLimits {
                max_nodes: 0,
                ..PythonMarkerLimits::default()
            },
            ..FrozenTargetLimits::default()
        },
    ] {
        assert_eq!(
            FrozenRegistryTarget::new(
                environment(&ENVIRONMENT),
                &tags(),
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                limits
            )
            .unwrap_err(),
            LockedGraphError::InvalidLimits
        );
    }
}

#[test]
fn an_owned_target_cannot_skip_tighter_scope_or_environment_limits() {
    let target = frozen(
        &ENVIRONMENT,
        &tags(),
        PythonMarkerScope::Dependency {
            requested_extras: &["fast", "slow"],
        },
    );
    target.check_limits(FrozenTargetLimits::default()).unwrap();
    assert_eq!(
        target
            .check_limits(FrozenTargetLimits {
                markers: PythonMarkerLimits {
                    max_set_entries: 1,
                    ..PythonMarkerLimits::default()
                },
                ..FrozenTargetLimits::default()
            })
            .unwrap_err(),
        LockedGraphError::Limit
    );
    assert_eq!(
        target
            .check_limits(FrozenTargetLimits {
                markers: PythonMarkerLimits {
                    max_environment_bytes: 75,
                    ..PythonMarkerLimits::default()
                },
                ..FrozenTargetLimits::default()
            })
            .unwrap_err(),
        LockedGraphError::Limit
    );
}

#[test]
fn an_owned_target_is_rebounded_before_its_data_is_cloned_for_another_request() {
    let target = frozen(&ENVIRONMENT, &tags(), PythonMarkerScope::Environment);
    let exact = FrozenTargetLimits {
        max_supported_tags: 2,
        max_field_bytes: 22,
        max_total_utf8_bytes: 110,
        ..FrozenTargetLimits::default()
    };
    target.check_limits(exact).unwrap();
    for limits in [
        FrozenTargetLimits {
            max_supported_tags: 1,
            ..exact
        },
        FrozenTargetLimits {
            max_field_bytes: 21,
            ..exact
        },
        FrozenTargetLimits {
            max_total_utf8_bytes: 109,
            ..exact
        },
    ] {
        assert_eq!(
            target.check_limits(limits).unwrap_err(),
            LockedGraphError::Limit
        );
    }
}
