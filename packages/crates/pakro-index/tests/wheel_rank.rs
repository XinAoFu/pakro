use pakro_index::{SupportedWheelTags, WheelTag, WheelTags, parse_wheel_filename};

fn tag(interpreter: &str, abi: &str, platform: &str) -> WheelTag {
    WheelTag::new(interpreter, abi, platform).unwrap()
}

fn supported() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![
        tag("cp310", "cp310", "manylinux_2_17_x86_64"),
        tag("cp39", "abi3", "manylinux_2_17_x86_64"),
        tag("py3", "none", "any"),
    ])
    .unwrap()
}

#[test]
fn explicit_order_is_retained_and_no_host_defaults_are_added() {
    let input = vec![
        tag("py3", "none", "any"),
        tag("pp310", "pypy310_pp73", "linux_x86_64"),
    ];
    let actual = SupportedWheelTags::new(input.clone()).unwrap();
    assert_eq!(actual.ordered(), input);
    assert_eq!(actual.ordered()[1].interpreter(), "pp310");
    assert_eq!(actual.ordered()[1].abi(), "pypy310_pp73");
    assert_eq!(actual.ordered()[1].platform(), "linux_x86_64");
}

#[test]
fn normalized_duplicate_support_entries_are_rejected() {
    let result = SupportedWheelTags::new(vec![
        tag("CP310", "CP310", "ANY"),
        tag("cp310", "cp310", "any"),
    ]);
    assert!(result.is_err());
}

#[test]
fn empty_support_list_is_a_valid_explicit_no_support_state() {
    let empty = SupportedWheelTags::new(vec![]).unwrap();
    let wheel = parse_wheel_filename("demo-1.0-py3-none-any.whl").unwrap();
    assert_eq!(empty.best_rank(&wheel.tags).unwrap(), None);
}

#[test]
fn compressed_tag_sets_choose_smallest_support_position() {
    let wheel = parse_wheel_filename(
        "demo-1.0-py3.cp39.cp310-none.abi3.cp310-any.manylinux_2_17_x86_64.whl",
    )
    .unwrap();
    assert_eq!(supported().best_rank(&wheel.tags).unwrap(), Some(0));
    let pure = parse_wheel_filename("demo-1.0-py3-none-any.whl").unwrap();
    assert_eq!(supported().best_rank(&pure.tags).unwrap(), Some(2));
}

#[test]
fn unsupported_full_tuple_cannot_be_synthesized_from_partial_matches() {
    let explicit = SupportedWheelTags::new(vec![
        tag("cp310", "cp310", "any"),
        tag("py3", "none", "manylinux_2_17_x86_64"),
    ])
    .unwrap();
    let wheel = parse_wheel_filename("demo-1.0-cp310-none-any.whl").unwrap();
    assert_eq!(explicit.best_rank(&wheel.tags).unwrap(), None);
}

#[test]
fn unknown_but_valid_abi_atom_is_accepted_only_when_explicitly_listed() {
    let wheel = parse_wheel_filename("demo-1.0-cp315-abi3t-any.whl").unwrap();
    assert_eq!(supported().best_rank(&wheel.tags).unwrap(), None);
    let explicit = SupportedWheelTags::new(vec![tag("cp315", "abi3t", "any")]).unwrap();
    assert_eq!(explicit.best_rank(&wheel.tags).unwrap(), Some(0));
    assert_eq!(
        tag("not_version", "unknown_abi", "any").interpreter(),
        "not_version"
    );
    assert_eq!(tag("_private", "none", "any").interpreter(), "_private");
}

#[test]
fn public_handwritten_wheel_tags_must_be_valid_even_with_empty_support() {
    let empty = SupportedWheelTags::new(vec![]).unwrap();
    let mut malformed = vec![
        WheelTags {
            python: vec![],
            abi: vec!["none".into()],
            platform: vec!["any".into()],
        },
        WheelTags {
            python: vec!["py3".into()],
            abi: vec![],
            platform: vec!["any".into()],
        },
        WheelTags {
            python: vec!["py3".into()],
            abi: vec!["none".into()],
            platform: vec![],
        },
    ];
    for (python, abi, platform) in [
        ("", "none", "any"),
        ("3py", "none", "any"),
        ("py3.cp310", "none", "any"),
        ("py3", "n one", "any"),
        ("py3", "none", "../any"),
        ("py3", "none", "any\\bad"),
        ("py3", "none", "任意"),
    ] {
        malformed.push(WheelTags {
            python: vec![python.into()],
            abi: vec![abi.into()],
            platform: vec![platform.into()],
        });
    }
    for tags in malformed {
        assert!(empty.best_rank(&tags).is_err(), "{tags:?}");
        assert!(supported().best_rank(&tags).is_err(), "{tags:?}");
        assert!(empty.plan_rank(&tags).is_err(), "{tags:?}");
        assert!(supported().plan_rank(&tags).is_err(), "{tags:?}");
    }
}

