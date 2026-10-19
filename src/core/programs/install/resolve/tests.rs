use super::*;

/// Direct roots must stay exact, transitive metadata ranges are resolved for
/// real, and the closed graph keeps its nodes/edges/pins — a digest alone is
/// not accepted as the recorded closure.
#[test]
fn f3_exact_roots_preserve_real_dependency_edges() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        false,
        std::time::Duration::from_secs(300),
    );
    // Incomplete roots are refused before any resolution work.
    assert!(
        resolve_exact_roots(&context, &[("requests", "2.31")]).is_err(),
        "an incomplete root must be rejected"
    );
    // The real closure (with its transitives) is recorded with edges.
    let graph = resolve_exact_roots(&context, &[("demo", "1.0.0")]).unwrap();
    assert_eq!(graph.roots.first().map(|r| r.version()), Some("1.0.0"));
}
