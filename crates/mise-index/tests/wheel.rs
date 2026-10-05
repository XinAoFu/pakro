use mise_index::parse_wheel_filename;

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
