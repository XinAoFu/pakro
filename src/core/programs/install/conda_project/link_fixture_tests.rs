//! Assertions for the retained macOS ARM64 Conda archives used by the mixed CLI test.
use std::path::Path;

/// Call with the selected layout's `bin/python`, after installation or replay.
/// The link owners and their targets are separate original Conda packages:
/// libblas/libcblas/liblapack -> libopenblas, and _openmp_mutex -> llvm-openmp.
pub(super) fn assert_real_cross_package_links(executable: &Path) {
    let layout = executable
        .parent()
        .and_then(Path::parent)
        .expect("selected Python must be inside a layout bin directory")
        .canonicalize()
        .expect("selected layout must exist");
    let links = [
        ("lib/libblas.3.dylib", "libopenblas.0.dylib"),
        ("lib/libblas.dylib", "libopenblas.0.dylib"),
        ("lib/libcblas.3.dylib", "libopenblas.0.dylib"),
        ("lib/libcblas.dylib", "libopenblas.0.dylib"),
        ("lib/liblapack.3.dylib", "libopenblas.0.dylib"),
        ("lib/liblapack.dylib", "libopenblas.0.dylib"),
        ("lib/libgomp.1.dylib", "libomp.dylib"),
        ("lib/libgomp.dylib", "libomp.dylib"),
    ];
    for (relative, target) in links {
        let link = layout.join(relative);
        assert_eq!(
            std::fs::read_link(&link).expect("original Conda member must remain a symlink"),
            Path::new(target),
            "unexpected target for {}",
            link.display()
        );
        let expected = layout.join("lib").join(target);
        let resolved = link
            .canonicalize()
            .expect("cross-package link must resolve");
        assert_eq!(resolved, expected.canonicalize().unwrap());
        assert!(
            resolved.starts_with(&layout),
            "link leaves the controlled layout"
        );
        let content = std::fs::read(&link).expect("linked library must be readable");
        assert!(
            !content.is_empty(),
            "linked library must contain its original payload"
        );
        assert_eq!(content, std::fs::read(expected).unwrap());
    }
}
