//! Typed admission reads real persistent journals without creating guards.
use super::*;
use std::os::unix::fs::MetadataExt;

type SnapshotRow = (PathBuf, u64, u64, u32, i64, i64, Vec<u8>);

fn snapshot(root: &Path) -> Vec<SnapshotRow> {
    fn visit(root: &Path, path: &Path, rows: &mut Vec<SnapshotRow>) {
        let info = fs::symlink_metadata(path).unwrap();
        let bytes = if info.is_file() {
            fs::read(path).unwrap()
        } else if info.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        rows.push((
            path.strip_prefix(root).unwrap().to_owned(),
            info.dev(),
            info.ino(),
            info.mode(),
            info.mtime(),
            info.mtime_nsec(),
            bytes,
        ));
        if info.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), rows);
            }
        }
    }
    let mut rows = Vec::new();
    visit(root, root, &mut rows);
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

#[test]
fn p3_own_pending_admission_is_readonly_before_and_after_catalog_commit_then_recovers() {
    for committed in [false, true] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let project = make_project(&root, "project", true);
        let other = make_project(&root, "other", false);
        let context = context(&root, false);
        let before = snapshot(&root);
        assert!(!own_pending_lock_commit(&context, &observe(&project)).unwrap());
        assert_eq!(
            snapshot(&root),
            before,
            "absent store must not create control state"
        );
        let transaction = Uuid::now_v7();
        let pending = apply(&context, &project, transaction);
        if committed {
            commit_catalog(&context, &project, transaction);
        }
        drop(pending);
        let observed = observe(&project);
        let before = snapshot(&root);
        assert!(own_pending_lock_commit(&context, &observed).unwrap());
        assert!(!own_pending_lock_commit(&context, &observe(&other)).unwrap());
        assert!(
            ensure_project_consistent(&context, &observed).is_err(),
            "preview remains a readonly rejection"
        );
        assert_eq!(
            snapshot(&root),
            before,
            "admission must preserve documents, journal and control identities"
        );
        let fresh = recover_project_lock_and_observe(&context, &observed)
            .unwrap()
            .unwrap();
        assert!(!own_pending_lock_commit(&context, &fresh).unwrap());
        ensure_project_consistent(&context, &fresh).unwrap();
        assert_eq!(
            fs::read(project.join("pakro.lock")).unwrap(),
            if committed { NEW } else { OLD }
        );
        assert!(
            recover_project_lock_and_observe(&context, &fresh)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn p3_own_pending_admission_rejects_damaged_identity_path_and_staging_facts_without_writes() {
    for fault in [
        "json",
        "schema",
        "project",
        "path",
        "parent",
        "stage",
        "reference",
    ] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let project = make_project(&root, "project", true);
        let other = make_project(&root, "other", true);
        let context = context(&root, false);
        drop(apply(&context, &project, Uuid::now_v7()));
        let observed = observe(&project);
        let identity = ProjectIdentity::from_observation(&observed).unwrap();
        let repository = ProjectRepository::open_writer(&context).unwrap();
        let writer = repository.writer().unwrap();
        let mut control = writer.control(ControlLimits::default()).unwrap();
        let original = control
            .read(ControlNamespace::Journals, &name(identity.as_uuid()))
            .unwrap()
            .unwrap();
        let mut journal = decode(&original, Some(identity.as_uuid())).unwrap();
        match fault {
            "schema" => journal.schema = 2,
            "project" => journal.project = Uuid::now_v7(),
            "path" => {
                journal.path = other.join("pakro.lock");
                journal.parent = parent_identity(&journal.path).unwrap();
            }
            "parent" => journal.parent.1 ^= 1,
            "stage" => journal.staged.as_mut().unwrap().0 = other.join(".pakro.lock.txn-foreign"),
            "reference" => journal.references.push("sha256:invalid".into()),
            _ => (),
        }
        let bytes = if fault == "json" {
            b"invalid journal JSON".to_vec()
        } else {
            serde_json::to_vec(&journal).unwrap()
        };
        control
            .replace(
                ControlNamespace::Journals,
                &name(identity.as_uuid()),
                Some(&original),
                &bytes,
            )
            .unwrap();
        drop(control);
        drop(writer);
        drop(repository);
        let before = snapshot(&root);
        assert!(
            own_pending_lock_commit(&context, &observed).is_err(),
            "accepted damaged journal field {fault}"
        );
        assert_eq!(
            snapshot(&root),
            before,
            "damaged {fault} must remain untouched"
        );
    }
}

#[test]
fn p3_another_pending_owner_cannot_be_admitted_through_a_shared_lock_or_symlink() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let alias = make_project(&root, "alias", false);
    let nested = project.join("nested");
    fs::create_dir(&nested).unwrap();
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    std::os::unix::fs::symlink(project.join("pakro.lock"), alias.join("pakro.lock")).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.clone()];
    let nested = observe_project(
        &nested,
        ReadPolicy::new(discovery, context.policy().deadline),
    )
    .unwrap();
    let before = snapshot(&root);
    assert!(own_pending_lock_commit(&context, &nested).is_err());
    drop(nested);
    assert!(own_pending_lock_commit(&context, &observe(&alias)).is_err());
    assert_eq!(snapshot(&root), before);
    // A valid own record must not hide a second project's claim on its lock.
    let foreign = ProjectIdentity::from_observation(&observe(&alias)).unwrap();
    let observed = observe(&project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_writer(&context).unwrap();
    let writer = repository.writer().unwrap();
    let mut control = writer.control(ControlLimits::default()).unwrap();
    let original = control
        .read(ControlNamespace::Journals, &name(identity.as_uuid()))
        .unwrap()
        .unwrap();
    let mut journal = decode(&original, Some(identity.as_uuid())).unwrap();
    journal.project = foreign.as_uuid();
    control
        .replace(
            ControlNamespace::Journals,
            &name(foreign.as_uuid()),
            None,
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
    drop(control);
    drop(writer);
    drop(repository);
    let before = snapshot(&root);
    assert!(own_pending_lock_commit(&context, &observed).is_err());
    assert_eq!(snapshot(&root), before);
}

#[test]
fn p3_own_pending_admission_rechecks_original_observation_and_ignores_retired_pyproject() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let project = make_project(&root, "project", true);
    let context = context(&root, false);
    drop(apply(&context, &project, Uuid::now_v7()));
    let observed = observe(&project);
    fs::write(
        project.join("pyproject.toml"),
        "invalid and outside Pakro input scope\n",
    )
    .unwrap();
    assert!(own_pending_lock_commit(&context, &observed).unwrap());
    let fresh = recover_project_lock_and_observe(&context, &observed)
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), OLD);
    assert_eq!(
        fs::read_to_string(project.join("pyproject.toml")).unwrap(),
        "invalid and outside Pakro input scope\n"
    );
    drop(apply(&context, &project, Uuid::now_v7()));
    let observed = observe(&project);
    fs::write(project.join("pakro.toml"), "[tools]\npython='3.14.0'\n").unwrap();
    let before = snapshot(&root);
    assert!(own_pending_lock_commit(&context, &observed).is_err());
    assert_eq!(snapshot(&root), before);
    fresh.recheck().unwrap_err();
}
