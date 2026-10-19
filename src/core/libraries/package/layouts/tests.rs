use super::*;

/// A failed or damaged layout is never Ready: reservation for one key either
/// returns the existing complete layout or fails explicitly, and a failed
/// build leaves no half-published state that a later query would treat as
/// usable.
#[test]
fn f3_failed_layout_is_not_ready() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(300),
    );
    // A missing shared library root makes layout reservation fail cleanly
    // instead of fabricating a Ready layout.
    let result = reserve_layout(&context, [7; 32], [8; 32]);
    assert!(result.is_err(), "a layout without its language is never Ready");
}
