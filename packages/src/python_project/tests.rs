use super::*;
use crate::packages::python_declarations::{PythonDeclarationOrigin, PythonMetadataWorkerReason};
use std::fs;
use std::time::Duration;

const BASIC: &str = "[project]\nname='app'\nversion='1.0'\n";
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn open(path: &Path) -> BoundPythonProjectFile {
    BoundPythonProjectFile::open(path, Default::default(), deadline()).unwrap()
}
fn selection<'a>(extras: &'a [&'a str], groups: &'a [&'a str]) -> PythonProjectSelection<'a> {
    PythonProjectSelection {
        include_project_dependencies: true,
        extras,
        groups,
    }
}

#[test]
fn actual_file_selection_retains_raw_ast_and_each_marker_origin_without_evaluation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    let raw = format!(
        "{BASIC}dependencies=[\"direct @ https://private.invalid/a?ticket=PRIVATE_SENTINEL ; os_name == 'posix'\"]\n[project.optional-dependencies]\nui=[\"project-only; extra == 'ui'\"]\n[dependency-groups]\ndev=[\"group-only; extra == 'ui'\"]\n"
    );
    // Establish the existing pure parser/selector setup independently of binding.
    let prelude =
        crate::packages::python_declarations::parse_pyproject(&raw, Default::default()).unwrap();
    assert_eq!(
        prelude
            .select(selection(&["ui"], &["dev"]), Default::default())
            .unwrap()
            .known()
            .requirements()
            .len(),
        3
    );
    fs::write(&path, raw).unwrap();
    let owner = open(&path);
    assert_eq!(owner.parsed().project().unwrap().name().as_str(), "app");
    let selected = owner
        .select(selection(&["ui"], &["dev"]), Default::default())
        .unwrap();
    selected.recheck_for(&owner).unwrap();
    let declarations = selected.declarations().known().requirements();
    assert_eq!(declarations.len(), 3);
    assert_eq!(
        declarations[0]
            .syntax()
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "https://private.invalid/a?ticket=PRIVATE_SENTINEL"
    );
    assert!(matches!(
        declarations[0].origin(),
        PythonDeclarationOrigin::Project { index: 0 }
    ));
    assert!(
        matches!(declarations[1].origin(), PythonDeclarationOrigin::Extra { name, index: 0 } if name == "ui")
    );
    assert!(
        matches!(declarations[2].origin(), PythonDeclarationOrigin::Group { name, index: 0 } if name == "dev")
    );
    assert_eq!(
        declarations[1].syntax().raw_for_semantic_resolution(),
        "project-only; extra == 'ui'"
    );
    assert_eq!(
        declarations[2].syntax().raw_for_semantic_resolution(),
        "group-only; extra == 'ui'"
    );
    assert!(!format!("{owner:?} {selected:?}").contains("PRIVATE_SENTINEL"));
}

#[test]
fn dynamic_selection_keeps_the_real_owner_and_static_prefix_without_worker_authority() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    fs::write(
        &path,
        format!(
            "{BASIC}dynamic=['dependencies','optional-dependencies']\ndependencies=['known>=1']\n"
        ),
    )
    .unwrap();
    let owner = open(&path);
    let selected = owner
        .select(selection(&["future-extra"], &[]), Default::default())
        .unwrap();
    assert_eq!(
        selected.declarations().metadata_need().unwrap().reason(),
        PythonMetadataWorkerReason::DynamicMetadata
    );
    assert_eq!(
        selected.declarations().known().requirements()[0]
            .syntax()
            .raw_for_semantic_resolution(),
        "known>=1"
    );
    selected.recheck_for(&owner).unwrap();
}

#[test]
fn identical_bytes_and_the_same_path_do_not_allow_selection_owner_substitution() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    fs::write(&path, BASIC).unwrap();
    let original = open(&path);
    let independently_opened = open(&path);
    let selected = original
        .select(selection(&[], &[]), Default::default())
        .unwrap();
    selected.recheck_for(&original).unwrap();
    assert_eq!(
        selected.recheck_for(&independently_opened),
        Err(PythonProjectFileError::WrongOwner)
    );
}

#[test]
fn modified_file_cannot_produce_or_revalidate_a_selection_but_old_data_stays_readonly() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private-project.toml");
    fs::write(&path, format!("{BASIC}dependencies=['old-dependency']\n")).unwrap();
    let owner = open(&path);
    let selected = owner
        .select(selection(&[], &[]), Default::default())
        .unwrap();
    fs::write(&path, format!("{BASIC}dependencies=['new-dependency']\n")).unwrap();
    assert!(matches!(
        selected.recheck_for(&owner),
        Err(PythonProjectFileError::Observation(
            ReadOnlyFileError::Changed
        ))
    ));
    assert!(matches!(
        owner.select(selection(&[], &[]), Default::default()),
        Err(PythonProjectFileError::Observation(
            ReadOnlyFileError::Changed
        ))
    ));
    assert_eq!(
        selected.declarations().known().requirements()[0]
            .syntax()
            .raw_for_semantic_resolution(),
        "old-dependency"
    );
}
