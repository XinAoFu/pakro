use super::tests::Fixture;
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn attached() -> Fixture {
    let mut fixture = Fixture::with_limits(PrefixLimits::default());
    fixture.attach(OwnershipObserverLimits::default()).unwrap();
    fixture
}
fn finished(fixture: &mut Fixture) -> PathBuf {
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("file"), 0o644)
        .unwrap();
    file.write_all(b"actual finished bytes").unwrap();
    file.finish().unwrap();
    assert!(fixture.prefix.check_binding().is_ok());
    fixture.prefix.final_path().join("file")
}
fn root_mode_rejected(mode: u32) {
    let mut fixture = attached();
    let file = finished(&mut fixture);
    let root = fixture.prefix.final_path().to_path_buf();
    fs::set_permissions(&root, fs::Permissions::from_mode(mode)).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "nonprivate root mode was accepted"
    );
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read(file).unwrap(), b"actual finished bytes");
    assert!(matches!(
        fixture.prefix.ownership,
        OwnershipRegistration::Retained { .. }
    ));
}
#[test]
fn root_mode_0755_is_rejected_before_capture_by_the_fresh_namespace_scan() {
    root_mode_rejected(0o755);
}
#[test]
fn root_mode_02700_is_rejected_before_capture_by_the_fresh_namespace_scan() {
    root_mode_rejected(0o2700);
}
#[test]
fn hardlinked_finished_file_is_not_deleted_as_a_private_owned_output() {
    let mut fixture = attached();
    let file = finished(&mut fixture);
    let outside = tempfile::tempdir().unwrap();
    let linked = outside.path().join("linked");
    fs::hard_link(&file, &linked).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "nlink2 file was accepted for cleanup"
    );
    assert_eq!(fs::read(file).unwrap(), b"actual finished bytes");
    assert_eq!(fs::read(linked).unwrap(), b"actual finished bytes");
}
#[test]
fn privileged_finished_mode_is_rejected_before_capture() {
    let mut fixture = attached();
    let file = finished(&mut fixture);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o4644)).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "privileged mode was accepted for cleanup"
    );
    assert_eq!(fs::read(file).unwrap(), b"actual finished bytes");
}
#[test]
fn finished_file_mode_must_match_the_durable_finish_facts() {
    let mut fixture = attached();
    let file = finished(&mut fixture);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "durable finished mode mismatch was ignored"
    );
    assert!(file.exists());
}
#[test]
fn finished_file_size_must_match_the_durable_finish_facts() {
    let mut fixture = attached();
    let file = finished(&mut fixture);
    fs::write(&file, b"changed size").unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "durable finished size mismatch was ignored"
    );
    assert!(file.exists());
}
#[test]
fn created_unfinished_file_rejects_privileged_bits_without_inventing_final_stats() {
    let mut fixture = attached();
    let mut file = fixture
        .prefix
        .create_file_exclusive(Path::new("file"), 0o755)
        .unwrap();
    file.write_all(b"partial").unwrap();
    drop(file);
    let file = fixture.prefix.final_path().join("file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o4600)).unwrap();
    assert!(
        fixture.prefix.discard().is_err(),
        "unfinished private identity accepted privileged bits"
    );
    assert_eq!(fs::read(file).unwrap(), b"partial");
}
