mod common;

use common::{identity_for, make_tgz};
use pakro_store::{Digest, PackageIdentity, Store, gc};

fn unpack_one(store: &Store, tag: &str) -> PackageIdentity {
    let bytes = make_tgz();
    let id = identity_for(tag, "1.0.0", Digest::from_sha256_bytes(&bytes));
    store.unpack(&id, &bytes).unwrap();
    id
}

#[test]
fn add_and_remove_refs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let id = unpack_one(&store, "numpy");

    store.add_ref(&id, "/proj/b").unwrap();
    store.add_ref(&id, "/proj/a").unwrap();
    store.add_ref(&id, "/proj/a").unwrap();
    assert_eq!(
        store.refs(&id).unwrap().projects,
        vec!["/proj/a".to_string(), "/proj/b".to_string()]
    );

    store.remove_ref(&id, "/proj/a").unwrap();
    assert_eq!(
        store.refs(&id).unwrap().projects,
        vec!["/proj/b".to_string()]
    );
}

#[test]
fn refs_missing_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let id = unpack_one(&store, "numpy");
    assert!(store.refs(&id).unwrap().projects.is_empty());
}

#[test]
fn gc_removes_unreferenced_entries_and_keeps_referenced() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let kept = unpack_one(&store, "numpy");
    let dropped = unpack_one(&store, "scipy");

    store.add_ref(&kept, "/proj/a").unwrap();
    let report = gc(&store).unwrap();

    assert!(report.removed.contains(&store.entry_path(&dropped)));
    assert!(report.freed_bytes > 0);
    assert!(!store.entry_path(&dropped).exists());
    assert!(store.contains(&kept));
}

#[test]
fn gc_removes_stale_tmp_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir).unwrap();
    let stale = store.root.join(".tmp-1234-abcdef");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("partial.bin"), b"xyz").unwrap();

    let report = gc(&store).unwrap();

    assert!(!stale.exists());
    assert!(report.removed.contains(&stale));
}
