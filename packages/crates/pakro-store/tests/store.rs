mod common;

use common::{identity_for, make_tgz};
use pakro_store::{Digest, Store};

#[test]
fn entry_path_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let id = identity_for("numpy", "4.2.2", Digest::from_sha256_bytes(b"x"));
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
    let id = identity_for("numpy", "4.2.2", Digest::from_sha256_bytes(&bytes));
    assert!(!store.contains(&id));

    let unpacked = store.unpack(&id, &bytes).unwrap();
    assert_eq!(
        std::fs::read_to_string(unpacked.join("pkg/hello.txt")).unwrap(),
        "hello"
    );
    assert!(store.contains(&id));
    assert!(unpacked.join(".pakro-complete").is_file());
    assert!(unpacked.join(".pakro-meta.json").is_file());

    // unpacking the same identity again is idempotent
    assert_eq!(store.unpack(&id, &bytes).unwrap(), unpacked);
}

#[test]
fn unpack_rejects_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let bytes = make_tgz();
    let id = identity_for("numpy", "4.2.2", Digest::from_sha256_bytes(b"other"));
    let err = store.unpack(&id, &bytes).unwrap_err();
    assert!(err.to_string().contains("digest mismatch"));
    assert!(!store.contains(&id));
    assert!(!store.entry_path(&id).exists());
}

#[test]
fn unpack_zip_archive() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("pkg/wheel.txt", options).unwrap();
        writer.write_all(b"wheel").unwrap();
        writer.finish().unwrap();
    }
    let buf = buf.into_inner();
    let id = identity_for("numpy", "4.2.2", Digest::from_sha256_bytes(&buf));
    let unpacked = store.unpack(&id, &buf).unwrap();
    assert!(unpacked.join("pkg/wheel.txt").is_file());
    assert!(store.contains(&id));
    assert_eq!(store.unpack(&id, &buf).unwrap(), unpacked);
}

#[test]
fn unpack_rejects_unknown_archive_format() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let bytes = b"definitely not an archive";
    let id = identity_for("numpy", "4.2.2", Digest::from_sha256_bytes(bytes));
    let err = store.unpack(&id, bytes).unwrap_err();
    assert!(err.to_string().contains("archive"));
}

#[test]
fn hardlink_tree_copies_content_not_just_names() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let pkg = src.path().join("package");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("package.json"), b"{\"name\":\"x\"}").unwrap();
    pakro_store::hardlink_tree(&pkg, &dst.path().join("x")).unwrap();
    assert_eq!(
        std::fs::read_to_string(dst.path().join("x/package.json")).unwrap(),
        "{\"name\":\"x\"}"
    );
    // the source must be untouched
    assert_eq!(
        std::fs::read_to_string(pkg.join("package.json")).unwrap(),
        "{\"name\":\"x\"}"
    );

    // re-materializing over its own hardlinks must not truncate the source:
    // fs::copy from a file onto another link to the same inode zeroes it
    pakro_store::hardlink_tree(&pkg, &dst.path().join("x")).unwrap();
    assert_eq!(
        std::fs::read_to_string(dst.path().join("x/package.json")).unwrap(),
        "{\"name\":\"x\"}"
    );
    assert_eq!(
        std::fs::read_to_string(pkg.join("package.json")).unwrap(),
        "{\"name\":\"x\"}"
    );
}
