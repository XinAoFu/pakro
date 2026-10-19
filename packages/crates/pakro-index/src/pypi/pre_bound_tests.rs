use super::*;
use crate::PythonMarkerContextBuilder;

#[test]
fn requires_python_pre_bounds_use_the_explicit_full_interpreter_version() {
    let context = PythonMarkerContext::new(PythonMarkerContextBuilder {
        os_name: "posix",
        sys_platform: "darwin",
        platform_release: "25.0.0",
        platform_system: "Darwin",
        platform_version: "explicit fixture",
        platform_machine: "arm64",
        platform_python_implementation: "CPython",
        python_version: "3.11",
        python_full_version: "3.11.0rc1",
        implementation_name: "cpython",
        implementation_version: "3.11.0rc1",
    })
    .unwrap();
    assert!(python_requires_matches_for_context("<3.11.0rc2", &context).unwrap());
    assert!(!python_requires_matches_for_context("<3.11.0", &context).unwrap());
    assert!(!python_requires_matches_for_context("<3.11.0rc1", &context).unwrap());
}

#[test]
fn requires_python_arbitrary_rhs_is_not_normalized_before_comparison() {
    let version = Version::from_str("3.11.0").unwrap();
    assert!(requires_matches_version("===3.11.0", &version).unwrap());
    assert!(requires_matches_version("==03.11.0", &version).unwrap());
    assert!(!requires_matches_version("===03.11.0", &version).unwrap());
    assert!(!requires_matches_version("===3.11.0,===03.11.0", &version).unwrap());
}