#[test]
fn manual_set_order_duplicate_and_case_do_not_change_rank() {
    let tags = WheelTags {
        python: vec!["PY3".into(), "cp310".into(), "PY3".into()],
        abi: vec!["none".into(), "CP310".into(), "none".into()],
        platform: vec!["ANY".into(), "manylinux_2_17_x86_64".into(), "ANY".into()],
    };
    assert_eq!(supported().best_rank(&tags).unwrap(), Some(0));
}

#[test]
fn s4_rank_plan_matches_the_ordered_scan_for_all_small_compressed_sets() {
    let ordered = vec![
        tag("py3", "none", "any"),
        tag("cp310", "abi3", "macosx_11_0_arm64"),
        tag("cp311", "cp311", "macosx_11_0_arm64"),
        tag("cp311", "none", "linux_x86_64"),
    ];
    let target = SupportedWheelTags::new(ordered.clone()).unwrap();
    let subsets = |values: [&str; 3], mask: usize| {
        let mut result = values
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1usize << *bit) != 0)
            .map(|(_, value)| value.to_owned())
            .collect::<Vec<_>>();
        result.push(result[0].clone());
        result.reverse();
        result
    };
    for python in 1..8 {
        for abi in 1..8 {
            for platform in 1..8 {
                let tags = WheelTags {
                    python: subsets(["CP311", "PY3", "cp310"], python),
                    abi: subsets(["CP311", "NONE", "abi3"], abi),
                    platform: subsets(["MACOSX_11_0_ARM64", "ANY", "linux_x86_64"], platform),
                };
                let contains = |field: &[String], value: &str| {
                    field.iter().any(|field| field.eq_ignore_ascii_case(value))
                };
                let expected = ordered.iter().position(|tag| {
                    contains(&tags.python, tag.interpreter())
                        && contains(&tags.abi, tag.abi())
                        && contains(&tags.platform, tag.platform())
                });
                assert_eq!(target.plan_rank(&tags).unwrap().best_rank(), expected);
                assert_eq!(target.best_rank(&tags).unwrap(), expected);
            }
        }
    }
    assert_eq!(target.ordered(), ordered);
}

#[test]
fn s4_rank_plan_looks_up_a_scalar_tuple_without_scanning_the_target() {
    let target = SupportedWheelTags::new(
        (0..2048)
            .map(|index| tag("cp311", "cp311", &format!("platform_{index}")))
            .collect(),
    )
    .unwrap();
    let tags = WheelTags {
        python: vec!["cp311".into()],
        abi: vec!["cp311".into()],
        platform: vec!["platform_1337".into()],
    };
    let plan = target.plan_rank(&tags).unwrap();
    assert!(plan.work_bound() < 128);
    assert_eq!(plan.best_rank(), Some(1337));
    assert_eq!(target.best_rank(&tags).unwrap(), Some(1337));
}

#[test]
fn s4_rank_plan_bounds_huge_compressed_products_by_the_linear_fallback() {
    let tags = WheelTags {
        python: (0..1024).map(|index| format!("cp{index}")).collect(),
        abi: (0..1024).map(|index| format!("abi{index}")).collect(),
        platform: (0..1024).map(|index| format!("platform{index}")).collect(),
    };
    let target = SupportedWheelTags::new(vec![
        tag("py3", "none", "any"),
        tag("cp1023", "abi1023", "platform1023"),
    ])
    .unwrap();
    let plan = target.plan_rank(&tags).unwrap();
    assert!(plan.work_bound() < 100_000);
    assert_eq!(plan.best_rank(), Some(1));
    let unsupported = SupportedWheelTags::new(vec![tag("py3", "none", "any")]).unwrap();
    assert_eq!(unsupported.plan_rank(&tags).unwrap().best_rank(), None);
}

#[test]
fn invalid_explicit_atoms_are_rejected_without_semantic_generation() {
    for (python, abi, platform) in [
        ("", "none", "any"),
        ("3py", "none", "any"),
        ("py3.cp310", "none", "any"),
        ("py3", "n one", "any"),
        ("py3", "none", "any/else"),
    ] {
        assert!(WheelTag::new(python, abi, platform).is_err());
    }
}
