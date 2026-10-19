use super::*;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

const NAME: &str = "demo-1.0-py3-none-any.whl";
const BYTES: &[u8] = b"private-cache-body-known-only-to-the-caller";

fn expected() -> Digest {
    Digest::from_sha256_bytes(BYTES)
}
fn entry(root: &Path, filename: &str, digest: Digest) -> PathBuf {
    // Independent, public cache-layout oracle; no fake SourceArtifact URL.
    root.join("artifacts")
        .join(digest.to_string())
        .join(filename)
}
fn seed(root: &Path, filename: &str, digest: Digest, bytes: &[u8]) -> PathBuf {
    let path = entry(root, filename, digest);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}
fn contents(artifact: &mut VerifiedArtifact) -> Vec<u8> {
    artifact.rewind().unwrap();
    let mut bytes = Vec::new();
    artifact.read_to_end(&mut bytes).unwrap();
    bytes
}
fn inventory(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, path: &Path, paths: &mut Vec<PathBuf>) {
        for child in fs::read_dir(path).unwrap() {
            let child = child.unwrap().path();
            paths.push(child.strip_prefix(root).unwrap().to_owned());
            if fs::symlink_metadata(&child).unwrap().is_dir() {
                walk(root, &child, paths);
            }
        }
    }
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    paths.sort();
    paths
}

#[test]
fn both_expected_digest_algorithms_return_owned_seekable_snapshots() {
    for digest in [expected(), Digest::from_sha512_bytes(BYTES)] {
        let root = tempfile::tempdir().unwrap();
        seed(root.path(), NAME, digest, BYTES);
        let before = inventory(root.path());
        let mut artifact = cached_verified_artifact(NAME, digest, root.path(), BYTES.len() as u64)
            .unwrap()
            .expect("actual expected-digest cache hit must return a snapshot");
        assert_eq!(artifact.filename(), NAME);
        assert_eq!(artifact.digest(), digest);
        assert_eq!(artifact.size(), BYTES.len() as u64);
        assert_eq!(artifact.stream_position().unwrap(), 0);
        assert_eq!(contents(&mut artifact), BYTES);
        assert_eq!(artifact.seek(SeekFrom::End(0)).unwrap(), BYTES.len() as u64);
        assert_eq!(contents(&mut artifact), BYTES);
        assert_eq!(
            inventory(root.path()),
            before,
            "lookup cannot publish cache entries"
        );
    }
}

#[test]
fn a_truly_missing_cache_is_none_and_does_not_create_any_directory() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing-cache-root");
    assert!(
        cached_verified_artifact(NAME, expected(), &missing, 1024)
            .unwrap()
            .is_none()
    );
    assert!(
        !missing.exists(),
        "cache miss must not allocate a scratch or artifact tree"
    );
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
fn corrupt_cache_is_an_error_and_never_repaired_or_treated_as_a_miss() {
    let root = tempfile::tempdir().unwrap();
    let corrupt = b"corrupt-cache-with-secret-body";
    let path = seed(root.path(), NAME, expected(), corrupt);
    let before = inventory(root.path());
    let error = cached_verified_artifact(NAME, expected(), root.path(), 1024).unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("digest"), "{diagnostic}");
    assert!(!diagnostic.contains("secret-body"));
    assert_eq!(fs::read(path).unwrap(), corrupt);
    assert_eq!(inventory(root.path()), before);
}

#[test]
fn exact_byte_limits_work_and_smaller_or_zero_limits_cannot_return_a_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = seed(root.path(), NAME, expected(), BYTES);
    let before = inventory(root.path());
    let mut artifact = cached_verified_artifact(NAME, expected(), root.path(), BYTES.len() as u64)
        .unwrap()
        .expect("exact limit must still observe EOF and the expected digest");
    assert_eq!(contents(&mut artifact), BYTES);
    let error = cached_verified_artifact(NAME, expected(), root.path(), BYTES.len() as u64 - 1)
        .unwrap_err();
    assert!(format!("{error:#}").contains("byte limit"));
    let missing = root.path().join("zero-limit-missing-root");
    let error = cached_verified_artifact(NAME, expected(), &missing, 0).unwrap_err();
    assert!(format!("{error:#}").contains("positive"));
    assert!(!missing.exists());
    assert_eq!(fs::read(path).unwrap(), BYTES);
    assert_eq!(inventory(root.path()), before);
}

