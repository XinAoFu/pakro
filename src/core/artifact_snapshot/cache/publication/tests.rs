use super::*;
use crate::artifact_snapshot::{cached_verified_artifact, spool_verified_reader};
use pakro_store::Digest;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

const NAME: &str = "verified-data-1.0.whl";
const BYTES: &[u8] = b"known verified data without any locator authority\n";

struct Fixture {
    _owner: tempfile::TempDir,
    scratch: PathBuf,
    cache: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        Self {
            _owner: owner,
            scratch: root.join("scratch"),
            cache: root.join("cache"),
        }
    }
    fn artifact(&self, name: &str, digest: Digest) -> VerifiedArtifact {
        spool_verified_reader(
            Cursor::new(BYTES),
            name,
            digest,
            BYTES.len() as u64,
            &self.scratch,
        )
        .expect("production expected-digest owner prerequisite")
    }
    fn entry(&self, name: &str, digest: Digest) -> PathBuf {
        self.cache
            .join("artifacts")
            .join(digest.to_string())
            .join(name)
    }
    fn seed(&self, bytes: &[u8]) -> PathBuf {
        let entry = self.entry(NAME, digest());
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, bytes).unwrap();
        entry
    }
}
fn digest() -> Digest {
    Digest::from_sha256_bytes(BYTES)
}
fn contents(owner: &mut VerifiedArtifact) -> Vec<u8> {
    owner.rewind().unwrap();
    let mut bytes = Vec::new();
    owner.read_to_end(&mut bytes).unwrap();
    bytes
}
fn inventory(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, here: &Path, result: &mut Vec<PathBuf>) {
        for child in fs::read_dir(here).unwrap() {
            let child = child.unwrap().path();
            result.push(child.strip_prefix(root).unwrap().to_owned());
            if fs::symlink_metadata(&child).unwrap().is_dir() {
                walk(root, &child, result);
            }
        }
    }
    if !root.exists() {
        return Vec::new();
    }
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    paths.sort();
    paths
}

#[test]
fn both_hashes_and_independent_filenames_publish_for_offline_reuse_from_the_held_owner() {
    let f = Fixture::new();
    let tuples = [
        (NAME, digest()),
        ("another-name.tar.gz", digest()),
        (NAME, Digest::from_sha512_bytes(BYTES)),
    ];
    for (name, hash) in tuples {
        let mut owner = f.artifact(name, hash);
        owner.seek(SeekFrom::Start(7)).unwrap();
        publish_verified_artifact(&mut owner, &f.cache, BYTES.len() as u64).unwrap();
        assert_eq!(owner.stream_position().unwrap(), 0);
        assert_eq!(contents(&mut owner), BYTES);
        assert_eq!(fs::read(f.entry(name, hash)).unwrap(), BYTES);
        let mut offline = cached_verified_artifact(name, hash, &f.cache, BYTES.len() as u64)
            .unwrap()
            .expect("published data must be reusable without a locator");
        assert_eq!(offline.filename(), name);
        assert_eq!(offline.digest(), hash);
        assert_eq!(contents(&mut offline), BYTES);
    }
    let files = inventory(&f.cache)
        .into_iter()
        .filter(|relative| f.cache.join(relative).is_file())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 3);
}

#[test]
fn zero_and_size_quotas_fail_before_cache_allocation_and_return_a_rewound_owner() {
    for maximum in [0, BYTES.len() as u64 - 1] {
        for existing in [false, true] {
            let f = Fixture::new();
            let mut owner = f.artifact(NAME, digest());
            owner.seek(SeekFrom::End(-1)).unwrap();
            if existing {
                fs::create_dir(&f.cache).unwrap();
                fs::write(f.cache.join("foreign-sentinel"), b"unchanged\n").unwrap();
            }
            let before = inventory(&f.cache);
            let result = publish_verified_artifact(&mut owner, &f.cache, maximum);
            assert!(result.is_err());
            assert_eq!(owner.stream_position().unwrap(), 0);
            assert_eq!(inventory(&f.cache), before);
            if existing {
                assert_eq!(
                    fs::read(f.cache.join("foreign-sentinel")).unwrap(),
                    b"unchanged\n"
                );
            } else {
                assert!(!f.cache.exists());
            }
            assert_eq!(contents(&mut owner), BYTES);
        }
    }
}

