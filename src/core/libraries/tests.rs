use super::*;

#[test]
fn r01_requested_locations_do_not_create_directories() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let cache = root.path().join("cache");
    let locations = RequestedLibraryLocations::new(data.clone(), cache.clone()).unwrap();
    assert_eq!(locations.data_root(), data);
    assert_eq!(locations.cache_root(), cache);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_requested_locations_reject_ambiguous_roots() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let cache = root.path().join("cache");
    for (data, cache) in [
        (PathBuf::from("relative/data"), cache.clone()),
        (data.clone(), PathBuf::from("relative/cache")),
        (data.clone(), data.clone()),
        (root.path().join("data/../other"), cache),
    ] {
        assert!(matches!(
            RequestedLibraryLocations::new(data, cache),
            Err(InvalidLibraryLocations)
        ));
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn r01_requested_locations_preserve_nested_descriptions() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let cache = data.join("cache");
    let locations = RequestedLibraryLocations::new(data.clone(), cache.clone()).unwrap();
    assert_eq!(locations.data_root(), data);
    assert_eq!(locations.cache_root(), cache);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
