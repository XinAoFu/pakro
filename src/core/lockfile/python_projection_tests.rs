use super::*;

fn image(version: u32, value: &str) -> String {
    format!(
        "lockfile_version = {version}\n\n[[tools.python]]\nversion = '3.11.17'\nbackend = 'core:python'\n\n[python]\nformat = 1\nmarker_semantics = 'current-2026'\nopaque = '{value}'\ngroups = []\n"
    )
}
fn fixture() -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let body = image(4, "preserved-native-python-state");
    fs::write(&path, &body).unwrap();
    (root, path, body)
}
fn python(body: &str) -> toml::Value {
    body.parse::<toml::Table>()
        .unwrap()
        .remove("python")
        .unwrap()
}

#[test]
fn native_lock_reads_and_writes_keep_the_complete_python_projection() {
    let (_root, path, body) = fixture();
    let lock = Lockfile::read(&path).unwrap();
    assert_eq!(lock.python_projection(), python(&body).as_table());
    lock.write(&path).unwrap();
    assert_eq!(python(&fs::read_to_string(&path).unwrap()), python(&body));
}

#[test]
fn a_native_tool_change_keeps_the_unrelated_python_projection() {
    let (_root, path, body) = fixture();
    let mut lock = Lockfile::read(&path).unwrap();
    lock.tools.insert(
        "dummy".into(),
        vec![LockfileTool {
            version: "1.0".into(),
            backend: Some("core:dummy".into()),
            specifiers: BTreeSet::new(),
            options: BTreeMap::new(),
            platforms: BTreeMap::new(),
        }],
    );
    lock.write(&path).unwrap();
    let actual = fs::read_to_string(&path).unwrap();
    assert_eq!(python(&actual), python(&body));
    assert!(actual.contains("1.0"));
}

#[tokio::test]
async fn regenerating_native_entries_keeps_the_python_projection() {
    crate::backend::load_tools().await.unwrap();
    let (_root, path, body) = fixture();
    let previous = Lockfile::read(&path).unwrap();
    let next = generate::generate(&previous, &[], &[], false, false, 1, &[])
        .await
        .unwrap();
    next.write(&path).unwrap();
    assert_eq!(python(&fs::read_to_string(&path).unwrap()), python(&body));
}

#[test]
fn scalar_python_sections_are_rejected_without_echoing_the_value() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(
        &path,
        "lockfile_version = 4\npython = 'private-test-marker'\n",
    )
    .unwrap();
    let error = Lockfile::read(&path).unwrap_err();
    assert!(error.to_string().contains("Python lock section"));
    assert!(!format!("{error:#}").contains("private-test-marker"));
}

#[test]
fn python_sections_require_revision_four() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, image(3, "legacy-body")).unwrap();
    let error = Lockfile::read(&path).unwrap_err();
    assert!(error.to_string().contains("requires lockfile revision 4"));
}

#[test]
fn ordinary_revision_three_native_files_keep_their_revision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(
        &path,
        "lockfile_version = 3\n[[tools.python]]\nversion = '3.11.17'\n",
    )
    .unwrap();
    let lock = Lockfile::read(&path).unwrap();
    assert_eq!(lock.lockfile_version(), 3);
    assert!(lock.python_projection().is_none());
    lock.write(&path).unwrap();
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("lockfile_version = 3")
    );
}

#[test]
fn native_lock_debug_does_not_print_python_declarations() {
    let (_root, path, _) = fixture();
    let lock = Lockfile::read(&path).unwrap();
    assert!(!format!("{lock:?}").contains("preserved-native-python-state"));
}

#[test]
fn prepared_native_entries_keep_a_python_update_committed_after_preparation() {
    let (_root, path, original) = fixture();
    let mut native = Lockfile::read(&path).unwrap();
    native.tools.clear();
    let prepared = native.prepare_write(&path).unwrap().unwrap();
    let changed = image(4, "newer-python-resolution");
    fs::write(&path, &changed).unwrap();
    prepared.publish_deferred().unwrap();
    let actual = fs::read_to_string(&path).unwrap();
    assert_ne!(python(&changed), python(&original));
    assert_eq!(python(&actual), python(&changed));
    assert!(Lockfile::read(&path).unwrap().tools.is_empty());
}