#[test]
fn existing_correct_cache_is_strictly_reused_without_replacement_or_temporary_residue() {
    let f = Fixture::new();
    let entry = f.seed(BYTES);
    let mut actual = cached_verified_artifact(NAME, digest(), &f.cache, 1024)
        .unwrap()
        .expect("real correct cache prerequisite");
    assert_eq!(contents(&mut actual), BYTES);
    let before = fs::metadata(&entry).unwrap();
    let paths = inventory(&f.cache);
    let mut owner = f.artifact(NAME, digest());
    owner.seek(SeekFrom::End(0)).unwrap();
    publish_verified_artifact(&mut owner, &f.cache, 1024).unwrap();
    assert_eq!(owner.stream_position().unwrap(), 0);
    assert_eq!(fs::read(&entry).unwrap(), BYTES);
    assert_eq!(inventory(&f.cache), paths);
    let after = fs::metadata(&entry).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(before.permissions(), after.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }
}

#[test]
fn corrupt_existing_cache_cannot_be_overwritten_repaired_or_accepted_as_a_miss() {
    let f = Fixture::new();
    let corrupt = b"private corrupt cache body\n";
    let entry = f.seed(corrupt);
    let paths = inventory(&f.cache);
    assert!(cached_verified_artifact(NAME, digest(), &f.cache, 1024).is_err());
    let mut owner = f.artifact(NAME, digest());
    owner.seek(SeekFrom::Start(2)).unwrap();
    let error = publish_verified_artifact(&mut owner, &f.cache, 1024).unwrap_err();
    assert!(!format!("{error:#}").contains("private corrupt cache body"));
    assert_eq!(owner.stream_position().unwrap(), 0);
    assert_eq!(contents(&mut owner), BYTES);
    assert_eq!(fs::read(&entry).unwrap(), corrupt);
    assert_eq!(inventory(&f.cache), paths);
}

#[test]
fn concurrent_verified_producers_publish_one_correct_tuple_without_clobbering() {
    let f = Fixture::new();
    let barrier = Arc::new(Barrier::new(6));
    let producers = (0..6)
        .map(|position| {
            let barrier = Arc::clone(&barrier);
            let scratch = f.scratch.clone();
            let cache = f.cache.clone();
            std::thread::spawn(move || -> std::result::Result<(), String> {
                let mut owner =
                    spool_verified_reader(Cursor::new(BYTES), NAME, digest(), 1024, &scratch)
                        .unwrap();
                owner.seek(SeekFrom::Start(position)).unwrap();
                barrier.wait();
                publish_verified_artifact(&mut owner, &cache, 1024)
                    .map_err(|e| format!("{e:#}"))?;
                assert_eq!(owner.stream_position().unwrap(), 0);
                assert_eq!(contents(&mut owner), BYTES);
                Ok(())
            })
        })
        .collect::<Vec<_>>();
    let outcomes = producers
        .into_iter()
        .map(|p| p.join().unwrap())
        .collect::<Vec<_>>();
    for result in outcomes {
        assert!(result.is_ok(), "{}", result.unwrap_err());
    }
    let mut offline = cached_verified_artifact(NAME, digest(), &f.cache, 1024)
        .unwrap()
        .expect("one actual published tuple");
    assert_eq!(contents(&mut offline), BYTES);
    let files = inventory(&f.cache)
        .into_iter()
        .filter(|relative| f.cache.join(relative).is_file())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
}

