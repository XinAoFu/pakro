use super::*;

#[test]
fn f1_incomplete_version_has_no_effects() {
    // The gate is pure: its only input is the declaration string. There is no
    // context, handle or IO argument here, so an incomplete declaration cannot
    // reach a network fetch or a business write through this call.
    for complete in ["3.11.14", "3.15.0", "3.14.0rc2", "pypy3.11-7.3.19"] {
        let request = exact_python_request(complete)
            .unwrap_or_else(|error| panic!("complete version {complete} rejected: {error}"));
        assert_eq!(request.as_str(), complete);
    }
    for incomplete in [
        "3.11",
        "3",
        "3.11.xx",
        "3.11.*",
        "3.11.x",
        ">=3.11",
        "3.11 - 4.0",
        "latest",
        "",
    ] {
        assert!(
            matches!(
                exact_python_request(incomplete),
                Err(ExactVersionError::Incomplete(raw)) if raw == incomplete
            ),
            "incomplete declaration {incomplete:?} must be rejected before any acquisition"
        );
    }
}
