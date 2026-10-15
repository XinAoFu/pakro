use std::cmp::Ordering;

use pakro_index::{WheelBuildTag, parse_wheel_filename};

#[test]
fn strict_filename_rejects_malformed_fields_and_paths() {
    for filename in [
        "demo-1.0-x-cp310-cp310-any.whl",
        "demo-1.0--cp310-cp310-any.whl",
        "-1.0-py3-none-any.whl",
        "demo--py3-none-any.whl",
        "demo-not_a_version-py3-none-any.whl",
        ".demo-1.0-py3-none-any.whl",
        "demo.-1.0-py3-none-any.whl",
        "_demo-1.0-py3-none-any.whl",
        "demo_-1.0-py3-none-any.whl",
        "demo-1.0-py3..cp310-none-any.whl",
        "demo-1.0-py3-none-any/.whl",
        "demo-1.0-py3-none-any\\bad.whl",
        "demo-1.0-py 3-none-any.whl",
        "demo-1.0-3py-none-any.whl",
        "demo-1.0-2 rebuild-py3-none-any.whl",
        "demo-1.0-2\nrebuild-py3-none-any.whl",
        "demo-1.0-py3-none-any.WHL",
        "demo-1.0-py3-none-any.whl/extra",
        "demo-1.0-py3-none-any.whl\n",
        "demo-1.0-py3-none-any-extra-more.whl",
    ] {
        assert!(parse_wheel_filename(filename).is_none(), "{filename:?}");
    }
}

#[test]
fn preserves_historical_distribution_spelling_and_pep440_versions() {
    for (filename, name, version) in [
        (
            "Zope.Interface-1.0-py3-none-any.whl",
            "Zope.Interface",
            "1.0",
        ),
        ("My_Pkg-1!2.0-py3-none-any.whl", "My_Pkg", "1!2.0"),
        ("demo-1.0+cpu.1-py3-none-any.whl", "demo", "1.0+cpu.1"),
        ("demo-3.15.0rc1-py3-none-any.whl", "demo", "3.15.0rc1"),
    ] {
        let parsed = parse_wheel_filename(filename).unwrap();
        assert_eq!(parsed.name, name);
        assert_eq!(parsed.version, version);
        assert!(parsed.build.is_none());
    }
}

#[test]
fn preserves_build_original_text_and_canonical_numeric_tuple() {
    let parsed = parse_wheel_filename("demo-1.0-0002rebuild-py3-none-any.whl").unwrap();
    let build = parsed.build.unwrap();
    assert_eq!(build.raw(), "0002rebuild");
    assert_eq!(build.number(), "2");
    assert_eq!(build.suffix(), "rebuild");
}

#[test]
fn numeric_build_order_is_not_filename_order_or_machine_integer() {
    let two = WheelBuildTag::parse("2").unwrap();
    let ten = WheelBuildTag::parse("10").unwrap();
    let huge = WheelBuildTag::parse("99999999999999999999999999999999999999999999999999").unwrap();
    assert!(two < ten);
    assert!(ten < huge);
    let parsed = parse_wheel_filename(
        "demo-1.0-99999999999999999999999999999999999999999999999999-py3-none-any.whl",
    )
    .unwrap();
    assert_eq!(parsed.build, Some(huge));
}

#[test]
fn leading_zero_equality_and_order_agree_and_suffix_is_second() {
    let canonical = WheelBuildTag::parse("2a").unwrap();
    let padded = WheelBuildTag::parse("0002a").unwrap();
    assert_eq!(canonical, padded);
    assert_eq!(canonical.cmp(&padded), Ordering::Equal);
    assert!(WheelBuildTag::parse("2a").unwrap() < WheelBuildTag::parse("2b").unwrap());
    assert!(WheelBuildTag::parse("2z").unwrap() < WheelBuildTag::parse("10a").unwrap());
    assert_eq!(WheelBuildTag::parse("000").unwrap().number(), "0");
}

#[test]
fn absent_build_precedes_explicit_zero() {
    let absent = parse_wheel_filename("demo-1.0-py3-none-any.whl").unwrap();
    let zero = parse_wheel_filename("demo-1.0-0-py3-none-any.whl").unwrap();
    assert!(absent.build < zero.build);
}

#[test]
fn build_suffix_allows_unicode_and_safe_symbols_without_path_or_control() {
    for value in ["2重建", "2+sdk!", "2.abc_1"] {
        let filename = format!("demo-1.0-{value}-py3-none-any.whl");
        let build = parse_wheel_filename(&filename).unwrap().build.unwrap();
        assert_eq!(build.raw(), value);
    }
    assert!(WheelBuildTag::parse("2a").unwrap() < WheelBuildTag::parse("2重建").unwrap());
    for value in ["", "a2", "２abc", "2/a", "2\\a", "2-a", "2 a", "2\u{0000}a"] {
        assert!(WheelBuildTag::parse(value).is_none(), "{value:?}");
    }
}

#[test]
fn compressed_fields_are_canonical_sets_and_unknown_abi_atoms_are_not_guessed() {
    let parsed = parse_wheel_filename(
        "demo-1.0-PY3.cp310.py3-abi3t.none.abi3t-any.manylinux_2_17_x86_64.any.whl",
    )
    .unwrap();
    assert_eq!(parsed.tags.python, ["cp310", "py3"]);
    assert_eq!(parsed.tags.abi, ["abi3t", "none"]);
    assert_eq!(parsed.tags.platform, ["any", "manylinux_2_17_x86_64"]);
    assert!(parse_wheel_filename("demo-1.0-not_version-unknown_abi-any.whl").is_some());
    assert!(parse_wheel_filename("demo-1.0-_private-none-any.whl").is_some());
}
