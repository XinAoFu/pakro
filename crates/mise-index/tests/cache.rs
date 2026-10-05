use std::fs::FileTimes;
use std::time::Duration;

use mise_index::cache::MetadataCache;

#[test]
fn put_get_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cache = MetadataCache::new(dir.path());
    assert!(cache.read("python", "numpy", "4.2.2").unwrap().is_none());
    cache
        .write("python", "numpy", "4.2.2", b"{\"ok\":true}")
        .unwrap();
    assert_eq!(
        cache.read("python", "numpy", "4.2.2").unwrap().unwrap(),
        b"{\"ok\":true}".to_vec()
    );
    assert!(cache.read("python", "numpy", "4.2.3").unwrap().is_none());
}

#[test]
fn fresh_within_ttl_and_stale_after() {
    let dir = tempfile::tempdir().unwrap();
    let cache = MetadataCache::new(dir.path());
    cache.write("python", "numpy", "4.2.2", b"{}").unwrap();
    let ttl = Duration::from_secs(3600);
    assert!(
        cache
            .read_with_ttl("python", "numpy", "4.2.2", ttl)
            .unwrap()
            .is_some()
    );

    // age the entry past the ttl by rewinding its mtime
    let path = dir.path().join("python").join("numpy").join("4.2.2.json");
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(7200);
    file.set_times(FileTimes::new().set_modified(old)).unwrap();
    drop(file);
    assert!(
        cache
            .read_with_ttl("python", "numpy", "4.2.2", ttl)
            .unwrap()
            .is_none()
    );
    // a plain read ignores freshness
    assert!(cache.read("python", "numpy", "4.2.2").unwrap().is_some());
}
