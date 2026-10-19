//! Locate a contained Python entrypoint after extracting a verified archive.

use std::io;
use std::path::{Path, PathBuf};

pub(in super::super) fn runtime_entrypoint(root: &Path) -> io::Result<PathBuf> {
    let root = root.canonicalize()?;
    if !std::fs::metadata(&root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Python runtime root is not a directory",
        ));
    }
    let bin = root.join("bin").canonicalize()?;
    if !bin.starts_with(&root) || !std::fs::metadata(&bin)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python runtime bin directory is not contained in its root",
        ));
    }
    let primary = bin.join("python");
    match std::fs::symlink_metadata(&primary) {
        Ok(_) => return contained_file(&root, &primary),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Validate the fallback before creating a relative alias. In particular,
    // a dangling or outside python3 must never turn into a usable entrypoint.
    let _ = contained_file(&root, &bin.join("python3"))?;
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink("python3", &primary);
    #[cfg(not(unix))]
    let created: io::Result<()> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Python runtime aliases require a supported POSIX platform",
    ));
    match created {
        Ok(()) => contained_file(&root, &primary),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            contained_file(&root, &primary)
        }
        Err(error) => Err(error),
    }
}

fn contained_file(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) || !std::fs::metadata(&canonical)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python runtime entrypoint is not a regular file contained in its root",
        ));
    }
    Ok(canonical)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..100 {
                let path = std::env::temp_dir().join(format!(
                    "pakro-entrypoint-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path.canonicalize().unwrap()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("creating entrypoint fixture: {error}"),
                }
            }
            panic!("entrypoint fixture names are exhausted");
        }

        fn root(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.join("bin")).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn file(path: &Path) {
        fs::write(path, b"fixture interpreter").unwrap();
    }

    #[test]
    fn existing_python_is_preferred_without_inspecting_an_unused_python3() {
        let fixture = Fixture::new();
        let root = fixture.root("runtime");
        let primary = root.join("bin/python");
        file(&primary);
        let outside = fixture.0.join("outside");
        file(&outside);
        symlink(&outside, root.join("bin/python3")).unwrap();
        assert_eq!(
            runtime_entrypoint(&root).unwrap(),
            primary.canonicalize().unwrap()
        );
        assert!(fs::symlink_metadata(primary).unwrap().is_file());
    }

    #[test]
    fn python3_gets_a_relative_alias_under_a_unicode_root() {
        let fixture = Fixture::new();
        let root = fixture.root("运行时 中文 路径");
        let target = root.join("bin/python3");
        file(&target);
        let entrypoint = runtime_entrypoint(&root).unwrap();
        assert_eq!(entrypoint, target.canonicalize().unwrap());
        assert_eq!(
            fs::read_link(root.join("bin/python")).unwrap(),
            Path::new("python3")
        );
        assert!(entrypoint.is_file());
    }

    #[test]
    fn an_existing_outside_or_dangling_alias_is_not_replaced_by_fallback() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        file(&outside);
        for (name, link) in [
            ("outside-runtime", outside),
            ("dangling-runtime", PathBuf::from("missing")),
        ] {
            let root = fixture.root(name);
            file(&root.join("bin/python3"));
            let alias = root.join("bin/python");
            symlink(&link, &alias).unwrap();
            assert!(runtime_entrypoint(&root).is_err(), "{name}");
            assert_eq!(fs::read_link(alias).unwrap(), link);
        }
    }

    #[test]
    fn fallback_target_and_bin_directory_cannot_escape_the_root() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside-interpreter");
        file(&outside);
        let root = fixture.root("outside-python3");
        symlink(&outside, root.join("bin/python3")).unwrap();
        assert!(runtime_entrypoint(&root).is_err());
        assert!(fs::symlink_metadata(root.join("bin/python")).is_err());

        let root = fixture.root("outside-bin");
        fs::remove_dir(root.join("bin")).unwrap();
        let external_bin = fixture.0.join("external-bin");
        fs::create_dir(&external_bin).unwrap();
        file(&external_bin.join("python3"));
        symlink(&external_bin, root.join("bin")).unwrap();
        assert!(runtime_entrypoint(&root).is_err());
        assert!(!external_bin.join("python").exists());
        file(&external_bin.join("python"));
        assert!(runtime_entrypoint(&root).is_err());
    }

    #[test]
    fn missing_roots_files_and_non_regular_entrypoints_are_rejected() {
        let fixture = Fixture::new();
        let missing = fixture.0.join("missing-root");
        assert!(runtime_entrypoint(&missing).is_err());
        assert!(!missing.exists());
        let root_file = fixture.0.join("root-file");
        file(&root_file);
        assert!(runtime_entrypoint(&root_file).is_err());
        let empty = fixture.root("empty-runtime");
        assert!(runtime_entrypoint(&empty).is_err());
        let bad_bin = fixture.0.join("bad-bin-runtime");
        fs::create_dir(&bad_bin).unwrap();
        file(&bad_bin.join("bin"));
        assert!(runtime_entrypoint(&bad_bin).is_err());
        let directory = fixture.root("directory-python3");
        fs::create_dir(directory.join("bin/python3")).unwrap();
        assert!(runtime_entrypoint(&directory).is_err());
        assert!(!directory.join("bin/python").exists());
        let directory = fixture.root("directory-python");
        fs::create_dir(directory.join("bin/python")).unwrap();
        file(&directory.join("bin/python3"));
        assert!(runtime_entrypoint(&directory).is_err());
    }

    #[test]
    fn an_existing_contained_alias_and_a_root_alias_are_canonicalized() {
        let fixture = Fixture::new();
        let root = fixture.root("runtime");
        let actual = root.join("bin/python3.12");
        file(&actual);
        symlink("python3.12", root.join("bin/python")).unwrap();
        let root_alias = fixture.0.join("root-alias");
        symlink(&root, &root_alias).unwrap();
        assert_eq!(
            runtime_entrypoint(&root_alias).unwrap(),
            actual.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_link(root.join("bin/python")).unwrap(),
            Path::new("python3.12")
        );
    }
}
