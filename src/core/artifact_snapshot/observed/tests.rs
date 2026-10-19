use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn bytes(artifact: &mut ObservedArtifact) -> Vec<u8> {
    artifact.rewind().unwrap();
    let mut result = Vec::new();
    artifact.read_to_end(&mut result).unwrap();
    result
}
struct Unreadable;
impl Read for Unreadable {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("preflight touched reader")
    }
}
#[derive(Clone)]
struct Meter(Rc<RefCell<Vec<(usize, usize)>>>);
struct Metered<'a> {
    input: &'a [u8],
    at: usize,
    meter: Meter,
}
impl Read for Metered<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.input.len() - self.at);
        self.meter.0.borrow_mut().push((output.len(), count));
        output[..count].copy_from_slice(&self.input[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

#[test]
fn known_vector_is_a_seekable_observation_with_private_debug() {
    let owner = tempfile::tempdir().unwrap();
    let mut artifact =
        spool_observed_reader(io::Cursor::new(b"abc"), "demo.whl", 3, owner.path()).unwrap();
    assert_eq!(artifact.filename(), "demo.whl");
    assert_eq!(artifact.size(), 3);
    assert_eq!(
        Digest::Sha256(artifact.observed_sha256()).hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(bytes(&mut artifact), b"abc");
    artifact.seek(SeekFrom::Start(1)).unwrap();
    let mut suffix = Vec::new();
    artifact.read_to_end(&mut suffix).unwrap();
    assert_eq!(suffix, b"bc");
    assert!(!format!("{artifact:?}").contains(owner.path().to_str().unwrap()));
    assert_eq!(
        std::fs::read_dir(owner.path()).unwrap().count(),
        0,
        "anonymous snapshot leaked a named file"
    );
}

#[test]
fn multi_chunk_observation_preserves_all_bytes_and_actual_size() {
    let owner = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..2_000_000).map(|index| (index % 251) as u8).collect();
    let mut artifact = spool_observed_reader(
        io::Cursor::new(&payload),
        "source.tar.gz",
        payload.len() as u64,
        owner.path(),
    )
    .unwrap();
    assert_eq!(artifact.size(), payload.len() as u64);
    assert_eq!(
        Digest::Sha256(artifact.observed_sha256()),
        Digest::from_sha256_bytes(&payload)
    );
    assert_eq!(bytes(&mut artifact), payload);
    assert_eq!(bytes(&mut artifact), payload);
}

#[test]
fn zero_limit_fails_before_read_or_scratch_creation() {
    let owner = tempfile::tempdir().unwrap();
    let absent = owner.path().join("absent");
    let error = spool_observed_reader(Unreadable, "demo.whl", 0, &absent).unwrap_err();
    assert!(format!("{error:#}").contains("limit"));
    assert!(!absent.exists());
}

#[test]
fn invalid_filenames_fail_before_read_or_scratch_creation() {
    let owner = tempfile::tempdir().unwrap();
    let absent = owner.path().join("absent");
    for filename in [
        "",
        ".",
        "..",
        "../escape",
        "one/two",
        "one\\two",
        "nul\0file",
        "newline\nfile",
    ] {
        let error = spool_observed_reader(Unreadable, filename, 1, &absent).unwrap_err();
        assert!(
            format!("{error:#}").contains("filename"),
            "missing filename diagnostic"
        );
        assert!(!absent.exists());
    }
}

#[test]
fn actual_limit_checks_exact_eof_and_stops_after_one_overflow_byte() {
    for overflow in [false, true] {
        let owner = tempfile::tempdir().unwrap();
        let meter = Meter(Rc::new(RefCell::new(Vec::new())));
        let input = if overflow {
            b"abcde".as_slice()
        } else {
            b"abc".as_slice()
        };
        let reader = Metered {
            input,
            at: 0,
            meter: meter.clone(),
        };
        let result = spool_observed_reader(reader, "demo.whl", 3, owner.path());
        if overflow {
            assert!(format!("{:#}", result.unwrap_err()).contains("limit"));
        } else {
            let mut artifact = result.unwrap();
            assert_eq!(bytes(&mut artifact), b"abc");
            assert_eq!(
                meter.0.borrow().last().unwrap().1,
                0,
                "exact prefix was mistaken for EOF"
            );
        }
        let mut read = 0usize;
        for (requested, count) in meter.0.borrow().iter().copied() {
            assert!(requested > 0 && requested <= 3usize.saturating_sub(read) + 1);
            read += count;
        }
        assert_eq!(read, if overflow { 4 } else { 3 });
        assert_eq!(std::fs::read_dir(owner.path()).unwrap().count(), 0);
    }
}

#[test]
fn reader_error_after_the_exact_limit_remains_an_error_and_hides_raw_text() {
    struct LateError(usize);
    impl Read for LateError {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.0 < 3 {
                let count = output.len().min(3 - self.0);
                output[..count].copy_from_slice(&b"abc"[self.0..self.0 + count]);
                self.0 += count;
                Ok(count)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "https://user:synthetic-observed-secret@example.test/?token=secret",
                ))
            }
        }
    }
    let owner = tempfile::tempdir().unwrap();
    let cache = owner.path().join("artifacts");
    std::fs::create_dir(&cache).unwrap();
    let sentinel = cache.join("existing-exact-pin");
    std::fs::write(&sentinel, b"old expected bytes").unwrap();
    let scratch = owner.path().join("scratch");
    let error = spool_observed_reader(LateError(0), "demo.whl", 3, &scratch).unwrap_err();
    let diagnostic = format!("{error:?} {error:#}");
    assert!(diagnostic.contains("ConnectionReset"));
    assert!(!diagnostic.contains("synthetic-observed-secret"));
    assert!(!diagnostic.contains("example.test"));
    assert_eq!(std::fs::read(sentinel).unwrap(), b"old expected bytes");
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn observed_bytes_survive_mutation_and_replacement_of_the_input_path() {
    let owner = tempfile::tempdir().unwrap();
    let source = owner.path().join("source");
    std::fs::write(&source, b"original bytes").unwrap();
    let scratch = owner.path().join("scratch");
    let mut artifact =
        spool_observed_reader(File::open(&source).unwrap(), "demo.whl", 64, &scratch).unwrap();
    std::fs::write(&source, b"mutated").unwrap();
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, b"replacement").unwrap();
    assert_eq!(bytes(&mut artifact), b"original bytes");
    assert_eq!(
        Digest::Sha256(artifact.observed_sha256()),
        Digest::from_sha256_bytes(b"original bytes")
    );
    assert_eq!(std::fs::read(source).unwrap(), b"replacement");
}

#[test]
fn invalid_reader_counts_fail_safely_and_u64_max_never_requests_zero_bytes() {
    struct InvalidCount;
    impl Read for InvalidCount {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            Ok(output.len() + 1)
        }
    }
    let owner = tempfile::tempdir().unwrap();
    let error = spool_observed_reader(InvalidCount, "demo.whl", 3, owner.path()).unwrap_err();
    assert!(format!("{error:#}").contains("invalid byte count"));
    let meter = Meter(Rc::new(RefCell::new(Vec::new())));
    let mut artifact = spool_observed_reader(
        Metered {
            input: b"abc",
            at: 0,
            meter: meter.clone(),
        },
        "demo.whl",
        u64::MAX,
        owner.path(),
    )
    .unwrap();
    assert_eq!(bytes(&mut artifact), b"abc");
    assert!(
        meter
            .0
            .borrow()
            .iter()
            .all(|(request, _)| *request > 0 && *request <= 64 * 1024)
    );
}
