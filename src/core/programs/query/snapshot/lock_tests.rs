use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_resolution::lock_model::PythonLockModel;
use std::time::{Duration, Instant};

fn observe(root: &Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    observe_project(
        root,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30)),
    )
    .unwrap()
}

#[test]
fn p2_query_validates_declaration_commitment_without_creating_a_guard() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let observed = observe(&root);
    let fingerprint =
        crate::programs::install::lock_document::declaration_fingerprint(&observed).unwrap();
    let model = PythonLockModel::from_parts(&[], &[], Default::default())
        .unwrap()
        .with_declaration_fingerprint(fingerprint);
    let document = format!(
        "lockfile_version=4\n{}",
        model.encode_projection(Default::default()).unwrap()
    );
    std::fs::write(root.join("pakro.lock"), &document).unwrap();
    let observed = observe(&root);
    assert!(project_lock_for_query(&observed, None).unwrap().is_some());
    assert!(!root.join(".pakro.lock.guard").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
        document
    );
    std::fs::write(
        root.join("pakro.toml"),
        "[tools]\npython='3.15.0'\n# changed declaration bytes\n",
    )
    .unwrap();
    let changed = observe(&root);
    assert!(project_lock_for_query(&changed, None).is_err());
    assert!(
        project_lock_for_query(&changed, Some("3.14.0"))
            .unwrap()
            .is_none()
    );
    assert!(!root.join(".pakro.lock.guard").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
        document
    );
}