#[test]
fn a_python_section_cleared_after_native_preparation_is_not_resurrected() {
    let (_root, path, _) = fixture();
    let mut native = Lockfile::read(&path).unwrap();
    native.tools.clear();
    let prepared = native.prepare_write(&path).unwrap().unwrap();
    fs::write(&path, "lockfile_version = 4\n").unwrap();
    prepared.publish_deferred().unwrap();
    assert!(Lockfile::read(&path).unwrap().python_projection().is_none());
}

#[test]
fn native_revision_three_preparation_keeps_a_later_new_python_section_and_revision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    fs::write(&path, "lockfile_version = 3\n").unwrap();
    let native = Lockfile::read(&path).unwrap();
    let prepared = native.prepare_write(&path).unwrap().unwrap();
    let changed = image(4, "first-python-resolution");
    fs::write(&path, &changed).unwrap();
    prepared.publish_deferred().unwrap();
    let actual = Lockfile::read(&path).unwrap();
    assert_eq!(actual.lockfile_version(), 4);
    assert_eq!(actual.python_projection(), python(&changed).as_table());
}

#[test]
fn native_publish_rejects_a_new_invalid_python_section_without_overwriting_it() {
    let (_root, path, _) = fixture();
    let mut native = Lockfile::read(&path).unwrap();
    native.tools.clear();
    let prepared = native.prepare_write(&path).unwrap().unwrap();
    let invalid = "lockfile_version = 4\npython = 'private-concurrent-declaration'\n";
    fs::write(&path, invalid).unwrap();
    let error = prepared.publish_deferred().unwrap_err();
    assert!(!format!("{error:#}").contains("private-concurrent-declaration"));
    assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
}

#[cfg(unix)]
#[test]
fn native_publication_rechecks_the_original_symlink_inside_the_commit_session() {
    use std::os::unix::fs::symlink;
    let mut accepted = Vec::new();
    for case in ["other-target", "same-target-new-link", "alias-parent"] {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("target");
        let aliases = root.path().join("aliases");
        fs::create_dir(&target_dir).unwrap();
        fs::create_dir(&aliases).unwrap();
        let target = target_dir.join("pakro.lock");
        let other = target_dir.join("other.lock");
        let body = image(4, "current-state");
        fs::write(&target, &body).unwrap();
        fs::write(&other, "lockfile_version=3\n").unwrap();
        let alias = aliases.join("pakro.lock");
        symlink(&target, &alias).unwrap();
        let mut native = Lockfile::read(&alias).unwrap();
        native.tools.clear();
        let prepared = native.prepare_write(&alias).unwrap().unwrap();
        let mut reached = false;
        let result = prepared.publish_with_hook(|| {
            reached = true;
            if case == "alias-parent" {
                fs::rename(&aliases, root.path().join("old-aliases")).unwrap();
                fs::create_dir(&aliases).unwrap();
                symlink(&target, &alias).unwrap();
            } else {
                fs::remove_file(&alias).unwrap();
                symlink(
                    if case == "other-target" {
                        &other
                    } else {
                        &target
                    },
                    &alias,
                )
                .unwrap();
            }
        });
        assert!(reached);
        if result.is_ok() {
            accepted.push(case);
        }
        if result.is_err() {
            assert_eq!(fs::read_to_string(&target).unwrap(), body);
        }
        assert_eq!(fs::read_to_string(&other).unwrap(), "lockfile_version=3\n");
    }
    assert!(
        accepted.is_empty(),
        "accepted replaced original aliases: {accepted:?}"
    );
}

#[cfg(unix)]
#[test]
fn native_dangling_lockfile_publication_replaces_only_the_original_link() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let missing = root.path().join("missing-target.lock");
    symlink(&missing, &path).unwrap();
    Lockfile::default().write(&path).unwrap();
    assert!(!path.is_symlink());
    assert!(!missing.exists());
    assert_eq!(Lockfile::read(&path).unwrap().lockfile_version(), 4);
}

#[cfg(unix)]
#[test]
fn native_dangling_preparation_cannot_overwrite_a_later_regular_lockfile() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pakro.lock");
    let missing = root.path().join("missing-target.lock");
    symlink(&missing, &path).unwrap();
    let prepared = Lockfile::default().prepare_write(&path).unwrap().unwrap();
    fs::remove_file(&path).unwrap();
    let current = image(4, "newer-unrelated-project-state");
    fs::write(&path, &current).unwrap();
    assert!(prepared.publish_deferred().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), current);
    assert!(!missing.exists());
}
