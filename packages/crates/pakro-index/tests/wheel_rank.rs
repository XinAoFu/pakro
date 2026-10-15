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
