use mise_index::{RuntimeInfo, parse_wheel_filename, wheel_matches};
use mise_store::Platform;

fn rt(major: u32, minor: u32) -> RuntimeInfo {
    RuntimeInfo { major, minor }
}

fn plat(s: &str) -> Platform {
    s.parse().unwrap()
}

fn tags(filename: &str) -> mise_index::WheelTags {
    parse_wheel_filename(filename).unwrap().tags
}

#[test]
fn cp310_wheel_matches_cpython_3_10_only() {
    let t = tags("numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.whl");
    assert!(wheel_matches(&t, &rt(3, 10), &plat("macos-aarch64")));
    assert!(!wheel_matches(&t, &rt(3, 11), &plat("macos-aarch64")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-gnu")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("macos-x86_64")));
}

#[test]
fn universal_wheel_matches_everywhere() {
    let t = tags("pip-24.0-py3-none-any.whl");
    for p in [
        "macos-aarch64",
        "macos-x86_64",
        "linux-x86_64-gnu",
        "linux-aarch64-musl",
    ] {
        assert!(wheel_matches(&t, &rt(3, 12), &plat(p)), "platform {p}");
    }
}

#[test]
fn py2_wheel_never_matches_py3_runtime() {
    let t = tags("foo-1.0-py2-none-any.whl");
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("macos-aarch64")));
}

#[test]
fn abi3_wheel_covers_same_major_from_min_minor() {
    let t = tags("bar-2.0-cp39-abi3-macosx_11_0_arm64.whl");
    assert!(wheel_matches(&t, &rt(3, 9), &plat("macos-aarch64")));
    assert!(wheel_matches(&t, &rt(3, 10), &plat("macos-aarch64")));
    assert!(!wheel_matches(&t, &rt(3, 8), &plat("macos-aarch64")));
    assert!(!wheel_matches(&t, &rt(4, 0), &plat("macos-aarch64")));
}

#[test]
fn manylinux_wheel_matches_linux_of_same_arch() {
    let t = tags("cupy-13.3-cp310-cp310-manylinux_2_17_x86_64.whl");
    assert!(wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-gnu")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("linux-aarch64-gnu")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("macos-aarch64")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-musl")));
}

#[test]
fn musllinux_wheel_requires_musl() {
    let t = tags("orjson-3.10-cp312-cp312-musllinux_1_2_x86_64.whl");
    assert!(wheel_matches(&t, &rt(3, 12), &plat("linux-x86_64-musl")));
    assert!(!wheel_matches(&t, &rt(3, 12), &plat("linux-x86_64-gnu")));
}

#[test]
fn legacy_linux_tag_matches_gnu_linux() {
    let t = tags("baz-1.0-2-cp310-cp310-linux_x86_64.whl");
    assert!(wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-gnu")));
    assert!(!wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-musl")));
}

#[test]
fn universal2_wheel_matches_both_macos_arches() {
    let t = tags("crypto-42.0-cp310-cp310-macosx_11_0_universal2.whl");
    assert!(wheel_matches(&t, &rt(3, 10), &plat("macos-aarch64")));
    assert!(wheel_matches(&t, &rt(3, 10), &plat("macos-x86_64")));
}

#[test]
fn compressed_cp_set_with_abi3_matches_from_lowest_version_up() {
    let t = tags("bar-2.0-cp39.cp310-abi3-manylinux_2_17_x86_64.whl");
    assert!(wheel_matches(&t, &rt(3, 9), &plat("linux-x86_64-gnu")));
    assert!(wheel_matches(&t, &rt(3, 10), &plat("linux-x86_64-gnu")));
    // the abi3 floor makes 3.11 eligible even though 3.11 has no exact tag
    assert!(wheel_matches(&t, &rt(3, 11), &plat("linux-x86_64-gnu")));
    assert!(!wheel_matches(&t, &rt(3, 8), &plat("linux-x86_64-gnu")));
}

#[test]
fn parses_platform_wheel() {
    let f = parse_wheel_filename("numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.whl").unwrap();
    assert_eq!(f.name, "numpy");
    assert_eq!(f.version, "4.2.2");
    assert_eq!(f.tags.python, vec!["cp310".to_string()]);
    assert_eq!(f.tags.abi, vec!["cp310".to_string()]);
    assert_eq!(f.tags.platform, vec!["macosx_11_0_arm64".to_string()]);
}

#[test]
fn parses_universal_wheel() {
    let f = parse_wheel_filename("pip-24.0-py3-none-any.whl").unwrap();
    assert_eq!(f.name, "pip");
    assert_eq!(f.version, "24.0");
    assert_eq!(f.tags.python, vec!["py3".to_string()]);
    assert_eq!(f.tags.abi, vec!["none".to_string()]);
    assert_eq!(f.tags.platform, vec!["any".to_string()]);
}

#[test]
fn expands_compressed_tag_sets() {
    let f = parse_wheel_filename("foo-1.0-py2.py3-none-any.whl").unwrap();
    assert_eq!(f.tags.python, vec!["py2".to_string(), "py3".to_string()]);
    let f = parse_wheel_filename("bar-2.0-cp39.cp310-abi3-manylinux_2_17_x86_64.whl").unwrap();
    assert_eq!(f.tags.python, vec!["cp39".to_string(), "cp310".to_string()]);
    assert_eq!(f.tags.abi, vec!["abi3".to_string()]);
}

#[test]
fn keeps_optional_build_tag() {
    let f = parse_wheel_filename("baz-1.0-2-cp310-cp310-linux_x86_64.whl").unwrap();
    assert_eq!(f.name, "baz");
    assert_eq!(f.version, "1.0");
    assert_eq!(f.tags.python, vec!["cp310".to_string()]);
    assert_eq!(f.tags.platform, vec!["linux_x86_64".to_string()]);
}

#[test]
fn rejects_malformed_filenames() {
    assert!(parse_wheel_filename("a-1.0.0-cp310-cp310.whl").is_none());
    assert!(parse_wheel_filename("a.whl").is_none());
    assert!(parse_wheel_filename("numpy-4.2.2-cp310-cp310-macosx_11_0_arm64.tar.gz").is_none());
    assert!(parse_wheel_filename("-1.0-cp310-cp310-any.whl").is_none());
}