#[cfg(unix)]
#[test]
fn nonregular_cache_leaves_are_errors_without_fifo_blocking() {
    use std::os::unix::fs::symlink;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    const PROBE_ROOT: &str = "PAKRO_CACHE_ACCESSOR_PROBE_ROOT";
    if let Some(root) = std::env::var_os(PROBE_ROOT) {
        let error = cached_verified_artifact(NAME, expected(), Path::new(&root), 1024).unwrap_err();
        assert!(format!("{error:#}").contains("regular file"));
        return;
    }
    let mut failed = Vec::new();
    for kind in ["symlink", "fifo", "directory"] {
        let root = tempfile::tempdir().unwrap();
        let path = entry(root.path(), NAME, expected());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        match kind {
            "symlink" => {
                let external = root.path().join("external");
                fs::write(&external, BYTES).unwrap();
                symlink(external, &path).unwrap();
            }
            "fifo" => {
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap()
            }
            "directory" => fs::create_dir(&path).unwrap(),
            _ => unreachable!(),
        }
        let test = format!(
            "{}::nonregular_cache_leaves_are_errors_without_fifo_blocking",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(PROBE_ROOT, root.path())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                if !status.success() {
                    failed.push(kind);
                }
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                failed.push(kind);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            fs::symlink_metadata(path).is_ok(),
            "lookup must not remove {kind}"
        );
    }
    assert!(
        failed.is_empty(),
        "wrongly accepted or blocked cache leaves: {failed:?}"
    );
}

#[cfg(unix)]
#[test]
fn actual_permission_denial_is_an_error_and_does_not_become_a_missing_cache() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = seed(root.path(), NAME, expected(), BYTES);
    fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
    let denied = match fs::File::open(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => true,
        Ok(_) => false,
        Err(error) => panic!("unexpected permission fixture error: {error:?}"),
    };
    let result = cached_verified_artifact(NAME, expected(), root.path(), 1024);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    if denied {
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains("opening artifact cache"));
    } else {
        eprintln!("effective user can read mode000; this is not permission-denial evidence");
        assert_eq!(
            contents(&mut result.unwrap().expect("readable cache must be a hit")),
            BYTES
        );
    }
    assert_eq!(fs::read(path).unwrap(), BYTES);
}

#[test]
fn a_held_snapshot_is_independent_of_later_cache_overwrite_and_replacement() {
    for replacement in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = seed(root.path(), NAME, expected(), BYTES);
        let mut held = cached_verified_artifact(NAME, expected(), root.path(), 1024)
            .unwrap()
            .expect("real owned cache snapshot prerequisite");
        if replacement {
            fs::rename(&path, root.path().join("original-cache-file")).unwrap();
        }
        fs::write(&path, b"new mutable cache bytes").unwrap();
        assert_eq!(contents(&mut held), BYTES);
        assert!(cached_verified_artifact(NAME, expected(), root.path(), 1024).is_err());
        assert_eq!(contents(&mut held), BYTES);
    }
}

#[test]
fn exact_filename_and_digest_paths_are_separate_and_invalid_requests_do_not_publish() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path(), NAME, expected(), BYTES);
    let before = inventory(root.path());
    for (filename, digest) in [
        ("other.whl", expected()),
        (NAME, Digest::from_sha512_bytes(BYTES)),
        (NAME, Digest::from_sha256_bytes(b"different digest")),
    ] {
        assert!(
            cached_verified_artifact(filename, digest, root.path(), 1024)
                .unwrap()
                .is_none()
        );
    }
    let mut artifact = cached_verified_artifact(NAME, expected(), root.path(), 1024)
        .unwrap()
        .expect("existing exact tuple remains a cache hit");
    assert_eq!(contents(&mut artifact), BYTES);
    let debug = format!("{artifact:?}");
    assert!(!debug.contains("known-only-to-the-caller"));
    assert!(!debug.contains(root.path().to_str().unwrap()));
    let missing = root.path().join("invalid-request-root");
    for name in [
        "",
        ".",
        "..",
        "../private-token.whl",
        "a/b",
        "a\\b",
        "bad\0name",
    ] {
        let error = cached_verified_artifact(name, expected(), &missing, 1024).unwrap_err();
        assert!(format!("{error:#}").contains("filename"));
        assert!(!format!("{error:#}").contains("private-token"));
        assert!(!missing.exists());
    }
    assert_eq!(inventory(root.path()), before);
}
