//! Preserve archive executable/read bits while removing shared write access.
//!
//! The 0775/0664 pair reproduces the verified official PBS 3.13.16 archive.
//! The executable itself is a fixed-probe protocol fixture, not real Python.

use super::*;
use crate::libraries::language::repository::{
    LanguageInstallSpec, LanguageRepository, LanguageReservation,
};
use std::os::unix::fs::MetadataExt;

#[tokio::test]
async fn r06_real_pbs_archive_modes_are_restricted_by_the_extraction_recipe() {
    for (executable_mode, library_mode) in [(0o775, 0o664), (0o777, 0o666)] {
        let fixture = Fixture::new();
        let observation = fixture.observed(false, "3.15.0", "3.15.0", "python");
        fixture.make_archive(vec![
            Entry::File(
                "bundle/bin/python".into(),
                fixture.script(&observation),
                executable_mode,
            ),
            Entry::File(
                "bundle/lib/python3.15/marker".into(),
                b"unchanged library bytes".to_vec(),
                library_mode,
            ),
        ]);
        install(&fixture, &fixture.plan(false), false)
            .await
            .unwrap();
        assert_eq!(
            fs::metadata(fixture.install_path().join("bin/python"))
                .unwrap()
                .mode()
                & 0o7777,
            0o755,
        );
        assert_eq!(
            fs::metadata(fixture.install_path().join("lib/python3.15/marker"))
                .unwrap()
                .mode()
                & 0o7777,
            0o644,
        );
        assert_eq!(
            fs::read(fixture.install_path().join("lib/python3.15/marker")).unwrap(),
            b"unchanged library bytes",
        );
        let inventory = fixture
            .owners
            .borrow()
            .last()
            .unwrap()
            .capture_inventory()
            .unwrap();
        for (path, mode) in [("bin/python", 0o755), ("lib/python3.15/marker", 0o644)] {
            assert_eq!(
                inventory
                    .entries()
                    .iter()
                    .find(|entry| entry.relative == Path::new(path))
                    .unwrap()
                    .mode,
                mode,
                "the extraction receipt must describe the actual restricted permissions",
            );
        }
        fixture.assert_only_fixed_probe();
    }
}

#[tokio::test]
async fn r06_privileged_archive_modes_are_rejected_before_writing_or_probing() {
    for mode in [0o4775, 0o2775, 0o1775] {
        let fixture = Fixture::new();
        let observation = fixture.observed(false, "3.15.0", "3.15.0", "python");
        fixture.make_archive(vec![Entry::File(
            "bundle/bin/python".into(),
            fixture.script(&observation),
            mode,
        )]);
        let error = install(&fixture, &fixture.plan(false), false)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("privileged permission bits"),
            "{error:#}"
        );
        assert_eq!(fs::read_dir(fixture.install_path()).unwrap().count(), 0);
        assert!(!fixture.calls.exists());
    }
}

#[test]
fn r06_language_owner_itself_still_rejects_shared_write_permissions() {
    let fixture = Fixture::new();
    fixture.make_archive(Vec::new());
    let context = owned_context(&fixture, Duration::from_secs(5));
    let spec = LanguageInstallSpec::from_plan(&fixture.plan(false), false).unwrap();
    let LanguageReservation::Build(mut owner) = LanguageRepository::open_writer(&context)
        .unwrap()
        .reserve_install(&spec)
        .unwrap()
    else {
        panic!("fresh fixture must reserve an owner");
    };
    assert!(
        owner
            .create_file_exclusive(Path::new("writable-file"), 0o775)
            .is_err()
    );
    assert!(
        owner
            .create_dir(Path::new("writable-directory"), 0o775)
            .is_err()
    );
    assert_eq!(fs::read_dir(owner.final_path()).unwrap().count(), 0);
    owner.capture_inventory().unwrap();
}
