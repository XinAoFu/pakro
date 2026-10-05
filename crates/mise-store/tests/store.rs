use mise_store::{Digest, PackageIdentity, Store};

fn make_tgz() -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut builder = tar::Builder::new(&mut enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "pkg/hello.txt", &b"hello"[..])
            .unwrap();
        builder.finish().unwrap();
    }
    enc.finish().unwrap()
}

fn identity(digest: Digest) -> PackageIdentity {
    format!("python:numpy@4.2.2+python@3.10.14/macos-aarch64/{digest}")
        .parse()
        .unwrap()
}

#[test]
fn entry_path_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let id = identity(Digest::from_sha256_bytes(b"x"));
    assert_eq!(
        store.entry_path(&id),
        dir.path()
            .join("python")
            .join("numpy")
            .join("4.2.2")
            .join(id.digest.to_string())
    );
}

#[test]
fn unpack_verifies_and_extracts() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let bytes = make_tgz();
    let id = identity(Digest::from_sha256_bytes(&bytes));
    assert!(!store.contains(&id));

    let unpacked = store.unpack(&id, &bytes).unwrap();
    assert!(unpacked.join("pkg/hello.txt").is_file());
    assert!(store.contains(&id));
    assert!(unpacked.join(".mise-complete").is_file());
    assert!(unpacked.join(".mise-meta.json").is_file());

    // unpacking the same identity again is idempotent
    assert_eq!(store.unpack(&id, &bytes).unwrap(), unpacked);
}

#[test]
fn unpack_rejects_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let bytes = make_tgz();
    let id = identity(Digest::from_sha256_bytes(b"other"));
    let err = store.unpack(&id, &bytes).unwrap_err();
    assert!(err.to_string().contains("digest mismatch"));
    assert!(!store.contains(&id));
    assert!(!store.entry_path(&id).exists());
}