#[test]
fn real_filesystem_failures_preserve_foreign_entries_and_cleanup_only_their_own_temporary() {
    for kind in ["root-file", "entry-directory"] {
        let f = Fixture::new();
        let mut owner = f.artifact(NAME, digest());
        owner.seek(SeekFrom::Start(1)).unwrap();
        let before = if kind == "root-file" {
            fs::write(&f.cache, b"foreign root file\n").unwrap();
            Vec::new()
        } else {
            let entry = f.entry(NAME, digest());
            fs::create_dir_all(&entry).unwrap();
            fs::write(entry.join("sentinel"), b"foreign directory\n").unwrap();
            inventory(&f.cache)
        };
        assert!(
            publish_verified_artifact(&mut owner, &f.cache, 1024).is_err(),
            "{kind}"
        );
        assert_eq!(owner.stream_position().unwrap(), 0);
        assert_eq!(contents(&mut owner), BYTES);
        if kind == "root-file" {
            assert_eq!(fs::read(&f.cache).unwrap(), b"foreign root file\n");
        } else {
            assert_eq!(inventory(&f.cache), before);
            assert_eq!(
                fs::read(f.entry(NAME, digest()).join("sentinel")).unwrap(),
                b"foreign directory\n"
            );
        }
    }
    // The preceding cases fail before staging. These conflicts are introduced
    // only after observing the actual fully written temporary file.
    for kind in ["late-directory", "late-corrupt", "late-correct"] {
        let f = Fixture::new();
        let mut owner = f.artifact(NAME, digest());
        owner.seek(SeekFrom::End(0)).unwrap();
        let entry = f.entry(NAME, digest());
        let mut observed = None;
        let mut expected = Vec::new();
        let mut foreign_metadata = None;
        let result = publish_with_hook(&mut owner, &f.cache, 1024, |temporary| {
            assert_eq!(fs::read(temporary).unwrap(), BYTES);
            assert!(
                !entry.exists(),
                "strict cache preflight must have observed a miss"
            );
            observed = Some(temporary.to_owned());
            match kind {
                "late-directory" => {
                    fs::create_dir(&entry).unwrap();
                    fs::write(entry.join("sentinel"), b"late foreign directory\n").unwrap();
                }
                "late-corrupt" => fs::write(&entry, b"late private corrupt cache bytes\n").unwrap(),
                "late-correct" => fs::write(&entry, BYTES).unwrap(),
                _ => unreachable!(),
            }
            foreign_metadata = Some(fs::metadata(&entry).unwrap());
            expected = inventory(&f.cache);
            expected.retain(|relative| f.cache.join(relative) != temporary);
        });
        let temporary = observed.expect("the actual post-fsync/pre-persist hook must run");
        if kind == "late-correct" {
            result
                .expect("noclobber must strictly verify the independently published correct entry");
        } else {
            let error = result.expect_err("late invalid existing entry cannot be overwritten");
            let diagnostic = format!("{error:#}");
            assert!(!diagnostic.contains("late private corrupt cache bytes"));
            if kind == "late-corrupt" {
                assert!(diagnostic.contains("digest"), "{diagnostic}");
            }
        }
        assert_eq!(owner.stream_position().unwrap(), 0);
        assert_eq!(contents(&mut owner), BYTES);
        assert!(
            !temporary.exists(),
            "owned temporary residue remained after {kind}"
        );
        assert_eq!(inventory(&f.cache), expected);
        if kind == "late-directory" {
            assert_eq!(
                fs::read(entry.join("sentinel")).unwrap(),
                b"late foreign directory\n"
            );
        } else {
            assert_eq!(
                fs::read(&entry).unwrap(),
                if kind == "late-correct" {
                    BYTES
                } else {
                    &b"late private corrupt cache bytes\n"[..]
                }
            );
        }
        let before = foreign_metadata.unwrap();
        let after = fs::metadata(&entry).unwrap();
        assert_eq!(before.permissions(), after.permissions());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                (before.dev(), before.ino(), before.mode()),
                (after.dev(), after.ino(), after.mode())
            );
        }
    }
    for correct in [true, false] {
        let f = Fixture::new();
        let mut owner = f.artifact(NAME, digest());
        let original_bytes = owner.file.metadata().unwrap().len();
        let corrupt = vec![b'x'; BYTES.len()];
        let winner_bytes = if correct { BYTES } else { corrupt.as_slice() };
        let entry = f.entry(NAME, digest());
        let mut before_live = None;
        let mut after_live = None;
        let mut verified_winner_bytes = None;
        let result = publish_with_validation_hooks(
            &mut owner,
            &f.cache,
            1024,
            |temporary| {
                assert_eq!(fs::read(temporary).unwrap(), BYTES);
                assert!(!entry.exists());
                fs::write(&entry, winner_bytes).unwrap();
            },
            |loser| {
                before_live = Some(fs::metadata(loser).ok().map(|m| m.len()));
            },
            |loser, winner| {
                after_live = Some(fs::metadata(loser).ok().map(|m| m.len()));
                verified_winner_bytes = Some(winner.file.metadata().unwrap().len());
            },
        );
        if correct {
            result.unwrap();
            assert_eq!(verified_winner_bytes, Some(original_bytes));
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("digest"));
        }
        assert_eq!(owner.file.metadata().unwrap().len(), original_bytes);
        assert_eq!(owner.stream_position().unwrap(), 0);
        // This measures live logical temporary file bytes, never RSS. The
        // original owner and actual strict winner snapshot each hold N bytes.
        assert_eq!(
            before_live,
            Some(None),
            "loser must be released before winner validation; N={original_bytes}, loser={before_live:?}, verified_winner={verified_winner_bytes:?}, after_loser={after_live:?}"
        );
        if correct {
            assert_eq!(after_live, Some(None));
        }
        assert_eq!(fs::read(entry).unwrap(), winner_bytes);
    }
}
